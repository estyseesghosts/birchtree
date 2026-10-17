//! Timeline use cases (Phase 4): home, public, hashtag and list feeds.
//!
//! Every feed is a bounded fan-out-on-read scan (`birchtree.md` §2.6). The
//! store walks an index and applies a [`TimelineFilter`] while it fills the
//! page; the viewer policy for that filter lives here so the REST handlers
//! stay thin. Account facts come from one `relationships` batch per account
//! and are cached for the duration of one request.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::application::lists;
use crate::application::policy::is_mentioned;
use crate::application::ports::{
    PairState, Relationship, Store, TimelineFilter, TimelinePage, TimelineQuery,
};
use crate::application::statuses::normalize_tag;
use crate::domain::{Error, Id, RepliesPolicy, Result, Status, Visibility};

/// Mastodon `limit_param` default and ceiling for every timeline.
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 40;

/// Most recent active follows a home feed reads. Follows beyond this are not
/// considered; the manifest records the bound as a deviation (D10a).
const HOME_AUTHOR_CAP: usize = 1000;
/// Members a list feed reads.
const LIST_MEMBER_CAP: usize = 500;
/// Distinct accounts (and reblog targets) one request may resolve. Past the
/// cap an unresolved account fails closed: its statuses are excluded, never
/// shown. This keeps public scans bounded without leaking. Sized above
/// `HOME_AUTHOR_CAP`: the home follows fill the cache first, and reblog
/// targets and reply parents from other accounts need room after them.
const ACCOUNT_CACHE_CAP: usize = 2000;

/// Viewer-side facts about one account, from a single `relationships` row.
#[derive(Debug, Clone, Copy)]
struct Facts {
    /// Block, follow and mute state between the viewer and the account.
    pair: PairState,
    /// The viewer's follow row asks for this account's reblogs.
    show_reblogs: bool,
    /// The viewer blocks the account's domain.
    domain_blocked: bool,
}

/// Request-scoped viewer policy. Facts are loaded lazily and cached per
/// account. A store error excludes the status being judged and is recorded;
/// [`ViewerScope::finish`] returns it after the scan so the request fails
/// instead of quietly returning a partial page.
struct ViewerScope<'a> {
    store: &'a dyn Store,
    viewer: Option<Id>,
    now_ms: u64,
    facts: Mutex<HashMap<Id, Option<Facts>>>,
    reblog_authors: Mutex<HashMap<Id, Option<Id>>>,
    /// Accounts on one of the viewer's exclusive lists, among the home
    /// authors (loaded by [`ViewerScope::seed_exclusive`]).
    exclusive: Mutex<HashSet<Id>>,
    error: Mutex<Option<Error>>,
}

impl<'a> ViewerScope<'a> {
    fn new(store: &'a dyn Store, viewer: Option<Id>, now_ms: u64) -> Self {
        Self {
            store,
            viewer,
            now_ms,
            facts: Mutex::new(HashMap::new()),
            reblog_authors: Mutex::new(HashMap::new()),
            exclusive: Mutex::new(HashSet::new()),
            error: Mutex::new(None),
        }
    }

    /// Load which of `ids` sit on one of the viewer's exclusive lists. Home
    /// only: list timelines never consult this (upstream's `timeline_type !=
    /// :list` guard).
    fn seed_exclusive(&self, ids: &[Id]) -> Result<()> {
        let Some(viewer) = self.viewer else {
            return Ok(());
        };
        let found = self.store.exclusive_list_members(viewer, ids)?;
        *lock(&self.exclusive) = found;
        Ok(())
    }

    /// Upstream's `FeedManager#filter_from_home` `:skip_home` rule: an account
    /// on one of the viewer's exclusive lists is kept off the home feed. The
    /// viewer's own statuses are never checked here (`HomeFilter` returns
    /// first), and list timelines do not apply the rule.
    fn on_exclusive_list(&self, account: Id) -> bool {
        lock(&self.exclusive).contains(&account)
    }

    /// Load facts for a known author set in two store calls.
    fn seed(&self, ids: &[Id]) -> Result<()> {
        let Some(viewer) = self.viewer else {
            return Ok(());
        };
        let others: Vec<Id> = ids.iter().copied().filter(|id| *id != viewer).collect();
        let accounts = self.store.accounts_by_ids(&others)?;
        let targets: Vec<(Id, String)> = others
            .iter()
            .filter_map(|id| accounts.get(id).map(|account| (*id, account.domain.clone())))
            .collect();
        let rows = self.store.relationships(viewer, &targets, self.now_ms)?;
        let by_id: HashMap<Id, Facts> = rows.iter().map(|row| (row.target_id, facts_of(row))).collect();
        let mut cache = lock(&self.facts);
        for id in others {
            cache.insert(id, by_id.get(&id).copied());
        }
        Ok(())
    }

    /// Facts for one account: cached, or loaded with two lookups. Past the
    /// cache cap, or on a store error, the account is unknown (fail closed).
    fn facts(&self, id: Id) -> Option<Facts> {
        let viewer = self.viewer?;
        if let Some(known) = lock(&self.facts).get(&id).copied() {
            return known;
        }
        if lock(&self.facts).len() >= ACCOUNT_CACHE_CAP {
            return None;
        }
        let loaded = self.load(viewer, id).unwrap_or_else(|err| {
            self.fail(err);
            None
        });
        lock(&self.facts).insert(id, loaded);
        loaded
    }

    fn load(&self, viewer: Id, id: Id) -> Result<Option<Facts>> {
        let Some(account) = self.store.account_by_id(id)? else {
            return Ok(None);
        };
        let rows = self
            .store
            .relationships(viewer, &[(id, account.domain)], self.now_ms)?;
        Ok(rows.first().map(facts_of))
    }

    /// Whether `author`'s statuses may appear at all: no block either way, no
    /// mute, and the author's domain not blocked. Anonymous viewers see all.
    fn author_allowed(&self, author: Id) -> bool {
        if self.viewer.is_none() || self.viewer == Some(author) {
            return true;
        }
        self.facts(author).is_some_and(|f| {
            !(f.pair.blocking || f.pair.blocked_by || f.pair.muting || f.domain_blocked)
        })
    }

    /// The viewer holds an active follow of `account`.
    fn follows(&self, account: Id) -> bool {
        self.facts(account).is_some_and(|f| f.pair.following)
    }

    /// The viewer's follow of `account` asks for its reblogs.
    fn shows_reblogs_of(&self, account: Id) -> bool {
        self.facts(account).is_some_and(|f| f.show_reblogs)
    }

    /// A reblog wrapper is shown only when the reblogged author passes the
    /// same gate as any other author.
    fn reblog_target_allowed(&self, status: &Status) -> bool {
        let Some(target) = status.reblog_of_id else {
            return true;
        };
        match self.reblog_author(target) {
            Some(author) => self.author_allowed(author),
            None => false,
        }
    }

    /// Author of a reblog target (`None` when it is missing or deleted).
    fn reblog_author(&self, target: Id) -> Option<Id> {
        if let Some(known) = lock(&self.reblog_authors).get(&target).copied() {
            return known;
        }
        if lock(&self.reblog_authors).len() >= ACCOUNT_CACHE_CAP {
            return None;
        }
        let loaded = match self.store.status_by_id(target) {
            Ok(Some(status)) if !status.is_deleted() => Some(status.account_id),
            Ok(_) => None,
            Err(err) => {
                self.fail(err);
                None
            }
        };
        lock(&self.reblog_authors).insert(target, loaded);
        loaded
    }

    fn fail(&self, err: Error) {
        let mut slot = lock(&self.error);
        if slot.is_none() {
            *slot = Some(err);
        }
    }

    /// Surface the first store error hit while the scan ran.
    fn finish(&self) -> Result<()> {
        match lock(&self.error).take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn facts_of(row: &Relationship) -> Facts {
    Facts {
        pair: PairState {
            blocking: row.blocking,
            blocked_by: row.blocked_by,
            following: row.following,
            followed_by: row.followed_by,
            muting: row.muting,
        },
        show_reblogs: row.show_reblogs,
        domain_blocked: row.domain_blocking,
    }
}

/// A reply whose parent status is set but whose parent account is unknown.
/// Such a status can never be rendered, so no feed shows it.
fn is_orphan_reply(status: &Status) -> bool {
    status.in_reply_to_id.is_some() && status.in_reply_to_account_id.is_none()
}

/// The account a reply is directed at, when it is not the author's own thread.
fn foreign_reply_parent(status: &Status) -> Option<Id> {
    status
        .in_reply_to_account_id
        .filter(|parent| *parent != status.account_id)
}

/// Home feed policy: own statuses, plus active follows that pass the gates.
struct HomeFilter<'a> {
    scope: ViewerScope<'a>,
    viewer: Id,
}

impl TimelineFilter for HomeFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        let author = status.account_id;
        // The viewer always sees their own statuses in their own home feed.
        if author == self.viewer {
            return true;
        }
        if self.scope.on_exclusive_list(author) {
            return false;
        }
        if !self.scope.follows(author) || !self.scope.author_allowed(author) {
            return false;
        }
        // Private statuses from a followed author are visible; direct ones
        // only to the people they name.
        if status.visibility == Visibility::Direct && !is_mentioned(Some(self.viewer), status) {
            return false;
        }
        if is_orphan_reply(status) {
            return false;
        }
        if let Some(parent) = foreign_reply_parent(status) {
            if parent != self.viewer && !self.scope.follows(parent) {
                return false;
            }
        }
        if status.is_reblog()
            && (!self.scope.shows_reblogs_of(author) || !self.scope.reblog_target_allowed(status))
        {
            return false;
        }
        true
    }
}

/// Public feed policy: public statuses, self-replies only, no reblogs (the
/// store already drops reblogs for this feed).
struct PublicFilter<'a> {
    scope: ViewerScope<'a>,
}

impl TimelineFilter for PublicFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        let self_reply_or_plain = status.in_reply_to_id.is_none()
            || status.in_reply_to_account_id == Some(status.account_id);
        status.visibility == Visibility::Public
            && self_reply_or_plain
            && self.scope.author_allowed(status.account_id)
    }
}

/// Hashtag feed policy: the public feed rules, but replies and reblogs stay.
struct TagFilter<'a> {
    scope: ViewerScope<'a>,
}

impl TimelineFilter for TagFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        status.visibility == Visibility::Public
            && self.scope.author_allowed(status.account_id)
            && (!status.is_reblog() || self.scope.reblog_target_allowed(status))
    }
}

/// List feed policy: members' statuses under the home gates, with the list's
/// own replies rule instead of the home one.
struct ListFilter<'a> {
    scope: ViewerScope<'a>,
    viewer: Id,
    members: HashSet<Id>,
    policy: RepliesPolicy,
}

impl TimelineFilter for ListFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        let author = status.account_id;
        if author == self.viewer {
            return true;
        }
        if !self.members.contains(&author) || !self.scope.author_allowed(author) {
            return false;
        }
        if status.visibility == Visibility::Direct && !is_mentioned(Some(self.viewer), status) {
            return false;
        }
        if is_orphan_reply(status) {
            return false;
        }
        if let Some(parent) = foreign_reply_parent(status) {
            let kept = parent == self.viewer
                || match self.policy {
                    RepliesPolicy::Followed => self.scope.follows(parent),
                    RepliesPolicy::List => self.members.contains(&parent),
                    RepliesPolicy::None => false,
                };
            if !kept {
                return false;
            }
        }
        if status.is_reblog()
            && (!self.scope.shows_reblogs_of(author) || !self.scope.reblog_target_allowed(status))
        {
            return false;
        }
        true
    }
}

/// Home timeline: the viewer's own statuses and their active follows, newest
/// first. Only the most recent [`HOME_AUTHOR_CAP`] follows are read. Members
/// of the viewer's exclusive lists are left out (upstream `:skip_home`).
pub fn home(
    store: &dyn Store,
    viewer: Id,
    query: TimelineQuery,
    now_ms: u64,
) -> Result<TimelinePage> {
    let follows = store.following(viewer, None, HOME_AUTHOR_CAP)?;
    let scope = ViewerScope::new(store, Some(viewer), now_ms);
    scope.seed(&follows)?;
    scope.seed_exclusive(&follows)?;
    let mut authors = Vec::with_capacity(follows.len() + 1);
    authors.push(viewer);
    authors.extend_from_slice(&follows);
    let filter = HomeFilter { scope, viewer };
    let page = store.authors_timeline(&authors, query, &filter)?;
    filter.scope.finish()?;
    Ok(page)
}

/// Public timeline: public statuses, self-replies kept, replies to others
/// dropped, reblogs excluded. `local`/`remote`/`only_media` ride in `query`.
pub fn public(
    store: &dyn Store,
    viewer: Option<Id>,
    query: TimelineQuery,
    now_ms: u64,
) -> Result<TimelinePage> {
    let filter = PublicFilter {
        scope: ViewerScope::new(store, viewer, now_ms),
    };
    // Replies stay in the scan so the filter can keep self-replies only; the
    // store would drop all of them otherwise.
    let page = store.public_timeline(query, true, false, &filter)?;
    filter.scope.finish()?;
    Ok(page)
}

/// Hashtag timeline. An unknown or empty tag yields an empty page.
pub fn tag(
    store: &dyn Store,
    viewer: Option<Id>,
    raw_tag: &str,
    query: TimelineQuery,
    now_ms: u64,
) -> Result<TimelinePage> {
    let normalized = normalize_tag(raw_tag);
    if normalized.is_empty() {
        return Ok(TimelinePage::default());
    }
    let filter = TagFilter {
        scope: ViewerScope::new(store, viewer, now_ms),
    };
    let page = store.hashtag_timeline(&normalized, query, &filter)?;
    filter.scope.finish()?;
    Ok(page)
}

/// List timeline. Ownership is checked first: a list the viewer does not own
/// is `NotFound`.
pub fn list(
    store: &dyn Store,
    viewer: Id,
    list_id: Id,
    query: TimelineQuery,
    now_ms: u64,
) -> Result<TimelinePage> {
    let list = lists::show(store, viewer, list_id)?;
    let members = store.all_list_members(list_id, LIST_MEMBER_CAP)?;
    let scope = ViewerScope::new(store, Some(viewer), now_ms);
    scope.seed(&members)?;
    let filter = ListFilter {
        scope,
        viewer,
        members: members.iter().copied().collect(),
        policy: list.replies_policy,
    };
    let page = store.authors_timeline(&members, query, &filter)?;
    filter.scope.finish()?;
    Ok(page)
}

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
use crate::application::policy::{self, HomeMentionPolicy, is_mentioned};
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
    /// D22: the viewer's home mention policy, resolved lazily from the
    /// stored user preference (`Show` when unset).
    mention_policy: Mutex<Option<HomeMentionPolicy>>,
    /// D23: the viewer's follow-languages preference per author, resolved
    /// lazily from `follow_of` (`None` = no row or no preference).
    follow_langs: Mutex<HashMap<Id, Option<Vec<String>>>>,
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
            mention_policy: Mutex::new(None),
            follow_langs: Mutex::new(HashMap::new()),
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

    /// Whether `status` is a reply whose remote parent has not been resolved
    /// yet (contract A): it is withheld from home and list feeds until its
    /// parent is linked.
    fn unresolved_reply(&self, status: &Status) -> bool {
        match self.store.unresolved_parent(status.id) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(err) => {
                self.fail(err);
                false
            }
        }
    }

    /// The viewer holds an active follow of `account`.
    fn follows(&self, account: Id) -> bool {
        self.facts(account).is_some_and(|f| f.pair.following)
    }

    /// The viewer's follow of `account` asks for its reblogs.
    fn shows_reblogs_of(&self, account: Id) -> bool {
        self.facts(account).is_some_and(|f| f.show_reblogs)
    }

    /// D22: the viewer's stored home mention policy, loaded once per scope
    /// (`Show` when the viewer is anonymous or has no preference stored).
    fn mention_policy(&self) -> HomeMentionPolicy {
        if let Some(policy) = *lock(&self.mention_policy) {
            return policy;
        }
        let policy = self
            .viewer
            .map_or(HomeMentionPolicy::Show, |v| home_mention_policy_of(self.store, v));
        *lock(&self.mention_policy) = Some(policy);
        policy
    }

    /// D23: upstream `filter_from_home`'s `crutches[:languages]` rule — the
    /// viewer's follow of `author` may restrict which status languages appear
    /// on home. Own statuses and language-less statuses always pass; so does
    /// anything when the follow names no languages.
    fn follow_language_allows(&self, author: Id, language: Option<&str>) -> bool {
        let Some(viewer) = self.viewer else {
            return true;
        };
        if viewer == author || language.is_none() {
            return true;
        }
        if let Some(cached) = lock(&self.follow_langs).get(&author).cloned() {
            return policy::follow_language_allows(cached.as_deref(), language);
        }
        let loaded = self
            .store
            .follow_of(viewer, author)
            .map(|row| row.and_then(|follow| follow.languages))
            .unwrap_or_else(|err| {
                self.fail(err);
                None
            });
        lock(&self.follow_langs).insert(author, loaded.clone());
        policy::follow_language_allows(loaded.as_deref(), language)
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
        if author != self.viewer && self.scope.unresolved_reply(status) {
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
        // D23: the viewer's follow of the author may restrict languages.
        if !self
            .scope
            .follow_language_allows(author, status.language.as_deref())
        {
            return false;
        }
        // D22: in `Filter` mode, statuses mentioning a blocked/muted account
        // leave home (upstream behavior, opt-in here). `Show` (default) and
        // `Collapse` (DTO fold) keep them.
        if self.scope.mention_policy() == HomeMentionPolicy::Filter
            && home_mentions_filtered(&self.scope, status)
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
        if author != self.viewer && self.scope.unresolved_reply(status) {
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
/// D23 per-follow language preferences and the D22 `Filter` mention policy
/// apply here as well (see [`HomeMentionPolicy`]); the home stream gate
/// ([`home_includes`]) shares this filter.
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

// ---------------------------------------------------------------------------
// Streaming gates (Phase 5). The same `ViewerScope` facts and filters as the
// REST timelines, applied to one status for one subscriber. Streaming adds no
// policy of its own (`birchtree.md` §4.3).
// ---------------------------------------------------------------------------

/// Upstream's streaming-side block and mute check on a status's mentions (the
/// author is covered by `author_allowed`): the viewer blocks or mutes one of
/// them. Upstream applies it to public and hashtag streams only.
fn mentions_blocked_or_muted(scope: &ViewerScope<'_>, status: &Status) -> bool {
    scope.viewer.is_some()
        && status
            .mentions
            .iter()
            .any(|mention| scope.facts(*mention).is_some_and(|f| f.pair.blocking || f.pair.muting))
}

/// D22: whether `status` mentions someone the viewer blocks or mutes — or
/// (for reblogs) the reblogged status does. Upstream's `filter_from_home`
/// `check_for_blocks` also covers the author and the reblog author, but those
/// are already gated by `author_allowed` / `reblog_target_allowed` here, so
/// only mentions remain.
fn home_mentions_filtered(scope: &ViewerScope<'_>, status: &Status) -> bool {
    if mentions_blocked_or_muted(scope, status) {
        return true;
    }
    let Some(target_id) = status.reblog_of_id else {
        return false;
    };
    match scope.store.status_by_id(target_id) {
        Ok(Some(target)) => mentions_blocked_or_muted(scope, &target),
        // An unloadable reblog target fails closed in
        // `reblog_target_allowed`; it must not filter here.
        Ok(None) | Err(_) => false,
    }
}

/// D22: the viewer's stored home mention policy (`Show` when unset).
/// Reads the `home_mention_policy` user preference; unknown values fall back
/// to `Show` (see [`HomeMentionPolicy::from_stored`]).
pub fn home_mention_policy_of(store: &dyn Store, viewer: Id) -> HomeMentionPolicy {
    store
        .user_by_account_id(viewer)
        .ok()
        .flatten()
        .map(|user| HomeMentionPolicy::from_stored(user.home_mention_policy.as_deref()))
        .unwrap_or(HomeMentionPolicy::Show)
}

/// D22 `Collapse`-mode helper for the status DTO and stream rendering:
/// whether `status` in `viewer`'s home feed should render folded behind a
/// content-warning-style fold. True only when the viewer chose `Collapse` and
/// the status mentions a blocked/muted account.
pub fn home_mention_fold(
    store: &dyn Store,
    viewer: Id,
    status: &Status,
    now_ms: u64,
) -> Result<bool> {
    let scope = ViewerScope::new(store, Some(viewer), now_ms);
    let fold = scope.mention_policy() == HomeMentionPolicy::Collapse
        && home_mentions_filtered(&scope, status);
    scope.finish()?;
    Ok(fold)
}

/// Home stream gate: whether `viewer`'s home feed would carry `status`.
pub fn home_includes(store: &dyn Store, viewer: Id, status: &Status, now_ms: u64) -> Result<bool> {
    let scope = ViewerScope::new(store, Some(viewer), now_ms);
    scope.seed_exclusive(&[status.account_id])?;
    let filter = HomeFilter { scope, viewer };
    let kept = filter.include(status);
    filter.scope.finish()?;
    Ok(kept)
}

/// Public stream gate (`public`, `public:local`, ...): public, not a reblog,
/// the public feed's reply rule, plus the streaming block and mute checks for
/// logged-in viewers. The caller applies the local/remote and media selectors.
pub fn public_stream_includes(
    store: &dyn Store,
    viewer: Option<Id>,
    status: &Status,
    now_ms: u64,
) -> Result<bool> {
    if status.is_reblog() {
        return Ok(false);
    }
    let filter = PublicFilter {
        scope: ViewerScope::new(store, viewer, now_ms),
    };
    let mut kept = filter.include(status);
    if kept {
        kept = !mentions_blocked_or_muted(&filter.scope, status);
    }
    filter.scope.finish()?;
    Ok(kept)
}

/// Hashtag stream gate: public, not a reblog, and the streaming block and mute
/// checks. Replies are kept, as upstream's hashtag streams keep them. The
/// caller checks that `status` carries the tag.
pub fn tag_stream_includes(
    store: &dyn Store,
    viewer: Option<Id>,
    status: &Status,
    now_ms: u64,
) -> Result<bool> {
    if status.is_reblog() || status.visibility != Visibility::Public {
        return Ok(false);
    }
    let scope = ViewerScope::new(store, viewer, now_ms);
    let kept = scope.author_allowed(status.account_id) && !mentions_blocked_or_muted(&scope, status);
    scope.finish()?;
    Ok(kept)
}

/// List stream gate: whether the list `list_id` owned by `owner` would carry
/// `status`. A list the owner does not own gates everything out.
pub fn list_stream_includes(
    store: &dyn Store,
    owner: Id,
    list_id: Id,
    status: &Status,
    now_ms: u64,
) -> Result<bool> {
    let Some(list) = store.list_by_id(list_id)? else {
        return Ok(false);
    };
    if list.account_id != owner {
        return Ok(false);
    }
    let members = store.all_list_members(list_id, LIST_MEMBER_CAP)?;
    let scope = ViewerScope::new(store, Some(owner), now_ms);
    scope.seed(&members)?;
    let filter = ListFilter {
        scope,
        viewer: owner,
        members: members.iter().copied().collect(),
        policy: list.replies_policy,
    };
    let kept = filter.include(status);
    filter.scope.finish()?;
    Ok(kept)
}

/// Cap on the statuses one retraction sends (block or mute).
const RETRACT_LIMIT: usize = 200;

/// Statuses a block or mute of `target` removes from `viewer`'s open streams.
///
/// Upstream's `FeedManager#clear_from_home` and `clear_from_lists` rule: the
/// statuses by `target`, reblogs by `target`, reblogs of `target`'s statuses,
/// and statuses mentioning `target`, from the viewer's home authors and list
/// members. Call this BEFORE the block or mute commits: a block unfollows, so
/// the home author set would no longer include `target`'s statuses afterwards.
pub fn retraction_candidates(store: &dyn Store, viewer: Id, target: Id) -> Result<Vec<Status>> {
    let matcher = RetractMatcher {
        store,
        target,
        reblog_authors: Mutex::new(HashMap::new()),
    };
    let query = TimelineQuery {
        limit: RETRACT_LIMIT,
        ..TimelineQuery::default()
    };
    let mut authors = store.following(viewer, None, HOME_AUTHOR_CAP)?;
    authors.push(viewer);
    let mut found = store.authors_timeline(&authors, query, &matcher)?.statuses;
    for list in store.lists_by_account(viewer)? {
        let members = store.all_list_members(list.id, LIST_MEMBER_CAP)?;
        found.extend(store.authors_timeline(&members, query, &matcher)?.statuses);
    }
    let mut seen = HashSet::new();
    found.retain(|status| seen.insert(status.id));
    Ok(found)
}

/// Matches the retraction rule. The reblog-author lookup is cached per call.
struct RetractMatcher<'a> {
    store: &'a dyn Store,
    target: Id,
    reblog_authors: Mutex<HashMap<Id, Option<Id>>>,
}

impl TimelineFilter for RetractMatcher<'_> {
    fn include(&self, status: &Status) -> bool {
        if status.account_id == self.target || status.mentions.contains(&self.target) {
            return true;
        }
        let Some(original) = status.reblog_of_id else {
            return false;
        };
        let author = {
            let mut cache = lock(&self.reblog_authors);
            match cache.get(&original).copied() {
                Some(known) => known,
                None => {
                    let loaded = self
                        .store
                        .status_by_id(original)
                        .ok()
                        .flatten()
                        .map(|s| s.account_id);
                    cache.insert(original, loaded);
                    loaded
                }
            }
        };
        author == Some(self.target)
    }
}

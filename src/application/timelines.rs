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
use crate::application::policy::{self, HomeMentionPolicy};
use crate::application::ports::{
    PairState, Relationship, Store, TimelineFilter, TimelinePage, TimelineQuery,
};
use crate::application::statuses::normalize_tag;
use crate::domain::{Error, Id, Result, Status, Visibility};

pub mod filters;

use self::filters::HomeFilter;
use self::filters::ListFilter;
use self::filters::PublicFilter;
use self::filters::TagFilter;
use self::filters::home_mentions_filtered;
use self::filters::mentions_blocked_or_muted;

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
pub(super) struct Facts {
    /// Block, follow and mute state between the viewer and the account.
    pub(super) pair: PairState,
    /// The viewer's follow row asks for this account's reblogs.
    show_reblogs: bool,
    /// The viewer blocks the account's domain.
    domain_blocked: bool,
}

/// Request-scoped viewer policy. Facts are loaded lazily and cached per
/// account. A store error excludes the status being judged and is recorded;
/// [`ViewerScope::finish`] returns it after the scan so the request fails
/// instead of quietly returning a partial page.
pub(super) struct ViewerScope<'a> {
    pub(super) store: &'a dyn Store,
    pub(super) viewer: Option<Id>,
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
    /// D21: the viewer's `chosen_languages` preference, resolved lazily
    /// from the user row (`None` = unset or empty).
    chosen_languages: Mutex<Option<Option<Vec<String>>>>,
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
            chosen_languages: Mutex::new(None),
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
    pub(super) fn on_exclusive_list(&self, account: Id) -> bool {
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
    pub(super) fn facts(&self, id: Id) -> Option<Facts> {
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
    pub(super) fn author_allowed(&self, author: Id) -> bool {
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
    pub(super) fn unresolved_reply(&self, status: &Status) -> bool {
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
    pub(super) fn follows(&self, account: Id) -> bool {
        self.facts(account).is_some_and(|f| f.pair.following)
    }

    /// The viewer's follow of `account` asks for its reblogs.
    pub(super) fn shows_reblogs_of(&self, account: Id) -> bool {
        self.facts(account).is_some_and(|f| f.show_reblogs)
    }

    /// D22: the viewer's stored home mention policy, loaded once per scope
    /// (`Show` when the viewer is anonymous or has no preference stored).
    pub(super) fn mention_policy(&self) -> HomeMentionPolicy {
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
    pub(super) fn follow_language_allows(&self, author: Id, language: Option<&str>) -> bool {
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

    /// D21: the viewer's `chosen_languages` preference, loaded once per
    /// scope (`None` when the viewer is anonymous or has none stored).
    /// Applied to the public timeline and the public + hashtag streams only,
    /// like upstream (`PublicFeed#language_scope`, `streaming/index.js`
    /// `chosenLanguages`): the REST tag timeline, home and list feeds keep
    /// no chosen-language filter, as upstream's `TagFeed`/`HomeFeed`/
    /// `ListFeed` have none.
    fn chosen_languages(&self) -> Option<Vec<String>> {
        if let Some(cached) = lock(&self.chosen_languages).clone() {
            return cached;
        }
        let loaded = self
            .viewer
            .and_then(|v| {
                self.store
                    .user_by_account_id(v)
                    .ok()
                    .flatten()
                    .and_then(|user| user.chosen_languages)
            })
            .filter(|langs| !langs.is_empty());
        *lock(&self.chosen_languages) = Some(loaded.clone());
        loaded
    }

    /// D21: whether a status in `language` passes the viewer's
    /// `chosen_languages` preference.
    pub(super) fn chosen_language_allows(&self, language: Option<&str>) -> bool {
        policy::chosen_language_allows(self.chosen_languages().as_deref(), language)
    }

    /// A reblog wrapper is shown only when the reblogged author passes the
    /// same gate as any other author.
    pub(super) fn reblog_target_allowed(&self, status: &Status) -> bool {
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
    tag_filtered(
        store,
        viewer,
        TagFilterQuery {
            raw_tag,
            any: &[],
            all: &[],
            none: &[],
            query,
            now_ms,
        },
    )
}

/// Tags per `any`/`all`/`none` mode (upstream `TagFeed::LIMIT_PER_MODE`).
pub const TAG_MODE_CAP: usize = 4;

/// Hashtag timeline with upstream's `any`/`all`/`none` modes (`TagFeed`):
/// `any` widens the base tag (a status carrying the base tag or any listed
/// tag passes), `all` narrows to statuses carrying the base tag and every
/// listed tag, `none` drops statuses carrying any listed tag. Each mode list
/// is capped at [`TAG_MODE_CAP`] extra tags.
///
/// The store reads one tag at a time, so each tag in `{base} ∪ any` is read
/// with the same cursors and the pages are merged newest-first, deduplicated
/// and cut to `limit` after the `all`/`none` test (which reads the status's
/// own normalized tags, the same values the store indexed). Merged cursor
/// pages can be shorter than `limit` when the other tags hold the rest —
/// the `Link` cursors still advance — which the single-query upstream never
/// does; the membership semantics match exactly.
/// Bundled arguments for [`tag_filtered`] (S12: 8 params → 1 struct + store).
/// Borrowed, not owned: `any`/`all`/`none` stay endpoint-owned `Vec<String>`s.
#[derive(Debug, Clone, Copy)]
pub struct TagFilterQuery<'a> {
    /// Raw (un-normalized) base tag from the path.
    pub raw_tag: &'a str,
    /// `any[]` mode list (widen; capped at [`TAG_MODE_CAP`]).
    pub any: &'a [String],
    /// `all[]` mode list (narrow; capped at [`TAG_MODE_CAP`]).
    pub all: &'a [String],
    /// `none[]` mode list (exclude; capped at [`TAG_MODE_CAP`]).
    pub none: &'a [String],
    /// Cursor-bounded page query (limit is already endpoint-bounded).
    pub query: TimelineQuery,
    /// Caller clock (`state.context.now_ms()`).
    pub now_ms: u64,
}

pub fn tag_filtered(
    store: &dyn Store,
    viewer: Option<Id>,
    filter: TagFilterQuery<'_>,
) -> Result<TimelinePage> {
    let TagFilterQuery {
        raw_tag,
        any,
        all,
        none,
        query,
        now_ms,
    } = filter;
    let normalized = normalize_tag(raw_tag);
    if normalized.is_empty() {
        return Ok(TimelinePage::default());
    }
    let mut tags = vec![normalized];
    for raw in any {
        let tag = normalize_tag(raw);
        if !tag.is_empty() && !tags.contains(&tag) && tags.len() < TAG_MODE_CAP + 1 {
            tags.push(tag);
        }
    }
    let all: Vec<String> = {
        let mut kept = Vec::new();
        for raw in all {
            let tag = normalize_tag(raw);
            if !tag.is_empty() && !kept.contains(&tag) && kept.len() < TAG_MODE_CAP {
                kept.push(tag);
            }
        }
        kept
    };
    let none: Vec<String> = {
        let mut kept = Vec::new();
        for raw in none {
            let tag = normalize_tag(raw);
            if !tag.is_empty() && !kept.contains(&tag) && kept.len() < TAG_MODE_CAP {
                kept.push(tag);
            }
        }
        kept
    };
    // Fast path: the historical single-tag read.
    if tags.len() == 1 && all.is_empty() && none.is_empty() {
        let filter = TagFilter {
            scope: ViewerScope::new(store, viewer, now_ms),
        };
        let page = store.hashtag_timeline(&tags[0], query, &filter)?;
        filter.scope.finish()?;
        return Ok(page);
    }
    let scope = ViewerScope::new(store, viewer, now_ms);
    let filter = TagFilter { scope };
    let mut merged: Vec<Status> = Vec::new();
    let mut scanned = 0usize;
    for tag in &tags {
        let page = store.hashtag_timeline(tag, query.clone(), &filter)?;
        scanned += page.scanned;
        merged.extend(page.statuses);
    }
    filter.scope.finish()?;
    merged.sort_by(|a, b| b.id.cmp(&a.id));
    merged.dedup_by_key(|status| status.id);
    merged.retain(|status| {
        all.iter().all(|tag| status.tags.iter().any(|owned| owned == tag))
            && !none.iter().any(|tag| status.tags.iter().any(|owned| owned == tag))
    });
    merged.truncate(query.limit);
    Ok(TimelinePage { statuses: merged, scanned })
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
/// caller checks that `status` carries the tag. D21: upstream's streaming
/// `chosenLanguages` filter applies here (`needsFiltering: true` on the
/// hashtag channels), even though the REST tag timeline has no language
/// scope (`TagFeed`).
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
    let kept = scope.author_allowed(status.account_id)
        && !mentions_blocked_or_muted(&scope, status)
        && scope.chosen_language_allows(status.language.as_deref());
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

/// Matches per `authors_timeline` call inside one retraction. Small enough
/// that a single call stays within the store's merge budget; calls repeat
/// with a `max_id` cursor until a short page ends the scan.
const RETRACT_BATCH: usize = 500;
/// Safety bound on retraction calls per author set (home authors, then each
/// list's members): `64 * 500 = 32_000` statuses, past any realistic open
/// stream. A hit is reported in [`RetractStats::truncated`] rather than
/// silently capping at 200 like before.
const RETRACT_MAX_BATCHES: usize = 64;

/// What one block/mute retraction walked (executed evidence for D15).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetractStats {
    /// Retractable statuses collected.
    pub retracted: usize,
    /// `authors_timeline` calls issued.
    pub batches: usize,
    /// `true` when the safety bound stopped the scan with a full page left.
    pub truncated: bool,
}

/// Statuses a block or mute of `target` removes from `viewer`'s open streams.
///
/// Upstream's `FeedManager#clear_from_home` (`app/lib/feed_manager.rb:286`)
/// and `clear_from_list` (`app/lib/feed_manager.rb:306`) rule, over every
/// feed entry held: the statuses by `target`, reblogs by `target`, reblogs of
/// `target`'s statuses, and statuses mentioning `target`, from the viewer's
/// home authors and list members. Upstream reads the whole materialized feed
/// (`zrange 0 -1`); here the scan pages through the same author sets with a
/// `max_id` cursor, one bounded call per batch, so a 10k-status backlog
/// cannot stall the request in a single unbounded read. Backpressure still
/// applies at delivery: a retraction bigger than the connection queue closes
/// the stream and the client reconnects to fresh state (contract "Bounds").
///
/// Call this BEFORE the block or mute commits: a block unfollows, so the
/// home author set would no longer include `target`'s statuses afterwards.
pub fn retraction_candidates(store: &dyn Store, viewer: Id, target: Id) -> Result<Vec<Status>> {
    Ok(retraction_candidates_with_stats(store, viewer, target)?.0)
}

/// [`retraction_candidates`] plus its [`RetractStats`].
pub fn retraction_candidates_with_stats(
    store: &dyn Store,
    viewer: Id,
    target: Id,
) -> Result<(Vec<Status>, RetractStats)> {
    let matcher = RetractMatcher {
        store,
        target,
        reblog_authors: Mutex::new(HashMap::new()),
    };
    let mut stats = RetractStats::default();
    let mut found: Vec<Status> = Vec::new();
    let mut authors = store.following(viewer, None, HOME_AUTHOR_CAP)?;
    authors.push(viewer);
    let mut truncated = false;
    truncated |= scan_retractable(&authors, &matcher, &mut found, &mut stats)?;
    for list in store.lists_by_account(viewer)? {
        let members = store.all_list_members(list.id, LIST_MEMBER_CAP)?;
        truncated |= scan_retractable(&members, &matcher, &mut found, &mut stats)?;
    }
    stats.truncated = truncated;
    let mut seen = HashSet::new();
    found.retain(|status| seen.insert(status.id));
    stats.retracted = found.len();
    Ok((found, stats))
}

/// Page one author set newest-first until a short page ends the scan or the
/// safety bound hits. Returns whether the bound truncated the scan.
fn scan_retractable(
    authors: &[Id],
    matcher: &RetractMatcher<'_>,
    found: &mut Vec<Status>,
    stats: &mut RetractStats,
) -> Result<bool> {
    let mut max_id: Option<Id> = None;
    for _ in 0..RETRACT_MAX_BATCHES {
        let query = TimelineQuery {
            limit: RETRACT_BATCH,
            max_id,
            ..TimelineQuery::default()
        };
        let page = matcher.store.authors_timeline(authors, query, matcher)?;
        stats.batches += 1;
        let full = page.statuses.len() >= RETRACT_BATCH;
        max_id = page.statuses.last().map(|status| status.id);
        found.extend(page.statuses);
        if !full {
            return Ok(false);
        }
    }
    Ok(true)
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

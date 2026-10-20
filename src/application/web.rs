//! Phase 7 web/first-party and relation-collection use cases.
//!
//! Thin validation + delegation over [`Store`]: web settings sync, web push
//! subscriptions, tag follows / featured tags, the block/mute/favourite/
//! bookmark collection reads, familiar followers, and the link timeline.
//! Handlers in `api::mastodon::endpoints::{web, relations_extra,
//! timelines_link}` call these; no HTTP or `redb` types appear here.

use std::collections::{HashMap, HashSet};

use crate::application::ports::{
    Relationship, Store, TagFollow, TimelineFilter, TimelinePage, TimelineQuery, WebPushSubscription,
};
use crate::application::{policy, statuses as status_cases};
use crate::domain::bookmark::Bookmark;
use crate::domain::favourite::Favourite;
use crate::domain::{Account, Error, Id, Mute, Result, Status, Visibility};

/// Maximum familiar-follower ids accepted per request (matches the accounts
/// multi-fetch ceiling rather than upstream's unbounded array).
pub const FAMILIAR_FOLLOWERS_LIMIT: usize = 40;

/// Default/max page sizes mirror the sibling timeline and account endpoints.
pub const LINK_DEFAULT_LIMIT: usize = 20;
pub const LINK_MAX_LIMIT: usize = 40;
pub const COLLECTION_DEFAULT_LIMIT: usize = 40;
pub const COLLECTION_MAX_LIMIT: usize = 80;
pub const FOLLOWED_TAGS_LIMIT: usize = 100;

// ---------------------------------------------------------------------------
// Web settings
// ---------------------------------------------------------------------------

/// Read the viewer's web settings data (raw JSON, `None` when never synced).
pub fn web_settings(store: &dyn Store, user_id: Id) -> Result<Option<String>> {
    store.web_settings(user_id)
}

/// Validate and store the viewer's web settings data.
///
/// Upstream persists `params[:data]` on a per-user `Web::Setting` row. The
/// payload must be a JSON object; anything else is a 422.
pub fn update_web_settings(store: &dyn Store, user_id: Id, raw: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| {
        Error::InvalidInput("Validation failed: Data must be a valid JSON object".into())
    })?;
    if !value.is_object() {
        return Err(Error::InvalidInput(
            "Validation failed: Data must be a valid JSON object".into(),
        ));
    }
    store.set_web_settings(user_id, raw)
}

// ---------------------------------------------------------------------------
// Web push subscriptions
// ---------------------------------------------------------------------------

/// The viewer's web push subscriptions (session-scoped first-party pushes).
pub fn web_push_subscriptions(
    store: &dyn Store,
    user_id: Id,
) -> Result<Vec<WebPushSubscription>> {
    store.web_push_subscriptions(user_id)
}

/// Create a web push subscription for the viewer.
///
/// `endpoint`, `key_p256dh` and `key_auth` are required (upstream's
/// presence validations); the endpoint must be an `https://` URL.
pub fn create_web_push_subscription(
    store: &dyn Store,
    user_id: Id,
    endpoint: &str,
    key_p256dh: &str,
    key_auth: &str,
    standard: bool,
    data: &str,
    now_ms: u64,
) -> Result<WebPushSubscription> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(Error::InvalidInput(
            "Validation failed: Endpoint can't be blank".into(),
        ));
    }
    if !(endpoint.starts_with("https://") && endpoint.len() > "https://".len()) {
        return Err(Error::InvalidInput(
            "Validation failed: Endpoint must be a valid https URL".into(),
        ));
    }
    if key_p256dh.trim().is_empty() {
        return Err(Error::InvalidInput(
            "Validation failed: Key p256dh can't be blank".into(),
        ));
    }
    if key_auth.trim().is_empty() {
        return Err(Error::InvalidInput(
            "Validation failed: Key auth can't be blank".into(),
        ));
    }
    let data_value: serde_json::Value = if data.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(data).map_err(|_| {
            Error::InvalidInput("Validation failed: Data is invalid".into())
        })?
    };
    store.create_web_push_subscription(
        user_id,
        endpoint,
        key_p256dh.trim(),
        key_auth.trim(),
        standard,
        &data_value.to_string(),
        now_ms,
    )
}

/// Replace a subscription's `data` payload (update path).
pub fn update_web_push_subscription(
    store: &dyn Store,
    user_id: Id,
    id: Id,
    data: &str,
    now_ms: u64,
) -> Result<WebPushSubscription> {
    let data_value: serde_json::Value = if data.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(data).map_err(|_| {
            Error::InvalidInput("Validation failed: Data is invalid".into())
        })?
    };
    store
        .update_web_push_subscription(user_id, id, &data_value.to_string(), now_ms)?
        .ok_or_else(|| Error::NotFound("subscription".into()))
}

// NOTE: there is deliberately no application-level destroy wrapper: the
// endpoint (`api::mastodon::endpoints::web::push_destroy`) tries the
// capability-token path first (CSRF-exempt, like upstream) and only then
// the numeric-id session path (CSRF-checked). A shared wrapper would hide
// which path was taken and re-open the CSRF gap.

// ---------------------------------------------------------------------------
// Relation collection reads
// ---------------------------------------------------------------------------

/// Blocked account ids, newest first (cursor = blocked account id).
pub fn blocks_of(
    store: &dyn Store,
    viewer: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Id>> {
    store.blocks_of(viewer, max_id, limit)
}

/// Mute rows, newest first (cursor = muted account id). Expired mutes are
/// included, as upstream lists them (the serializer nulls the expiry).
pub fn mutes_of(
    store: &dyn Store,
    viewer: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Mute>> {
    store.mutes_of(viewer, max_id, limit)
}

/// The viewer's favourites, newest first (cursor = favourite row id).
pub fn favourited(
    store: &dyn Store,
    viewer: Id,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Favourite>> {
    store.favourited_statuses(viewer, max_id, since_id, min_id, limit)
}

/// The viewer's bookmarks, newest first (cursor = bookmark row id).
pub fn bookmarked(
    store: &dyn Store,
    viewer: Id,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Bookmark>> {
    store.bookmarked_statuses(viewer, max_id, since_id, min_id, limit)
}

// ---------------------------------------------------------------------------
// Familiar followers
// ---------------------------------------------------------------------------

/// One familiar-followers entry: the requested account plus the accounts the
/// viewer follows that also follow it.
pub struct FamiliarEntry {
    pub target_id: Id,
    pub familiar_ids: Vec<Id>,
}

/// Familiar followers for up to [`FAMILIAR_FOLLOWERS_LIMIT`] account ids.
///
/// Mirrors `FamiliarFollowersPresenter`: unknown and suspended targets are
/// skipped; a target with `hide_collections` yields an empty list; otherwise
/// the result is the viewer's followed accounts (excluding
/// `hide_collections` ones) that follow the target.
pub fn familiar_followers(
    store: &dyn Store,
    viewer: Id,
    ids: &[Id],
    now_ms: u64,
) -> Result<Vec<FamiliarEntry>> {
    if ids.len() > FAMILIAR_FOLLOWERS_LIMIT {
        return Err(Error::InvalidInput(format!(
            "Validation failed: Too many ids (maximum is {FAMILIAR_FOLLOWERS_LIMIT})"
        )));
    }
    // The viewer's outgoing follows, fully paged (bounded by the store's
    // scan caps), with hidden-collection and missing accounts removed.
    let mut following: HashSet<Id> = HashSet::new();
    {
        let mut cursor: Option<Id> = None;
        loop {
            let page = store.following(viewer, cursor, COLLECTION_MAX_LIMIT)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().copied();
            for id in page {
                following.insert(id);
            }
            if following.len() >= 10_000 {
                break;
            }
        }
    }
    let followed_accounts = store.accounts_by_ids(&following.iter().copied().collect::<Vec<_>>())?;
    let visible_following: HashSet<Id> = following
        .into_iter()
        .filter(|id| {
            followed_accounts
                .get(id)
                .is_none_or(|account| !account.suspended)
        })
        .collect();

    let mut entries = Vec::with_capacity(ids.len());
    for &target_id in ids {
        let Some(target) = store.account_by_id(target_id)? else {
            continue;
        };
        if target.suspended {
            continue;
        }
        // `hide_collections` does not exist in this server's account model
        // (no collections surface): every visible target is listed.
        // Accounts followed by BOTH the viewer and the target (mutual
        // follows), fully paged on the target side and intersected with
        // the viewer's visible follows.
        let mut familiars: Vec<Id> = Vec::new();
        let mut cursor: Option<Id> = None;
        'pages: loop {
            let page = store.following(target_id, cursor, COLLECTION_MAX_LIMIT)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().copied();
            for id in &page {
                if visible_following.contains(id) {
                    familiars.push(*id);
                }
            }
            if familiars.len() >= 10_000 {
                break 'pages;
            }
            if page.len() < COLLECTION_MAX_LIMIT {
                break;
            }
        }
        entries.push(FamiliarEntry {
            target_id,
            familiar_ids: familiars,
        });
    }
    let _ = now_ms;
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Tags: follows, featured tags
// ---------------------------------------------------------------------------

/// Normalize a tag name the way storage does (strip `#`, compatibility
/// fold, lowercase). Empty when nothing tag-like remains.
pub fn normalize_tag_name(raw: &str) -> String {
    status_cases::normalize_tag(raw)
}

/// Whether `raw` names a tag (`Tag::HASHTAG_NAME_RE` approximation: no
/// whitespace or fragment delimiters, non-empty after normalization).
pub fn valid_tag_name(raw: &str) -> bool {
    let stripped = raw.trim().trim_start_matches(['#', '＃']);
    if stripped.is_empty() || stripped.chars().any(char::is_whitespace) {
        return false;
    }
    if stripped
        .chars()
        .any(|c| matches!(c, '/' | '?' | '#' | '@' | '&' | '%' | '+'))
    {
        return false;
    }
    !normalize_tag_name(raw).is_empty()
}

/// Follow a tag (idempotent). Unknown-shaped names are a 404, matching
/// `TagsController#set_or_create_tag`.
pub fn follow_tag(store: &dyn Store, viewer: Id, raw: &str, now_ms: u64) -> Result<TagFollow> {
    if !valid_tag_name(raw) {
        return Err(Error::NotFound("tag".into()));
    }
    store.follow_tag(viewer, &normalize_tag_name(raw), now_ms)
}

/// Unfollow a tag (idempotent no-op when absent).
pub fn unfollow_tag(
    store: &dyn Store,
    viewer: Id,
    raw: &str,
) -> Result<Option<TagFollow>> {
    if !valid_tag_name(raw) {
        return Err(Error::NotFound("tag".into()));
    }
    store.unfollow_tag(viewer, &normalize_tag_name(raw))
}

/// Tags the viewer follows, newest first (cursor = tag-follow row id).
pub fn followed_tags(
    store: &dyn Store,
    viewer: Id,
    max_id: Option<Id>,
    since_id: Option<Id>,
    limit: usize,
) -> Result<Vec<TagFollow>> {
    store.followed_tags(viewer, max_id, since_id, limit)
}

/// Whether the viewer follows / features `raw` (both false for unknown names).
pub fn tag_relationship(store: &dyn Store, viewer: Id, raw: &str) -> Result<(bool, bool)> {
    if !valid_tag_name(raw) {
        return Ok((false, false));
    }
    let name = normalize_tag_name(raw);
    Ok((
        store.tag_follow(viewer, &name)?.is_some(),
        store.featured_tag(viewer, &name)?.is_some(),
    ))
}

/// Feature a tag on the viewer's profile (idempotent).
pub fn feature_tag(
    store: &dyn Store,
    viewer: Id,
    raw: &str,
    now_ms: u64,
) -> Result<crate::application::ports::FeaturedTag> {
    if !valid_tag_name(raw) {
        return Err(Error::NotFound("tag".into()));
    }
    store.feature_tag(viewer, &normalize_tag_name(raw), now_ms)
}

/// Remove a featured tag (no-op when absent). `id` is the numeric featured-tag row.
pub fn unfeature_tag(
    store: &dyn Store,
    viewer: Id,
    id: Id,
) -> Result<Option<crate::application::ports::FeaturedTag>> {
    store.unfeature_tag(viewer, id)
}

/// Featured tags with live usage counts: each row joined to a bounded scan
/// of the public tag timeline for that account's statuses.
pub struct FeaturedTagWithUsage {
    pub tag: crate::application::ports::FeaturedTag,
    /// Number of the account's visible statuses carrying the tag (bounded scan).
    pub statuses_count: u64,
    /// Newest matching status time, if any.
    pub last_status_at_ms: Option<u64>,
}

/// The viewer's (or any account's) featured tags with live counts, ordered by
/// count descending like upstream's `set_featured_tags`.
pub fn featured_tags(
    store: &dyn Store,
    account_id: Id,
    now_ms: u64,
) -> Result<Vec<FeaturedTagWithUsage>> {
    let rows = store.featured_tags_of(account_id)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        // Bounded usage scan: the newest 200 public tag hits, filtered to
        // this account's fetchable statuses.
        let query = TimelineQuery {
            max_id: None,
            since_id: None,
            min_id: None,
            limit: 200,
            only_media: false,
            local_only: false,
            remote_only: false,
        };
        let filter = TagUsageFilter {
            store,
            account_id,
            now_ms,
        };
        let page = store.hashtag_timeline(&row.tag, query, &filter)?;
        let mut count = 0u64;
        let mut last: Option<u64> = None;
        for status in &page.statuses {
            if status.account_id != account_id {
                continue;
            }
            count += 1;
            last = Some(last.map_or(status.created_at_ms, |prev: u64| {
                prev.max(status.created_at_ms)
            }));
        }
        out.push(FeaturedTagWithUsage {
            tag: row,
            statuses_count: count,
            last_status_at_ms: last,
        });
    }
    out.sort_by(|a, b| {
        b.statuses_count
            .cmp(&a.statuses_count)
            .then_with(|| a.tag.tag.cmp(&b.tag.tag))
    });
    Ok(out)
}

struct TagUsageFilter<'a> {
    store: &'a dyn Store,
    account_id: Id,
    now_ms: u64,
}

impl TimelineFilter for TagUsageFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        if status.visibility != Visibility::Public {
            return false;
        }
        let Ok(Some(author)) = self.store.account_by_id(status.account_id) else {
            return false;
        };
        if author.suspended {
            return false;
        }
        let Ok(pair) = self
            .store
            .pair_state(self.account_id, status.account_id, self.now_ms)
        else {
            return false;
        };
        policy::can_fetch(Some(self.account_id), status, pair)
    }
}

// ---------------------------------------------------------------------------
// Link timeline
// ---------------------------------------------------------------------------

struct LinkUrlFilter {
    url: String,
}

impl TimelineFilter for LinkUrlFilter {
    fn include(&self, status: &Status) -> bool {
        if status.is_reblog() {
            return false;
        }
        let self_reply_or_plain = status.in_reply_to_id.is_none()
            || status.in_reply_to_account_id == Some(status.account_id);
        status.visibility == Visibility::Public
            && self_reply_or_plain
            && status.source_text.contains(self.url.as_str())
    }
}

/// The link timeline: public statuses containing an approved trend-link URL.
///
/// The URL must match an `allowed` trend link row exactly, else 404 (upstream's
/// `PreviewCard...find_by!`). Viewer gating (blocks, mutes, domain blocks,
/// suspended authors) is applied to the page in one batched read.
pub fn link_timeline(
    store: &dyn Store,
    viewer: Option<Id>,
    url: &str,
    query: TimelineQuery,
    now_ms: u64,
) -> Result<TimelinePage> {
    if url.trim().is_empty() {
        return Err(Error::NotFound("link".into()));
    }
    let approved = store
        .all_trend_links()?
        .into_iter()
        .find(|link| link.allowed == Some(true) && link.url == url);
    if approved.is_none() {
        return Err(Error::NotFound("link".into()));
    }
    let page = store.public_timeline(query, true, false, &LinkUrlFilter {
        url: url.to_string(),
    })?;
    if viewer.is_none() {
        return Ok(page);
    }
    let viewer = viewer.expect("checked");
    // Batch-gate the page authors: one account load plus one relationship
    // read for the whole page.
    let mut author_ids: Vec<Id> = page.statuses.iter().map(|s| s.account_id).collect();
    author_ids.sort_unstable();
    author_ids.dedup();
    let accounts: HashMap<Id, Account> = store.accounts_by_ids(&author_ids)?;
    let targets: Vec<(Id, String)> = author_ids
        .iter()
        .filter_map(|id| {
            accounts
                .get(id)
                .map(|account| (*id, account.domain.clone()))
        })
        .collect();
    let relationships = store.relationships(viewer, &targets, now_ms)?;
    let blocked: HashSet<Id> = relationships
        .iter()
        .filter(|rel| {
            rel.blocking || rel.blocked_by || rel.domain_blocking || rel.muting
        })
        .map(|rel| rel.target_id)
        .collect();
    let suspended: HashSet<Id> = accounts
        .iter()
        .filter(|(_, account)| account.suspended)
        .map(|(id, _)| *id)
        .collect();
    Ok(TimelinePage {
        statuses: page
            .statuses
            .into_iter()
            .filter(|status| {
                status.account_id == viewer
                    || (!blocked.contains(&status.account_id)
                        && !suspended.contains(&status.account_id))
            })
            .collect(),
        scanned: page.scanned,
    })
}

/// Relationship aggregates for viewer/target pairs (familiar-follower page
/// rendering shares the DTO with the relationships endpoint).
pub fn relationships_for(
    store: &dyn Store,
    viewer: Id,
    targets: &[Id],
    now_ms: u64,
) -> Result<Vec<Relationship>> {
    crate::application::relationships::relationships_for(store, viewer, targets, now_ms)
}

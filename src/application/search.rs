//! Search use cases (Phase 4): `GET /api/v2/search` over local data.
//!
//! Mastodon v4.7.3 has no `GET /api/v1/search` (removed in 3.0) and its
//! status search requires Elasticsearch, so `statuses` is always `[]`
//! upstream without Chewy. This server keeps a documented local profile:
//! accounts by handle/display-name substring, hashtags by tag prefix, and
//! statuses by bounded recent-scan text match (authenticated callers only,
//! like Mastodon's `status_searchable?`). No remote resolution (`resolve`)
//! — federation is Phase 6.

use crate::application::ports::Store;
use crate::domain::{Account, Error, Id, Result, Status};

/// Mastodon `Api::V2::SearchController::RESULTS_LIMIT` (doubled maximum).
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 40;

/// Bounded local search results (newest first for statuses).
pub struct SearchResults {
    pub accounts: Vec<Account>,
    pub hashtags: Vec<String>,
    pub statuses: Vec<Status>,
}

/// `GET /api/v2/search` — search local data.
///
/// `type` narrows the search; an unknown type returns empty arrays (Mastodon
/// runs no branch for it). `offset` pages accounts/hashtags only (bounded).
pub fn search(
    store: &dyn Store,
    viewer: Option<Id>,
    query: &str,
    kind: Option<&str>,
    offset: usize,
    limit: usize,
    following: bool,
    account_id: Option<Id>,
    max_id: Option<Id>,
    min_id: Option<Id>,
) -> Result<SearchResults> {
    let query = query.trim();
    let wants_accounts = kind.is_none_or(|k| k == "accounts");
    let wants_hashtags = kind.is_none_or(|k| k == "hashtags");
    let wants_statuses = kind == Some("statuses");

    let mut results = SearchResults {
        accounts: Vec::new(),
        hashtags: Vec::new(),
        statuses: Vec::new(),
    };
    if query.is_empty() {
        return Ok(results);
    }

    if wants_accounts {
        results.accounts = search_accounts(store, viewer, query, offset, limit, following)?;
    }
    if wants_hashtags {
        results.hashtags = search_hashtags(store, query, offset, limit)?;
    }
    if wants_statuses {
        // Mastodon only serves status search to authenticated callers (with
        // Elasticsearch enabled upstream). Anonymous callers get `[]`.
        if let Some(viewer) = viewer {
            results.statuses = search_statuses(store, viewer, query, limit, account_id, max_id, min_id)?;
        }
    }
    Ok(results)
}

/// Accounts whose `acct`/`username`/`display_name` contains the query
/// (bounded scan over the handle index plus account rows).
fn search_accounts(
    store: &dyn Store,
    viewer: Option<Id>,
    query: &str,
    offset: usize,
    limit: usize,
    following: bool,
) -> Result<Vec<Account>> {
    let needle = query.to_ascii_lowercase();
    let candidates: Vec<Account> = store.search_accounts(&needle, 500)?;
    let mut out: Vec<Account> = Vec::new();
    for account in candidates {
        // Hidden/suspended accounts never surface (Mastodon
        // `Account.searchable`).
        if account.suspended {
            continue;
        }
        if following {
            let Some(viewer) = viewer else {
                continue;
            };
            let Some(follow) = store.follow_of(viewer, account.id)? else {
                continue;
            };
            if follow.state != crate::domain::FollowState::Active {
                continue;
            }
        }
        out.push(account);
    }
    out.drain(..out.len().min(offset));
    out.truncate(limit);
    Ok(out)
}

/// Hashtags starting with the query (tag index prefix scan).
fn search_hashtags(
    store: &dyn Store,
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<Vec<String>> {
    let normalized = crate::application::statuses::normalize_tag(query);
    if normalized.is_empty() {
        return Ok(Vec::new());
    }
    let mut tags = store.search_tags(&normalized, 1_000)?;
    tags.retain(|tag| tag.starts_with(&normalized));
    tags.drain(..tags.len().min(offset));
    tags.truncate(limit);
    Ok(tags)
}

/// Recent statuses whose text mentions the query (bounded recent scan; the
/// caller is authenticated, mirroring Mastodon's `status_searchable?`).
fn search_statuses(
    store: &dyn Store,
    viewer: Id,
    query: &str,
    limit: usize,
    account_id: Option<Id>,
    max_id: Option<Id>,
    min_id: Option<Id>,
) -> Result<Vec<Status>> {
    let needle = query.to_ascii_lowercase();
    let candidates = store.recent_statuses(account_id, max_id, min_id, 2_000)?;
    let mut out = Vec::new();
    for status in candidates {
        let haystack = format!(
            "{}\n{}",
            status.source_text.to_ascii_lowercase(),
            status.spoiler_text.clone().unwrap_or_default().to_ascii_lowercase()
        );
        if !haystack.contains(&needle) {
            continue;
        }
        // Visibility: only statuses the viewer may fetch.
        let pair = store.pair_state(status.account_id, viewer, 0)?;
        if !crate::application::policy::can_fetch(Some(viewer), &status, pair) {
            continue;
        }
        out.push(status);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Validate the `q` parameter (Mastodon `params.require(:q)` → 400 with the
/// ParameterMissing message).
pub fn require_query(raw: Option<&str>) -> Result<&str> {
    let query = raw.map(str::trim).unwrap_or("");
    if query.is_empty() {
        return Err(Error::InvalidInput(
            "param is missing or the value is empty: q".into(),
        ));
    }
    Ok(query)
}

//! Search use cases: `GET /api/v2/search` over local data.
//!
//! Mastodon v4.7.3 has no `GET /api/v1/search` (removed in 3.0) and its
//! status search requires Elasticsearch, so `statuses` is always `[]`
//! upstream without Chewy. This server keeps a documented local profile:
//! accounts by handle/display-name substring, hashtags by tag prefix, and
//! statuses by text match (authenticated callers only, like Mastodon's
//! `status_searchable?`). No remote resolution (`resolve`) — federation is
//! Phase 6.
//!
//! Phase 7.2: status search runs behind [`SearchMode`](crate::search::SearchMode)
//! (`embedded` default / `database` / `off`) via [`search_with_mode`]. The
//! legacy [`search`] entry keeps the Phase 4 database behavior for callers
//! that do not thread a mode (it is the `Database` arm, exactly).

use crate::application::ports::Store;
use crate::domain::{Account, Error, Id, Result, Status};
use crate::search::{EmbeddedIndex, SearchMode};

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
            results.statuses =
                search_statuses(store, viewer, query, limit, account_id, max_id, min_id, false)?;
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
    aperture: bool,
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
        // Aperture (Stage 2): statuses without media leave search.
        if aperture && status.media_ids.is_empty() {
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

// ---------------------------------------------------------------------------
// Phase 7.2: backend dispatch.
// ---------------------------------------------------------------------------

/// Upper bound on embedded-index candidates pulled per status search. The
/// viewer, cursor and account filters apply after the index lookup, so the
/// pull is wider than `limit`; the bound keeps one search to a bounded
/// candidate set (mirroring the `2_000` database scan bound below).
const EMBEDDED_CANDIDATE_CAP: usize = 2_000;

/// `GET /api/v2/search` with an explicit search backend.
///
/// Accounts and hashtags always take the database path (upstream's account
/// search is database-backed, and hashtag search is a tag prefix scan, not
/// full text). Only statuses switch:
///
/// - `Embedded`: the in-process index; `index` must be `Some` (a missing
///   index falls back to the database scan rather than failing the search).
/// - `Database`: the Phase 4 interim bounded recent scan.
/// - `Off`: statuses are always empty, matching upstream with no engine.
///
/// Every other rule is mode-independent: `type` narrowing, unknown-type
/// emptiness, `offset`/`limit` paging, `following`, `resolve`/`collections`
/// handling (owned by the endpoint), and authenticated-only status search.
#[allow(clippy::too_many_arguments)]
pub fn search_with_mode(
    mode: SearchMode,
    store: &dyn Store,
    index: Option<&EmbeddedIndex>,
    viewer: Option<Id>,
    query: &str,
    kind: Option<&str>,
    offset: usize,
    limit: usize,
    following: bool,
    account_id: Option<Id>,
    max_id: Option<Id>,
    min_id: Option<Id>,
    aperture: bool,
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
            results.statuses = match mode {
                SearchMode::Off => Vec::new(),
                SearchMode::Database => {
                    search_statuses(store, viewer, query, limit, account_id, max_id, min_id, aperture)?
                }
                SearchMode::Embedded => match index {
                    Some(index) if !index.is_empty() => search_statuses_embedded(
                        store, index, viewer, query, limit, account_id, max_id, min_id, aperture,
                    )?,
                    // No (or cold) index: degrade to the database scan
                    // rather than failing the search. A cold index and a
                    // fresh server both read empty here; once the backfill
                    // chain fills the index this arm stops triggering.
                    _ => {
                        search_statuses(store, viewer, query, limit, account_id, max_id, min_id, aperture)?
                    }
                },
            };
        }
    }
    Ok(results)
}

/// Status search over the embedded index.
///
/// The index yields newest-first candidates; cursor (`max_id`/`min_id`),
/// author (`account_id`) and fetchability (`can_fetch`: blocks, visibility,
/// deletion) filters apply after the lookup, exactly as on the database
/// path, before truncating to `limit`.
#[allow(clippy::too_many_arguments)]
fn search_statuses_embedded(
    store: &dyn Store,
    index: &EmbeddedIndex,
    viewer: Id,
    query: &str,
    limit: usize,
    account_id: Option<Id>,
    max_id: Option<Id>,
    min_id: Option<Id>,
    aperture: bool,
) -> Result<Vec<Status>> {
    let candidates = index.search_ids(query, EMBEDDED_CANDIDATE_CAP);
    let mut ids: Vec<Id> = Vec::new();
    for id in candidates {
        if max_id.is_some_and(|m| id >= m) {
            continue;
        }
        if min_id.is_some_and(|m| id <= m) {
            continue;
        }
        ids.push(id);
    }
    // One batched load (non-deleted rows only, newest first).
    let mut statuses = store.statuses_by_ids(&ids)?;
    if let Some(author) = account_id {
        statuses.retain(|status| status.account_id == author);
    }
    let mut out = Vec::new();
    for status in statuses {
        // Visibility: only statuses the viewer may fetch (re-checked at
        // serve time, since the index stores text, not permissions).
        let pair = store.pair_state(status.account_id, viewer, 0)?;
        if !crate::application::policy::can_fetch(Some(viewer), &status, pair) {
            continue;
        }
        // Aperture (Stage 2): statuses without media leave search.
        if aperture && status.media_ids.is_empty() {
            continue;
        }
        out.push(status);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

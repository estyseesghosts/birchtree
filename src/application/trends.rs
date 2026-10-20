//! Trend candidates and approvals (Phase 7.3).
//!
//! Public reads serve only admin-approved items; unreviewed and rejected rows
//! never appear there. Candidates are derived from real local usage —
//! hashtag counts, recent public statuses, URLs found in status text — and
//! joined to the persisted approval flags. Nothing here fabricates ranks,
//! scores or preview cards.

use std::collections::{HashMap, HashSet};

use crate::application::ports::{Store, TimelineFilter, TimelineQuery};
use crate::domain::{Id, Result, Status, TrendLink, TrendPublisher, TrendTagFlag};

/// Recent public statuses scanned to derive link/status candidates.
const CANDIDATE_SCAN_CAP: usize = 200;
/// Distinct tags considered per read.
const TAG_CANDIDATE_CAP: usize = 500;

/// Parse Mastodon's trends paging: `offset` (default 0, negatives clamp to
/// 0) and `limit` (default `default`, clamped to `default * 2`, like
/// `limit_param` upstream).
pub fn parse_offset_limit(
    offset_raw: Option<&str>,
    limit_raw: Option<&str>,
    default: usize,
) -> (usize, usize) {
    let offset = offset_raw
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .map(|value| value.max(0) as usize)
        .unwrap_or(0);
    let limit = limit_raw
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .map(|value| value.clamp(0, (default * 2) as i64) as usize)
        .unwrap_or(default);
    (offset, limit)
}

/// Apply an `(offset, limit)` page to an ordered candidate list.
pub fn page<T>(items: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    items.into_iter().skip(offset).take(limit).collect()
}

/// A filter that admits every status (candidate derivation reads the public
/// timeline, which already holds only `public`-visibility statuses).
struct AllowAll;

impl TimelineFilter for AllowAll {
    fn include(&self, _status: &Status) -> bool {
        true
    }
}

/// Recent public statuses (newest first), bounded — the shared candidate
/// input for link and status trends.
pub fn recent_public_statuses(store: &dyn Store, cap: usize) -> Result<Vec<Status>> {
    let page = store.public_timeline(
        TimelineQuery {
            limit: cap.min(CANDIDATE_SCAN_CAP),
            ..Default::default()
        },
        true,
        false,
        &AllowAll,
    )?;
    Ok(page.statuses)
}

/// Extract `http(s)` URLs from status text in order, deduplicated.
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for token in text.split(char::is_whitespace) {
        let mut url = token.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        // Strip trailing punctuation that is not part of the URL.
        while url.ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']) && url.len() > 8
        {
            url = &url[..url.len() - 1];
        }
        if url.len() > 8 && seen.insert(url.to_string()) {
            out.push(url.to_string());
        }
    }
    out
}

/// One tag candidate: real usage joined to its approval flag (`None` when
/// never reviewed).
#[derive(Debug, Clone)]
pub struct TagCandidate {
    pub name: String,
    pub uses: u64,
    pub flag: Option<TrendTagFlag>,
}

/// Approved tag names for the public read, most-used first.
pub fn approved_tags(store: &dyn Store) -> Result<Vec<TrendTagFlag>> {
    let mut out = Vec::new();
    for usage in store.trend_tag_usages(TAG_CANDIDATE_CAP)? {
        match store.trend_tag_flag(&usage.name)? {
            Some(flag) if flag.allowed == Some(true) => out.push(flag),
            _ => {}
        }
    }
    Ok(out)
}

/// All tag candidates (usage ∪ reviewed flags for vanished tags), most-used
/// first. Materializes pending flag rows so every candidate has a numeric
/// admin `:id`.
pub fn tag_candidates(store: &dyn Store, now_ms: u64) -> Result<Vec<TagCandidate>> {
    let mut by_name: HashMap<String, TagCandidate> = HashMap::new();
    for usage in store.trend_tag_usages(TAG_CANDIDATE_CAP)? {
        by_name.insert(
            usage.name.clone(),
            TagCandidate {
                name: usage.name,
                uses: usage.uses,
                flag: None,
            },
        );
    }
    for flag in store.all_trend_tag_flags()? {
        if let Some(candidate) = by_name.get_mut(&flag.name) {
            candidate.flag = Some(flag);
        } else {
            by_name.insert(
                flag.name.clone(),
                TagCandidate {
                    name: flag.name.clone(),
                    uses: 0,
                    flag: Some(flag),
                },
            );
        }
    }
    // Mint pending rows for candidates that have never been reviewed.
    let mut out: Vec<TagCandidate> = Vec::new();
    for mut candidate in by_name.into_values() {
        if candidate.flag.is_none() {
            candidate.flag = Some(store.ensure_trend_tag(&candidate.name, now_ms)?);
        }
        out.push(candidate);
    }
    out.sort_by(|a, b| b.uses.cmp(&a.uses).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// Approved links for the public read (newest first).
///
/// Ordering note: upstream orders by trend score; this server has no score
/// pipeline, so rows are newest-first (documented in the endpoint manifest).
pub fn approved_links(store: &dyn Store) -> Result<Vec<TrendLink>> {
    Ok(store
        .all_trend_links()?
        .into_iter()
        .filter(|link| link.allowed == Some(true))
        .collect())
}

/// All link candidates: derive URLs from recent public statuses, materialize
/// rows, and return every row (newest first).
pub fn link_candidates(store: &dyn Store, now_ms: u64) -> Result<Vec<TrendLink>> {
    let statuses = recent_public_statuses(store, CANDIDATE_SCAN_CAP)?;
    let mut urls = Vec::new();
    for status in &statuses {
        urls.extend(extract_urls(&status.source_text));
    }
    if !urls.is_empty() {
        store.ensure_trend_links(&urls, now_ms)?;
    }
    store.all_trend_links()
}

/// One publisher candidate with its flag (`None` when never reviewed —
/// impossible in practice: publisher rows materialize alongside link rows,
///
/// always pending at minimum).
pub fn publisher_candidates(store: &dyn Store) -> Result<Vec<TrendPublisher>> {
    store.all_trend_publishers()
}

/// One status candidate: the status plus its approval state (`None` when
/// never reviewed).
#[derive(Debug, Clone)]
pub struct StatusCandidate {
    pub status: Status,
    pub allowed: Option<bool>,
}

/// Whether a viewer may see a candidate status in trends (the
/// `filtered_for` equivalent): the central show gate plus exclusions for
/// authors the viewer mutes/blocks or whose domain the viewer blocks.
fn trend_visible_for(
    store: &dyn Store,
    viewer: Option<Id>,
    status: &Status,
    viewer_domain_blocks: &HashSet<String>,
    now_ms: u64,
) -> Result<bool> {
    // Approval never overrides visibility: the status must be fetchable by
    // this viewer (upstream's trends query has no visibility re-check, so an
    // admin-approved non-public status would leak there; this server refuses
    // that leak — see the compatibility profile). Deleted statuses and
    // reblog wrappers are never trend payloads.
    if status.is_deleted() || status.is_reblog() {
        return Ok(false);
    }
    if crate::application::statuses::show_status(store, viewer, status.id, now_ms).is_err() {
        return Ok(false);
    }
    let Some(viewer) = viewer else {
        return Ok(true);
    };
    let pair = store.pair_state(viewer, status.account_id, now_ms)?;
    if pair.muting || pair.blocking || pair.blocked_by {
        return Ok(false);
    }
    if let Ok(Some(author)) = store.account_by_id(status.account_id)
        && viewer_domain_blocks.contains(&author.domain)
    {
        return Ok(false);
    }
    Ok(true)
}

/// Approved statuses for the public read (newest first).
pub fn approved_statuses(
    store: &dyn Store,
    viewer: Option<Id>,
    now_ms: u64,
) -> Result<Vec<Status>> {
    let domain_blocks = viewer_domain_block_set(store, viewer, now_ms)?;
    let mut out = Vec::new();
    for flag in store.all_trend_status_flags()? {
        if !flag.allowed {
            continue;
        }
        let Some(status) = store.status_by_id(flag.status_id)? else {
            continue;
        };
        if trend_visible_for(store, viewer, &status, &domain_blocks, now_ms)? {
            out.push(status);
        }
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(out)
}

/// All status candidates (recent public statuses ∪ every reviewed flag),
/// newest first, each carrying its approval state.
pub fn status_candidates(store: &dyn Store) -> Result<Vec<StatusCandidate>> {
    let mut flags: HashMap<Id, bool> = HashMap::new();
    for flag in store.all_trend_status_flags()? {
        flags.insert(flag.status_id, flag.allowed);
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for status in recent_public_statuses(store, CANDIDATE_SCAN_CAP)? {
        // Admin candidates skip viewer filtering (moderators see the whole
        // queue); the public read filters instead.
        if seen.insert(status.id) {
            out.push(StatusCandidate {
                allowed: flags.get(&status.id).copied(),
                status,
            });
        }
    }
    for (status_id, allowed) in &flags {
        if seen.contains(status_id) {
            continue;
        }
        if let Some(status) = store.status_by_id(*status_id)?
            && !status.is_deleted()
            && seen.insert(status.id)
        {
            out.push(StatusCandidate {
                allowed: Some(*allowed),
                status,
            });
        }
    }
    out.sort_by(|a, b| b.status.id.cmp(&a.status.id));
    Ok(out)
}

fn viewer_domain_block_set(
    store: &dyn Store,
    viewer: Option<Id>,
    _now_ms: u64,
) -> Result<HashSet<String>> {
    let Some(viewer) = viewer else {
        return Ok(HashSet::new());
    };
    // Domain blocks are few per account; one bounded read per trends request.
    let mut set = HashSet::new();
    for block in store.domain_blocks(viewer, None, None, 500)? {
        set.insert(block.domain);
    }
    Ok(set)
}

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
/// Statuses read per tag for the 7-day history buckets (bounded).
const HISTORY_SCAN: usize = 100;
/// Days of tag history served (upstream `Trends::History#each`: 7 days).
const HISTORY_DAYS: u64 = 7;

/// Trend-score decay scale: one day in milliseconds.
///
/// Upstream (`Trends::Tags::default_options`, Mastodon v4.7.3
/// `app/models/trends/tags.rb`) scores observed uses in Redis with a
/// 4-hour peak halflife, a 2-day max-score cooldown, a threshold of 5
/// unique accounts, and orders `score: :desc` (plus a locale clause when
/// the viewer set preferred languages). This server keeps no Redis, so
/// scores are derived at read time from stored rows — usage counts with a
/// day-scale decay from the newest use — and ordered the same way
/// (score first). The decay shape is this server's (decided D40); the
/// ordering contract (score-ordered, approved-only) matches upstream.
/// There is no threshold filter: an approved item is always listed.
pub const SCORE_HALFLIFE_MS: f64 = 86_400_000.0;

/// Score of `uses` observations whose newest use is `newest_ms`
/// (`None` = no recency information; the raw count stands).
pub fn trend_score(uses: u64, newest_ms: Option<u64>, now_ms: u64) -> f64 {
    let base = uses as f64;
    let Some(newest_ms) = newest_ms else {
        return base;
    };
    let age_ms = now_ms.saturating_sub(newest_ms) as f64;
    base / (1.0 + age_ms / SCORE_HALFLIFE_MS)
}

/// Engagement score of one status trend candidate: reblogs + favourites
/// with the same day-scale decay from the status's own timestamp (`+1` so
/// a freshly approved zero-engagement status still ranks instead of
/// vanishing; ordering only, never filtering).
pub fn status_score(status: &Status, now_ms: u64) -> f64 {
    let engagement = status.reblogs_count.saturating_add(status.favourites_count);
    trend_score(engagement.saturating_add(1), Some(status.id.timestamp_ms()), now_ms)
}

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
    pub score: f64,
    pub flag: Option<TrendTagFlag>,
}

/// Score every tag usage: the stored count decayed by the recency of its
/// newest indexed status (one bounded index read per tag).
fn score_usages(store: &dyn Store, now_ms: u64) -> Result<Vec<(String, u64, f64)>> {
    let mut out = Vec::new();
    for usage in store.trend_tag_usages(TAG_CANDIDATE_CAP)? {
        let newest = store
            .tag_newest_status_id(&usage.name)?
            .map(|id| id.timestamp_ms());
        out.push((usage.name, usage.uses, trend_score(usage.uses, newest, now_ms)));
    }
    Ok(out)
}

/// Approved tag names for the public read, score-ordered.
pub fn approved_tags(store: &dyn Store, now_ms: u64) -> Result<Vec<TrendTagFlag>> {
    let mut scored: Vec<(TrendTagFlag, f64)> = Vec::new();
    for (name, _uses, score) in score_usages(store, now_ms)? {
        match store.trend_tag_flag(&name)? {
            Some(flag) if flag.allowed == Some(true) => scored.push((flag, score)),
            _ => {}
        }
    }
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.name.cmp(&b.0.name))
    });
    Ok(scored.into_iter().map(|(flag, _)| flag).collect())
}

/// All tag candidates (usage ∪ reviewed flags for vanished tags),
/// score-ordered. Materializes pending flag rows so every candidate has a
/// numeric admin `:id`.
pub fn tag_candidates(store: &dyn Store, now_ms: u64) -> Result<Vec<TagCandidate>> {
    let mut by_name: HashMap<String, TagCandidate> = HashMap::new();
    for (name, uses, score) in score_usages(store, now_ms)? {
        by_name.insert(
            name.clone(),
            TagCandidate {
                name,
                uses,
                score,
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
                    score: 0.0,
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
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.uses.cmp(&a.uses))
            .then(a.name.cmp(&b.name))
    });
    Ok(out)
}

/// Per-URL occurrence stats over recent public statuses: (shares,
/// newest-share timestamp). The shared candidate-scan input for link
/// scores, so tags, links and statuses all order by observed usage.
pub fn link_occurrences(store: &dyn Store) -> Result<HashMap<String, (u64, u64)>> {
    let mut out: HashMap<String, (u64, u64)> = HashMap::new();
    for status in recent_public_statuses(store, CANDIDATE_SCAN_CAP)? {
        let created = status.id.timestamp_ms();
        for url in extract_urls(&status.source_text) {
            let entry = out.entry(url).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry.1.max(created);
        }
    }
    Ok(out)
}

/// Score of one link row from its occurrence stats (`None` = the URL was
/// not seen in the recent scan; the row still ranks by recency of its own
/// materialization).
fn link_score(url: &str, occurrences: &HashMap<String, (u64, u64)>, fallback_ms: u64, now_ms: u64) -> f64 {
    match occurrences.get(url) {
        Some((shares, newest)) => trend_score(*shares, Some(*newest), now_ms),
        None => trend_score(1, Some(fallback_ms), now_ms),
    }
}

/// Approved links for the public read, score-ordered (upstream
/// `Trends::Links` orders by score; the score here is observed shares
/// with the same day-scale decay as tags).
pub fn approved_links(store: &dyn Store, now_ms: u64) -> Result<Vec<TrendLink>> {
    let occurrences = link_occurrences(store)?;
    let mut out: Vec<TrendLink> = store
        .all_trend_links()?
        .into_iter()
        .filter(|link| link.allowed == Some(true))
        .collect();
    out.sort_by(|a, b| {
        link_score(&b.url, &occurrences, b.created_at_ms, now_ms)
            .partial_cmp(&link_score(&a.url, &occurrences, a.created_at_ms, now_ms))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.id.cmp(&a.id))
    });
    Ok(out)
}

/// All link candidates: derive URLs from recent public statuses, materialize
/// rows, and return every row score-ordered.
pub fn link_candidates(store: &dyn Store, now_ms: u64) -> Result<Vec<TrendLink>> {
    let statuses = recent_public_statuses(store, CANDIDATE_SCAN_CAP)?;
    let mut urls = Vec::new();
    for status in &statuses {
        urls.extend(extract_urls(&status.source_text));
    }
    if !urls.is_empty() {
        store.ensure_trend_links(&urls, now_ms)?;
    }
    let rows = store.all_trend_links()?;
    let mut occurrences: HashMap<String, (u64, u64)> = HashMap::new();
    for status in &statuses {
        let created = status.id.timestamp_ms();
        for url in extract_urls(&status.source_text) {
            let entry = occurrences.entry(url).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry.1.max(created);
        }
    }
    let mut out = rows;
    out.sort_by(|a, b| {
        link_score(&b.url, &occurrences, b.created_at_ms, now_ms)
            .partial_cmp(&link_score(&a.url, &occurrences, a.created_at_ms, now_ms))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.id.cmp(&a.id))
    });
    Ok(out)
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
    pub score: f64,
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

/// Approved statuses for the public read, score-ordered (upstream
/// `Trends::Statuses` orders by score; the score here is observed
/// reblogs + favourites with the same day-scale decay as tags).
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
    out.sort_by(|a, b| {
        status_score(b, now_ms)
            .partial_cmp(&status_score(a, now_ms))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.id.cmp(&a.id))
    });
    Ok(out)
}

/// All status candidates (recent public statuses ∪ every reviewed flag),
/// score-ordered, each carrying its approval state.
pub fn status_candidates(store: &dyn Store, now_ms: u64) -> Result<Vec<StatusCandidate>> {
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
                score: status_score(&status, now_ms),
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
                score: status_score(&status, now_ms),
                status,
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.status.id.cmp(&a.status.id))
    });
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

/// One day of tag history (upstream `Trends::History::Day#as_json`:
/// `{day, uses, accounts}`, all rendered as strings on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagHistoryDay {
    /// Midnight UTC of the day, in unix seconds.
    pub day_sec: u64,
    pub uses: u64,
    pub accounts: u64,
}

/// Seven days of tag history ending today, newest first — derived from the
/// tag's most recent indexed statuses (bounded scan), never fabricated.
/// A tag with no indexed statuses still yields seven zero days (the wire
/// shape is always present; upstream renders the same shape from Redis).
pub fn tag_history(store: &dyn Store, tag: &str, now_ms: u64) -> Result<Vec<TagHistoryDay>> {
    const DAY_MS: u64 = 86_400_000;
    let today = (now_ms / DAY_MS) * (DAY_MS / 1000);
    let mut days: Vec<TagHistoryDay> = (0..HISTORY_DAYS)
        .map(|ago| TagHistoryDay {
            day_sec: today.saturating_sub(ago * (DAY_MS / 1000)),
            uses: 0,
            accounts: 0,
        })
        .collect();
    if tag.is_empty() {
        return Ok(days);
    }
    let page = store.hashtag_timeline(
        tag,
        TimelineQuery {
            limit: HISTORY_SCAN,
            ..Default::default()
        },
        &AllowAll,
    )?;
    let mut seen_accounts: Vec<HashSet<Id>> = vec![HashSet::new(); HISTORY_DAYS as usize];
    for status in &page.statuses {
        if status.is_deleted() {
            continue;
        }
        let day = (status.id.timestamp_ms() / DAY_MS) * (DAY_MS / 1000);
        for (index, entry) in days.iter_mut().enumerate() {
            if entry.day_sec == day {
                entry.uses += 1;
                if seen_accounts[index].insert(status.account_id) {
                    entry.accounts += 1;
                }
                break;
            }
        }
    }
    Ok(days)
}

/// Ordering-only locale pass (D40): when the caller names a `locale`,
/// statuses stored in that language sort first, stably. Tags and links
/// carry no locale data, so the parameter is accepted but has no effect
/// there (real per-user locale ranking needs the `chosen_languages`
/// setting, gap D21). Never filters: every approved item is still listed.
pub fn locale_first(statuses: Vec<Status>, locale: Option<&str>) -> Vec<Status> {
    let Some(locale) = locale.map(str::trim).filter(|locale| !locale.is_empty()) else {
        return statuses;
    };
    let (matching, rest): (Vec<Status>, Vec<Status>) = statuses
        .into_iter()
        .partition(|status| {
            status
                .language
                .as_deref()
                .is_some_and(|language| language.eq_ignore_ascii_case(locale))
        });
    matching.into_iter().chain(rest).collect()
}

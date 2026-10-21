//! Timeline range scans (fan-out-on-read).
//!
//! Mastodon materializes home/list feeds in Redis; this server deliberately
//! does not (`birchtree.md` §2.6), so every timeline is an *indexed, bounded*
//! scan: descending-id range reads over `TIMELINE_PUBLIC`/`TIMELINE_LOCAL`,
//! `STATUS_AUTHOR_INDEX` (k-way merge for home/list) or `HASHTAG_INDEX`,
//! stopped by per-call scan caps and filtered by the caller's
//! [`TimelineFilter`] as the page fills. Each method reports the number of
//! index entries it walked so the bounded-scan tests can assert it.

use std::collections::BinaryHeap;

use redb::{ReadableTable, ReadTransaction};

use crate::application::ports::{TimelineFilter, TimelinePage, TimelineQuery, TimelineStore};
use crate::domain::{Id, Result, Status};

use super::schema::{
    self, HASHTAG_INDEX, STATUSES, STATUS_AUTHOR_INDEX, TIMELINE_LOCAL, TIMELINE_PUBLIC,
};

/// Status id of an author-index key `(author_be || desc(id)_be)`.
fn merged_id(key: &[u8]) -> Id {
    Id::new(u64::MAX - u64::from_be_bytes(key[8..].try_into().unwrap()))
}
use super::{decode, RedbStore};

/// Index entries walked per public/tag timeline call.
const PUBLIC_SCAN_CAP: usize = 2_000;
/// Index entries walked per hashtag timeline call.
const HASHTAG_SCAN_CAP: usize = 2_000;
/// Total index entries a single home/list merge may walk, counting the head
/// of every author cursor. Sized so a home request at the follow cap stays
/// within 5 000 `items_scanned` in the worst case: the follow scan adds up to
/// 1 000 entries of its own.
const AUTHORS_MERGE_CAP: usize = 4_000;
/// Authors merged per call (the application passes already-bounded lists;
/// this is the store-level backstop). One above the home cap: the viewer plus
/// `HOME_AUTHOR_CAP` follows (Phase 4.1). A smaller value silently drops the
/// oldest follows from the merge.
const AUTHORS_CAP: usize = 1_001;

/// Read a non-deleted status row.
fn status_at(read: &ReadTransaction, id: Id) -> Result<Option<Status>> {
    let table = read.open_table(STATUSES)?;
    match table.get(id.value())? {
        Some(guard) => {
            let status: Status = decode(guard.value())?;
            if status.is_deleted() {
                Ok(None)
            } else {
                Ok(Some(status))
            }
        }
        None => Ok(None),
    }
}

/// Descending-id scan over a `(desc(id) -> ())` range index.
///
/// Cursor mapping (exclusive on both ends, like Mastodon): `id < max_id`,
/// `id > since_id` / `id > min_id`. Returns the accepted statuses and the
/// number of index entries walked.
fn scan_desc_range<R, F>(
    read: &ReadTransaction,
    table: &R,
    query: &TimelineQuery,
    scan_cap: usize,
    mut accept: F,
) -> Result<(Vec<Status>, usize)>
where
    R: ReadableTable<u64, ()> + ?Sized,
    F: FnMut(&Status) -> bool,
{
    if query.limit == 0 {
        return Ok((Vec::new(), 0));
    }
    if query.max_id.is_some_and(|m| m.value() == 0) {
        return Ok((Vec::new(), 0));
    }
    let lower = query
        .max_id
        .map(|m| schema::desc_id(m.value()).wrapping_add(1))
        .unwrap_or(0);
    let upper_exclusive = query
        .since_id
        .or(query.min_id)
        .map(|x| schema::desc_id(x.value()));
    let want_oldest = query.min_id.is_some() && query.since_id.is_none();

    let mut matched: Vec<Status> = Vec::new();
    let mut scanned = 0usize;
    let entries = match upper_exclusive {
        Some(upper) => table.range(lower..upper)?,
        None => table.range(lower..=u64::MAX)?,
    };
    for entry in entries {
        let (key, _value) = entry?;
        scanned += 1;
        let id = Id::new(u64::MAX - key.value());
        let Some(status) = status_at(read, id)? else {
            if scanned >= scan_cap {
                break;
            }
            continue;
        };
        if accept(&status) {
            matched.push(status);
        }
        if (!want_oldest && matched.len() >= query.limit) || scanned >= scan_cap {
            break;
        }
    }
    if want_oldest && matched.len() > query.limit {
        matched.drain(..matched.len() - query.limit);
    }
    Ok((matched, scanned))
}

impl TimelineStore for RedbStore {
    fn public_timeline(
        &self,
        query: TimelineQuery,
        include_replies: bool,
        include_reblogs: bool,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage> {
        if query.limit == 0 {
            return Ok(TimelinePage::default());
        }
        let read = self.begin_read()?;
        let public = read.open_table(TIMELINE_PUBLIC)?;
        let local = read.open_table(TIMELINE_LOCAL)?;

        // Structural predicates applied before the viewer filter.
        let accept = |status: &Status| -> bool {
            if !include_replies && status.in_reply_to_id.is_some() {
                return false;
            }
            if !include_reblogs && status.is_reblog() {
                return false;
            }
            if query.only_media && status.media_ids.is_empty() {
                return false;
            }
            filter.include(status)
        };

        let (statuses, scanned) = if query.local_only {
            scan_desc_range(&read, &local, &query, PUBLIC_SCAN_CAP, accept)?
        } else if query.remote_only {
            // Remote = in the public range but not in the local range. The
            // candidate walk covers the whole (capped) window, then the page
            // keeps the newest/oldest `limit` remote matches.
            let mut out = Vec::new();
            let cloned = TimelineQuery { limit: usize::MAX, ..query };
            let (candidates, public_scanned) =
                scan_desc_range(&read, &public, &cloned, PUBLIC_SCAN_CAP, |_| true)?;
            let want_oldest = query.min_id.is_some() && query.since_id.is_none();
            for status in candidates {
                if local.get(schema::desc_id(status.id.value()))?.is_some() {
                    continue;
                }
                if accept(&status) {
                    out.push(status);
                }
                if !want_oldest && out.len() >= query.limit {
                    break;
                }
            }
            if want_oldest && out.len() > query.limit {
                out.drain(..out.len() - query.limit);
            }
            (out, public_scanned)
        } else {
            scan_desc_range(&read, &public, &query, PUBLIC_SCAN_CAP, accept)?
        };
        self.note_scanned(scanned);
        Ok(TimelinePage {
            statuses,
            scanned,
        })
    }

    fn hashtag_timeline(
        &self,
        tag: &str,
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage> {
        if query.limit == 0 || tag.is_empty() {
            return Ok(TimelinePage::default());
        }
        let read = self.begin_read()?;
        let index = read.open_table(HASHTAG_INDEX)?;

        // Key layout is `tag || 0xFF || status_id_be` (ascending ids), so the
        // bounded window is collected then reversed for newest-first order.
        let prefix: Vec<u8> = {
            let mut prefix = Vec::with_capacity(tag.len() + 9);
            prefix.extend_from_slice(tag.as_bytes());
            prefix.push(0xFF);
            prefix
        };
        let key_with = |id: u64| -> Vec<u8> {
            let mut key = prefix.clone();
            key.extend_from_slice(&id.to_be_bytes());
            key
        };
        if query.max_id.is_some_and(|m| m.value() == 0) {
            return Ok(TimelinePage::default());
        }
        let lower = query.max_id.map(|m| m.value().wrapping_add(1)).unwrap_or(0);
        let upper_exclusive = query.since_id.or(query.min_id).map(|x| x.value());
        let range_start = key_with(lower);
        let entries = match upper_exclusive {
            Some(upper) => index.range(range_start.as_slice()..key_with(upper).as_slice())?,
            None => {
                let mut end = prefix.clone();
                end.extend_from_slice(&u64::MAX.to_be_bytes());
                index.range(range_start.as_slice()..=end.as_slice())?
            }
        };
        let mut ids: Vec<Id> = Vec::new();
        for entry in entries {
            let (key, _value) = entry?;
            let bytes = key.value();
            // Key is `tag || 0xFF || id_be`: the id starts after the separator.
            let id = Id::new(u64::from_be_bytes(bytes[tag.len() + 1..].try_into().unwrap()));
            ids.push(id);
            if ids.len() >= HASHTAG_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(ids.len());
        let hashtag_scanned = ids.len();
        ids.sort_unstable_by(|a, b| b.cmp(a));

        let want_oldest = query.min_id.is_some() && query.since_id.is_none();
        let mut matched: Vec<Status> = Vec::new();
        for id in ids {
            let Some(status) = status_at(&read, id)? else {
                continue;
            };
            if query.only_media && status.media_ids.is_empty() {
                continue;
            }
            if filter.include(&status) {
                matched.push(status);
            }
            if !want_oldest && matched.len() >= query.limit {
                break;
            }
        }
        if want_oldest && matched.len() > query.limit {
            matched.drain(..matched.len() - query.limit);
        }
        Ok(TimelinePage {
            statuses: matched,
            scanned: hashtag_scanned,
        })
    }

    fn authors_timeline(
        &self,
        authors: &[Id],
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage> {
        if query.limit == 0 || authors.is_empty() {
            return Ok(TimelinePage::default());
        }
        if query.max_id.is_some_and(|m| m.value() == 0) {
            return Ok(TimelinePage::default());
        }
        let read = self.begin_read()?;
        let index = read.open_table(STATUS_AUTHOR_INDEX)?;

        let lower = query
            .max_id
            .map(|m| schema::desc_id(m.value()).wrapping_add(1))
            .unwrap_or(0);
        let upper_exclusive = query
            .since_id
            .or(query.min_id)
            .map(|x| schema::desc_id(x.value()));

        // Lazy k-way merge, newest first: one cursor per author, and a heap of
        // each cursor's head entry. Only the entries the page needs are walked,
        // so the order of `authors` never decides which posts are reached: a
        // new post from the oldest follow is found at the top of the feed. The
        // walk is bounded by `AUTHORS_MERGE_CAP` entries, counting the head of
        // every cursor.
        let want_oldest = query.min_id.is_some() && query.since_id.is_none();
        let mut cursors = Vec::with_capacity(authors.len().min(AUTHORS_CAP));
        let mut heads: BinaryHeap<(Id, usize)> = BinaryHeap::new();
        let mut scanned = 0usize;
        for author in authors.iter().take(AUTHORS_CAP) {
            let range_start: [u8; 16] = schema::pair_key(author.value(), lower);
            let mut entries = match upper_exclusive {
                Some(upper) => {
                    let hi = schema::pair_key(author.value(), upper);
                    index.range(range_start.as_slice()..hi.as_slice())?
                }
                None => {
                    let hi = schema::pair_key(author.value(), u64::MAX);
                    index.range(range_start.as_slice()..=hi.as_slice())?
                }
            };
            if let Some(entry) = entries.next() {
                let (key, _value) = entry?;
                scanned += 1;
                heads.push((merged_id(key.value()), cursors.len()));
            }
            cursors.push(entries);
        }

        let mut matched: Vec<Status> = Vec::new();
        while let Some((id, cursor)) = heads.pop() {
            if scanned < AUTHORS_MERGE_CAP {
                if let Some(entry) = cursors[cursor].next() {
                    let (key, _value) = entry?;
                    scanned += 1;
                    heads.push((merged_id(key.value()), cursor));
                }
            }
            let Some(status) = status_at(&read, id)? else {
                continue;
            };
            // Aperture (Stage 2) rides the existing `only_media` predicate:
            // home/list force it at the endpoint when the flag is on.
            // Mirrors the public/tag scans above.
            if query.only_media && status.media_ids.is_empty() {
                continue;
            }
            if filter.include(&status) {
                matched.push(status);
            }
            if !want_oldest && matched.len() >= query.limit {
                break;
            }
        }
        self.note_scanned(scanned);
        if want_oldest && matched.len() > query.limit {
            matched.drain(..matched.len() - query.limit);
        }
        Ok(TimelinePage {
            statuses: matched,
            scanned,
        })
    }
}

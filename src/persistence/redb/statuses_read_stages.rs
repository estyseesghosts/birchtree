//! Stages for `statuses_by_account_impl` (in `statuses_read.rs`).
//!
//! The pinned-only path (pin records first, then rows in pin order) and the
//! general author-index path are independent scans sharing only the
//! limit-zero guard and the read transaction; splitting them keeps the
//! orchestrator at a glance. Both are read-only stages taking the open
//! `ReadTransaction` (plus `&RedbStore` where scan accounting applies).

use redb::ReadTransaction;

use crate::application::ports::AccountStatusesQuery;
use crate::domain::{Id, Result, Status};

use super::schema::{self, STATUS_AUTHOR_INDEX, STATUS_PINS, STATUSES};
use super::statuses_interactions::PinRecord;
use super::statuses_read::{AUTHOR_SCAN_CAP, PINS_SCAN_CAP};
use super::{RedbStore, decode};

/// Pin records first (pin order, newest pin first), then rows. Cursor
/// `max_id` is exclusive; deleted and missing rows are skipped silently.
pub(super) fn pinned_statuses(
    read: &ReadTransaction,
    account_id: Id,
    query: &AccountStatusesQuery,
) -> Result<Vec<Status>> {
    let mut pinned: Vec<(u64, Id)> = Vec::new();
    {
        let pins = read.open_table(STATUS_PINS)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        for entry in pins.range(lo.as_slice()..=hi.as_slice())? {
            let (key, value) = entry?;
            let bytes = key.value();
            let status_id = Id::new(u64::from_be_bytes(bytes[8..16].try_into().unwrap()));
            let record: PinRecord = decode(value.value())?;
            pinned.push((record.pinned_at_ms, status_id));
            if pinned.len() >= PINS_SCAN_CAP {
                break;
            }
        }
    }
    pinned.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    let mut out = Vec::new();
    let statuses = read.open_table(STATUSES)?;
    for (_, status_id) in pinned {
        if let Some(m) = query.max_id
            && status_id >= m
        {
            continue;
        }
        let Some(guard) = statuses.get(status_id.value())? else {
            continue;
        };
        let status: Status = decode(guard.value())?;
        if status.is_deleted() {
            continue;
        }
        out.push(status);
        if out.len() >= query.limit {
            break;
        }
    }
    Ok(out)
}

/// Author index: keys are `(account || desc(id))`, so a forward scan yields
/// status ids descending. With `min_id` (and no `since_id`) the oldest
/// matching ids are wanted, so keep scanning and retain the last `limit`
/// matches instead of stopping at the first `limit`.
pub(super) fn general_statuses(
    read: &ReadTransaction,
    store: &RedbStore,
    account_id: Id,
    query: &AccountStatusesQuery,
) -> Result<Vec<Status>> {
    let want_oldest = query.min_id.is_some() && query.since_id.is_none();
    let mut matched: Vec<Status> = Vec::new();
    {
        let index = read.open_table(STATUS_AUTHOR_INDEX)?;
        let statuses = read.open_table(STATUSES)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut scanned = 0usize;
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            if scanned >= AUTHOR_SCAN_CAP {
                break;
            }
            scanned += 1;
            let (key, _value) = entry?;
            let bytes = key.value();
            let desc = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
            let id = Id::new(u64::MAX - desc);
            if query.max_id.is_some_and(|m| id >= m)
                || query.since_id.is_some_and(|s| id <= s)
                || query.min_id.is_some_and(|m| id <= m)
            {
                continue;
            }
            let Some(guard) = statuses.get(id.value())? else {
                continue;
            };
            let status: Status = decode(guard.value())?;
            if status.is_deleted() {
                continue;
            }
            if query.exclude_replies && status.in_reply_to_id.is_some() {
                continue;
            }
            if query.exclude_reblogs && status.is_reblog() {
                continue;
            }
            matched.push(status);
            if !want_oldest && matched.len() >= query.limit {
                break;
            }
        }
        store.note_scanned(scanned);
    }
    if want_oldest && matched.len() > query.limit {
        matched.drain(..matched.len() - query.limit);
    }
    Ok(matched)
}

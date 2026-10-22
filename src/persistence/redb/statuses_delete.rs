//! Status deletion: tombstones, reblog/quote cascades and counter fixes.
//!
//! `delete_status_impl` opens ONE `redb` write transaction; the cascade
//! stages each take that transaction:
//!
//! * [`cascade_reblogs_txn`] — tombstone every reblog wrapper, fix authors.
//! * [`cascade_quotes_txn`] — flip quotes of the status to `deleted`.
//!
//! Helpers owned here (`read_quote_txn`, `write_quote_txn`,
//! `clear_poll_votes`, `newest_live_status_at`) are imported by the sibling
//! `statuses_*` modules (`clear_poll_votes` is used by the edit patch);
//! nothing is duplicated.

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::DeletedStatus;
use crate::domain::{Error, Id, Job, JobKind, QuoteRecord, QuoteState, Result, Status};

use super::schema::{
    self, QUOTES, QUOTES_BY_QUOTED, REBLOGS_BY_ACCOUNT, STATUS_PINS, STATUS_REBLOG_INDEX,
};
use super::statuses_create::{read_stats_txn, write_stats_txn};
use super::statuses_read::{
    CASCADE_SCAN_CAP, LAST_STATUS_SCAN_CAP, read_status_txn, write_status_txn,
};
use super::{RedbStore, StoreResult, decode, encode};

/// Read a quote row inside an open write transaction.
pub(super) fn read_quote_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<QuoteRecord>> {
    let table = write.open_table(QUOTES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Write (upsert) a quote row inside an open write transaction.
pub(super) fn write_quote_txn(write: &WriteTransaction, quote: &QuoteRecord) -> StoreResult<()> {
    let mut table = write.open_table(QUOTES)?;
    table.insert(quote.id.value(), encode(quote)?.as_slice())?;
    Ok(())
}

/// Remove every ballot of a poll (prefix scan collected first, so the table is
/// not iterated while it is mutated).
pub(super) fn clear_poll_votes(write: &WriteTransaction, poll_id: Id) -> StoreResult<()> {
    let keys: Vec<Vec<u8>> = {
        let votes = write.open_table(super::schema::POLL_VOTES)?;
        let lo = schema::pair_key(poll_id.value(), 0);
        let hi = schema::pair_key(poll_id.value(), u64::MAX);
        let mut keys = Vec::new();
        for entry in votes.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            keys.push(key.value().to_vec());
        }
        keys
    };
    if keys.is_empty() {
        return Ok(());
    }
    let mut votes = write.open_table(super::schema::POLL_VOTES)?;
    for key in &keys {
        votes.remove(key.as_slice())?;
    }
    Ok(())
}

/// Newest `created_at_ms` among the account's non-deleted statuses.
///
/// The author index yields ids in descending order, so the first live row
/// found is the answer; the scan is bounded to `LAST_STATUS_SCAN_CAP` entries.
pub(super) fn newest_live_status_at(
    write: &WriteTransaction,
    account_id: Id,
) -> StoreResult<Option<u64>> {
    let index = write.open_table(super::schema::STATUS_AUTHOR_INDEX)?;
    let lo = schema::pair_key(account_id.value(), 0);
    let hi = schema::pair_key(account_id.value(), u64::MAX);
    let mut scanned = 0usize;
    for entry in index.range(lo.as_slice()..=hi.as_slice())? {
        if scanned >= LAST_STATUS_SCAN_CAP {
            break;
        }
        scanned += 1;
        let (_key, value) = entry?;
        let Some(status) = read_status_txn(write, Id::new(value.value()))? else {
            continue;
        };
        if !status.is_deleted() {
            return Ok(Some(status.created_at_ms));
        }
    }
    Ok(None)
}

/// Tombstone every reblog of the status and fix each wrapper author's
/// counter. Returns the wrapper ids and their pre-deletion snapshots.
pub(super) fn cascade_reblogs_txn(
    write: &WriteTransaction,
    status_id: Id,
    now_ms: u64,
) -> Result<(Vec<Id>, Vec<Status>)> {
    // Tombstone every reblog of this status and fix its author's counter.
    // Each wrapper's pre-deletion snapshot is returned for streaming.
    let mut reblog_ids: Vec<Id> = Vec::new();
    let mut cascaded: Vec<Status> = Vec::new();
    {
        let index = write.open_table(STATUS_REBLOG_INDEX)?;
        let lo = schema::pair_key(status_id.value(), 0);
        let hi = schema::pair_key(status_id.value(), u64::MAX);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            reblog_ids.push(Id::new(value.value()));
            if reblog_ids.len() >= CASCADE_SCAN_CAP {
                break;
            }
        }
    }
    for reblog_id in &reblog_ids {
        let Some(mut reblog) = read_status_txn(write, *reblog_id)? else {
            continue;
        };
        if reblog.is_deleted() {
            continue;
        }
        cascaded.push(reblog.clone());
        reblog.deleted_at_ms = Some(now_ms);
        write_status_txn(write, &reblog)?;
        let mut stats = read_stats_txn(write, reblog.account_id)?;
        stats.statuses_count = stats.statuses_count.saturating_sub(1);
        write_stats_txn(write, reblog.account_id, &stats)?;
    }
    Ok((reblog_ids, cascaded))
}

/// Flip every quote of the status to `deleted`.
pub(super) fn cascade_quotes_txn(write: &WriteTransaction, status_id: Id) -> Result<()> {
    // Quotes of this status flip to `deleted`.
    let mut quote_ids: Vec<Id> = Vec::new();
    {
        let index = write.open_table(QUOTES_BY_QUOTED)?;
        let lo = schema::pair_key(status_id.value(), 0);
        let hi = schema::pair_key(status_id.value(), u64::MAX);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            quote_ids.push(Id::new(value.value()));
            if quote_ids.len() >= CASCADE_SCAN_CAP {
                break;
            }
        }
    }
    for quote_id in quote_ids {
        let Some(mut quote) = read_quote_txn(write, quote_id)? else {
            continue;
        };
        quote.state = QuoteState::Deleted;
        write_quote_txn(write, &quote)?;
    }
    Ok(())
}

/// Tombstone a status in ONE transaction (tombstone + dependent reblogs +
/// pin removal + parent/author counter fixes + follow-up job). Returns the
/// pre-deletion snapshots: the status and every reblog wrapper the cascade
/// tombstoned (streaming needs them to send `delete` events). `NotFound`
/// when missing, deleted, or not owned.
pub(super) fn delete_status_impl(
    store: &RedbStore,
    id: Id,
    account_id: Id,
    now_ms: u64,
) -> Result<DeletedStatus> {
    let write = store.begin_write()?;

    let snapshot = read_status_txn(&write, id)?
        .filter(|s| !s.is_deleted() && s.account_id == account_id)
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    // Tombstone first, so the author scan below skips this status.
    let mut status = snapshot.clone();
    status.deleted_at_ms = Some(now_ms);
    write_status_txn(&write, &status)?;

    // Parent reply counter.
    if let Some(parent_id) = status.in_reply_to_id
        && let Some(mut parent) = read_status_txn(&write, parent_id)?
    {
        parent.replies_count = parent.replies_count.saturating_sub(1);
        write_status_txn(&write, &parent)?;
    }

    let (reblog_ids, cascaded) = cascade_reblogs_txn(&write, id, now_ms)?;

    // Drop the author's pin of this status, if any.
    {
        let mut pins = write.open_table(STATUS_PINS)?;
        pins.remove(schema::pair_key(account_id.value(), id.value()).as_slice())?;
    }

    cascade_quotes_txn(&write, id)?;

    // Author counters: decrement and recompute `last_status_at` from the
    // newest surviving status (bounded scan).
    let mut stats = read_stats_txn(&write, account_id)?;
    stats.statuses_count = stats.statuses_count.saturating_sub(1);
    stats.last_status_at_ms = newest_live_status_at(&write, account_id)?;
    write_stats_txn(&write, account_id, &stats)?;

    super::federation_content::federation_event_txn(
        &write,
        store.next_id(),
        account_id,
        serde_json::json!({"event": "delete", "status_id": id.value()}),
        now_ms,
    )?;
    // Mastodon parity (item 52): deleting a reblog wrapper must also send
    // `Undo(Announce)` so the origin decrements its reblog count. The
    // `delete` above is kept for followers' timelines.
    if let Some(target_id) = snapshot.reblog_of_id {
        write
            .open_table(REBLOGS_BY_ACCOUNT)?
            .remove(schema::pair_key(account_id.value(), target_id.value()).as_slice())?;
        super::federation_content::federation_event_txn(
            &write,
            store.next_id(),
            account_id,
            serde_json::json!({"event": "undo_announce", "status_id": target_id.value(), "announce_uri": snapshot.uri.clone()}),
            now_ms,
        )?;
    }
    // Phase 7.2: incremental search indexing rides the same commit
    // (tombstoned rows and their cascaded reblogs leave the index).
    super::jobs::insert_job(
        &write,
        &Job::new(
            store.next_id(),
            JobKind::SearchIndexStatus,
            crate::jobs::search_index::index_payload(id),
            now_ms,
            now_ms,
        ),
    )?;
    for reblog_id in &reblog_ids {
        super::jobs::insert_job(
            &write,
            &Job::new(
                store.next_id(),
                JobKind::SearchIndexStatus,
                crate::jobs::search_index::index_payload(*reblog_id),
                now_ms,
                now_ms,
            ),
        )?;
    }
    write.commit()?;
    Ok(DeletedStatus {
        status: snapshot,
        reblogs: cascaded,
    })
}

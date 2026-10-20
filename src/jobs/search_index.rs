//! Phase 7.2 post-commit search-index jobs.
//!
//! Indexing is post-commit durable work: the status mutation commits first
//! (record + indexes + counters + this job, atomically), and the worker
//! applies the job afterwards. Every function here is idempotent — a retried
//! job re-reads the current row and converges the index to it — so crash
//! recovery (lease expiry → reclaim → retry) can never corrupt derived
//! state, and search reads never mutate.
//!
//! Wiring (PATCHES FOR PARENT, applied before this module is reachable):
//!
//! - `domain::job::JobKind` gains `SearchIndexStatus` (`"search_index_status"`)
//!   and `SearchBackfill` (`"search_backfill"`).
//! - `jobs/mod.rs` gains `pub mod search_index;`.
//! - `jobs/handlers.rs` dispatches both kinds to [`apply_global`] / the
//!   `run_backfill` chain (exact arms in the handoff report).
//! - `persistence/redb/statuses.rs` enqueues one `SearchIndexStatus` job in
//!   the same transaction as every create / edit-commit / delete (exact
//!   insertion points in the handoff report).
//!
//! This module deliberately names no `JobKind`: it compiles before and after
//! the parent's enum patch, and the handler (a parent-owned file) owns the
//! `store.enqueue(JobKind::…)` calls via the `enqueue_next` callback.

use crate::application::ports::Store;
use crate::domain::{Error, Id, Result};
use crate::search::{EmbeddedIndex, backfill_batch, global, indexable_text, is_indexable};

/// Build a `SearchIndexStatus` payload for one status.
pub fn index_payload(status_id: Id) -> serde_json::Value {
    serde_json::json!({ "status_id": status_id.value() })
}

/// Parse a `SearchIndexStatus` payload.
pub fn parse_index_payload(payload: &serde_json::Value) -> Result<Id> {
    payload
        .get("status_id")
        .and_then(|v| v.as_u64())
        .map(Id::new)
        .ok_or_else(|| Error::Internal("search_index_status: missing status_id".into()))
}

/// Apply one `SearchIndexStatus` job to `index`.
///
/// Re-reads the current row and converges the index to it: indexable rows
/// are (re-)indexed with their current text, everything else (deleted,
/// private, direct, or missing rows) is removed. Create, edit, delete and
/// visibility-change all funnel through this one step, so out-of-order
/// delivery still converges on a re-read — the last applied job wins, and a
/// retry simply re-applies the current row.
pub fn apply(store: &dyn Store, index: &EmbeddedIndex, payload: &serde_json::Value) -> Result<()> {
    let id = parse_index_payload(payload)?;
    match store.status_by_id(id)? {
        Some(status) if is_indexable(&status) => {
            index.index_status(id, &indexable_text(&status));
        }
        _ => {
            index.remove_status(id);
        }
    }
    Ok(())
}

/// Apply one `SearchIndexStatus` job to the process-wide index (the worker
/// path; the worker only receives the store).
pub fn apply_global(store: &dyn Store, payload: &serde_json::Value) -> Result<()> {
    apply(store, global(), payload)
}

/// Build a `SearchBackfill` payload: resume below `cursor` (`None` starts at
/// the newest row) with `batch` rows per step.
pub fn backfill_payload(cursor: Option<Id>, batch: usize) -> serde_json::Value {
    serde_json::json!({ "cursor": cursor.map(|c| c.value()), "batch": batch })
}

/// Parse a `SearchBackfill` payload.
pub fn parse_backfill_payload(payload: &serde_json::Value) -> Result<(Option<Id>, usize)> {
    let batch = payload
        .get("batch")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| Error::Internal("search_backfill: missing batch".into()))?;
    if batch == 0 {
        return Err(Error::Internal("search_backfill: batch must be > 0".into()));
    }
    let cursor = match payload.get("cursor") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .map(Id::new)
                .ok_or_else(|| Error::Internal("search_backfill: bad cursor".into()))?,
        ),
    };
    Ok((cursor, batch))
}

/// Run one `SearchBackfill` step against `index`.
///
/// Indexes one bounded batch, then hands the next payload to `enqueue_next`
/// (the handler enqueues it as the follow-up `SearchBackfill` job, due
/// immediately). Returns `true` when the corpus is covered (no follow-up).
/// A crash between batches loses nothing durable: the next step resumes
/// from its payload cursor, and re-indexing is idempotent. Note the
/// in-memory caveat: a process restart wipes the index itself, so startup
/// re-seeds the chain from `None` when the index is empty (parent patch in
/// `main.rs`); the chain cursor then paces batching within the run.
pub fn run_backfill(
    store: &dyn Store,
    index: &EmbeddedIndex,
    payload: &serde_json::Value,
    now_ms: u64,
    enqueue_next: &mut dyn FnMut(serde_json::Value, u64) -> Result<()>,
) -> Result<bool> {
    let (cursor, batch) = parse_backfill_payload(payload)?;
    let progress = backfill_batch(store, index, cursor, batch)?;
    if progress.done {
        return Ok(true);
    }
    enqueue_next(backfill_payload(progress.next_cursor, batch), now_ms)?;
    Ok(false)
}

/// Run one backfill step against the process-wide index (the worker path).
pub fn run_backfill_global(
    store: &dyn Store,
    payload: &serde_json::Value,
    now_ms: u64,
    enqueue_next: &mut dyn FnMut(serde_json::Value, u64) -> Result<()>,
) -> Result<bool> {
    run_backfill(store, global(), payload, now_ms, enqueue_next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_round_trip() {
        let id = Id::new(12345);
        assert_eq!(parse_index_payload(&index_payload(id)).unwrap(), id);
        assert!(parse_index_payload(&serde_json::json!({})).is_err());

        let payload = backfill_payload(Some(id), 250);
        assert_eq!(parse_backfill_payload(&payload).unwrap(), (Some(id), 250));
        let fresh = backfill_payload(None, 500);
        assert_eq!(parse_backfill_payload(&fresh).unwrap(), (None, 500));
        assert!(parse_backfill_payload(&serde_json::json!({"batch": 0})).is_err());
        assert!(parse_backfill_payload(&serde_json::json!({})).is_err());
    }
}

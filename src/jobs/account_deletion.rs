//! Async account-deletion worker (D38).
//!
//! Upstream (`AccountDeletionWorker` + `DeleteAccountService`, Mastodon
//! v4.7.3) removes a deleted account's statuses in batches and distributes a
//! federated `Delete` of the actor to the account's followers. This module is
//! the bounded, restartable equivalent:
//!
//! - One [`crate::domain::JobKind::AccountDeletionBatch`] job carries
//!   `{account_id, after}`, where `after` is the exclusive resume cursor
//!   (`None` starts at the newest status).
//! - Each batch tombstones at most [`DELETION_BATCH`] statuses through the
//!   normal [`crate::application::ports::StatusStore::delete_status`] path,
//!   so every tombstone commits its own federated `Delete` event (a
//!   `Tombstone` of the Note) and its search-index removal in the same
//!   transaction as the tombstone itself.
//! - While a full page was seen, the batch enqueues its follow-up with the
//!   last id as the cursor. A batch that crashes mid-page is retried from
//!   its own cursor (the job lease makes it claimable again); tombstones
//!   are idempotent — an already-gone status answers `NotFound` and is
//!   skipped — so replays never duplicate work and never skip a status.
//! - A non-`NotFound` store error fails the batch: the job backs off with
//!   the standard retry budget and ends quarantined, never looping forever.

use crate::application::ports::{AccountStatusesQuery, Store, StreamingSink};
use crate::application::streaming::StreamEvent;
use crate::domain::{Error, Id, JobKind, Result};

/// Statuses tombstoned per batch (upstream's `BatchedRemoveStatusService`
/// deletes in slices of 50 to avoid a long-running transaction lock).
pub const DELETION_BATCH: usize = 50;

/// Build an `AccountDeletionBatch` payload.
pub fn payload(account_id: Id, after: Option<Id>) -> serde_json::Value {
    serde_json::json!({
        "account_id": account_id.value(),
        "after": after.map(|id| id.value()),
    })
}

/// Parse an `AccountDeletionBatch` payload.
pub fn parse_payload(payload: &serde_json::Value) -> Result<(Id, Option<Id>)> {
    let account_id = payload
        .get("account_id")
        .and_then(|v| v.as_u64())
        .map(Id::new)
        .ok_or_else(|| Error::Internal("account_deletion_batch: missing account_id".into()))?;
    let after = payload.get("after").and_then(|v| v.as_u64()).map(Id::new);
    Ok((account_id, after))
}

/// Run one deletion batch: tombstone up to [`DELETION_BATCH`] of the
/// account's live statuses and chain the follow-up while a full page was
/// seen. Returns the number of statuses tombstoned by this batch.
pub fn run_batch(
    store: &dyn Store,
    streaming: &dyn StreamingSink,
    payload_data: &serde_json::Value,
    now_ms: u64,
) -> Result<usize> {
    let (account_id, after) = parse_payload(payload_data)?;
    let page = store.statuses_by_account(
        account_id,
        AccountStatusesQuery {
            max_id: after,
            since_id: None,
            min_id: None,
            limit: DELETION_BATCH,
            pinned_only: false,
            exclude_replies: false,
            exclude_reblogs: false,
        },
    )?;
    let mut tombstoned = 0usize;
    for status in &page {
        match store.delete_status(status.id, account_id, now_ms) {
            Ok(deleted) => {
                tombstoned += 1;
                streaming.publish(StreamEvent::StatusDeleted {
                    status: deleted.status,
                    reblogs: deleted.reblogs,
                });
            }
            Err(Error::NotFound(_)) => {
                // Gone between the page read and the tombstone (a concurrent
                // delete or a retried batch): already the desired end state.
            }
            Err(err) => return Err(err),
        }
    }
    // A full page may hide more statuses: chain the follow-up from the last
    // id seen. A short page ends the chain.
    if page.len() >= DELETION_BATCH
        && let Some(last) = page.last()
    {
        store.enqueue(JobKind::AccountDeletionBatch, payload(account_id, Some(last.id)), now_ms)?;
    }
    Ok(tombstoned)
}

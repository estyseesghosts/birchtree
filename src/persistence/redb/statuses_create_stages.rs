//! Job-family stages for the status-creation transaction.
//!
//! `create_status_impl` (in `statuses_create.rs`) opens ONE `redb` write
//! transaction; `enqueue_create_jobs_txn` is its in-transaction fragment
//! (not a single-transaction body itself, so it cannot be grandfathered).
//! This module splits that fragment by job family, in the original relative
//! order (quote fan-out, reply fan-out, mention fan-outs, follower fan-out,
//! federation event, search-index job) plus the idempotency record. Every
//! stage takes `&WriteTransaction` first and runs inside the caller's
//! transaction; `store` is used only to mint generated ids and for
//! read-only follower scans.

use redb::WriteTransaction;

use crate::application::ports::{NewStatus, RelationshipStore};
use crate::domain::{Id, IdempotencyRecord, Job, JobKind, NotificationType, Result, Visibility};

use super::schema::IDEMPOTENCY;
use super::statuses_create::{PreparedCreate, enqueue_fan_out, idempotency_key};
use super::{RedbStore, encode};

/// Quote fan-out, reply fan-out and per-mention fan-outs (original relative
/// order: quoted author first as `Quote`, then parent author and mentions
/// as `Mention`; self-mentions journal nothing).
pub(super) fn fan_out_quote_reply_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    input: &NewStatus,
    prepared: &PreparedCreate,
    now_ms: u64,
) -> Result<()> {
    let status_id = prepared.status_id;

    if let Some(recipient) = prepared.quoted_account {
        enqueue_fan_out(
            store,
            write,
            recipient,
            NotificationType::Quote,
            Some(input.account_id),
            Some(status_id),
            now_ms,
        )?;
    }
    if let Some(recipient) = prepared.reply_notify {
        enqueue_fan_out(
            store,
            write,
            recipient,
            NotificationType::Mention,
            Some(input.account_id),
            Some(status_id),
            now_ms,
        )?;
    }
    for mentioned in &prepared.mentions {
        if *mentioned != input.account_id {
            enqueue_fan_out(
                store,
                write,
                *mentioned,
                NotificationType::Mention,
                Some(input.account_id),
                Some(status_id),
                now_ms,
            )?;
        }
    }
    Ok(())
}

/// D51 `status`: followers with `notify` set are fanned out in this same
/// transaction (G-B producer). Non-Direct only: followers cannot see direct
/// posts (upstream's exact visibility gate is unverified — revisit against
/// `fan_out_on_write_service.rb`). `store` is read-only here (follower
/// scans) plus generated-id minting.
pub(super) fn fan_out_followers_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    input: &NewStatus,
    status_id: Id,
    now_ms: u64,
) -> Result<()> {
    if input.visibility != Visibility::Direct {
        use crate::application::notifications::emit_status_for_followers;
        let mut notify: Vec<Id> = Vec::new();
        let mut cursor: Option<Id> = None;
        for _ in 0..20 {
            let batch = store.followers(input.account_id, cursor, 500)?;
            if batch.is_empty() {
                break;
            }
            for follower in &batch {
                cursor = Some(*follower);
                if store
                    .follow_of(*follower, input.account_id)?
                    .is_some_and(|f| f.notify)
                {
                    notify.push(*follower);
                }
            }
            if batch.len() < 500 {
                break;
            }
        }
        for job in emit_status_for_followers(input.account_id, status_id, &notify, now_ms) {
            super::jobs::insert_job(
                write,
                &Job::new(store.next_id(), job.kind, job.payload, now_ms, now_ms),
            )?;
        }
    }
    Ok(())
}

/// Federation event plus the Phase 7.2 incremental search-index job, in
/// the original relative order.
pub(super) fn federation_search_txn(
    store: &RedbStore,
    write: &WriteTransaction,
    account_id: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<()> {
    super::federation_content::federation_event_txn(
        write,
        store.next_id(),
        account_id,
        serde_json::json!({"event": "create", "status_id": status_id.value()}),
        now_ms,
    )?;
    // Phase 7.2: incremental search indexing rides the same commit.
    super::jobs::insert_job(
        write,
        &Job::new(
            store.next_id(),
            JobKind::SearchIndexStatus,
            crate::jobs::search_index::index_payload(status_id),
            now_ms,
            now_ms,
        ),
    )?;
    Ok(())
}

/// Idempotency record, committed with the mutation it protects.
pub(super) fn write_idempotency_txn(
    write: &WriteTransaction,
    input: &NewStatus,
    status_id: Id,
    now_ms: u64,
) -> Result<()> {
    if let Some(raw) = input.idempotency_key.as_deref() {
        let key = idempotency_key(input.account_id, raw)?;
        let record = IdempotencyRecord {
            created_at_ms: now_ms,
            scope: "status_create".to_string(),
            result_id: status_id,
        };
        let mut table = write.open_table(IDEMPOTENCY)?;
        table.insert(key.as_slice(), encode(&record)?.as_slice())?;
    }
    Ok(())
}

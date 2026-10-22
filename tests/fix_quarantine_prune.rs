//! Quarantine retention (prod-2 item 8).
//!
//! Quarantine is pruned for real: `prune_quarantined` deletes old
//! quarantined rows (oldest first, bounded batch) from the maintenance tick,
//! pending/leased/succeeded rows are never touched, oversize payloads are
//! quarantined on insert with a recorded reason (never stored unbounded),
//! and prune is delete-only.

mod common;

use birchtree::application::ports::JobQueue;
use birchtree::domain::{JobKind, JobState};
use birchtree::jobs::worker::{
    QUARANTINE_MAX_PAYLOAD_BYTES, QUARANTINE_PRUNE_BATCH, run_table_prunes,
};

use common::temp_store;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

/// Old quarantined rows prune oldest-first within the batch; live and
/// succeeded rows are never eligible, and fresh quarantined rows are kept.
#[test]
fn prune_quarantined_is_oldest_first_bounded_and_terminal_only() {
    let (_dir, store) = temp_store();
    let now = now_ms();
    // Five old quarantined rows (due 1000..5000), one fresh.
    for due in [1000u64, 2000, 3000, 4000, 5000] {
        let job = store
            .enqueue_at(JobKind::DeliverActivity, serde_json::json!({}), due, now)
            .expect("enqueue");
        store.quarantine(job.id, "refused").expect("quarantine");
    }
    let fresh = store
        .enqueue(JobKind::DeliverActivity, serde_json::json!({}), now)
        .expect("enqueue");
    store.quarantine(fresh.id, "fresh").expect("quarantine");
    // Live rows: pending + leased + succeeded.
    let pending = store
        .enqueue(JobKind::DeliverActivity, serde_json::json!({}), now)
        .expect("pending");
    let leased = store
        .enqueue(JobKind::DeliverActivity, serde_json::json!({}), now)
        .expect("leased");
    let claimed = store.claim_due(now + 1, 10).expect("claim");
    assert!(claimed.iter().any(|job| job.id == leased.id));
    let done = store
        .enqueue(JobKind::DeliverActivity, serde_json::json!({}), now)
        .expect("done");
    store.complete(done.id).expect("complete");

    // Bounded pass of two reaps the two oldest; nothing else moves.
    let removed = store.prune_quarantined(6000, 2).expect("prune");
    assert_eq!(removed, 2);
    let remaining: Vec<u64> = store
        .all_jobs()
        .expect("jobs")
        .iter()
        .filter(|job| job.state == JobState::Quarantined)
        .map(|job| job.due_at_ms)
        .collect();
    assert_eq!(remaining.len(), 4);
    assert!(!remaining.contains(&1000) && !remaining.contains(&2000));
    for id in [pending.id, leased.id, done.id, fresh.id] {
        assert!(
            store.get(id).expect("get").is_some(),
            "non-victim {id} survives"
        );
    }

    // Fresh quarantined rows are kept while the threshold precedes them...
    // and go once it passes them.
    assert_eq!(store.prune_quarantined(fresh.due_at_ms, 100).expect("prune"), 3);
    assert_eq!(
        store
            .prune_quarantined(fresh.due_at_ms + 1, 100)
            .expect("prune"),
        1
    );
    assert!(
        store
            .all_jobs()
            .expect("jobs")
            .iter()
            .all(|job| job.state != JobState::Quarantined),
        "quarantine fully aged out"
    );
}

/// One maintenance pass over more than the batch cap removes exactly one
/// batch of the oldest quarantined rows and leaves pending work untouched.
#[test]
fn maintenance_pass_prunes_one_batch_and_keeps_pending() {
    let (_dir, store) = temp_store();
    let now = now_ms();
    // More than the batch cap, all long past retention.
    for _ in 0..QUARANTINE_PRUNE_BATCH + 5 {
        let job = store
            .enqueue_at(JobKind::DeliverActivity, serde_json::json!({}), 1, now)
            .expect("enqueue");
        store.quarantine(job.id, "stale").expect("quarantine");
    }
    // Pending work plus a fresh succeeded row.
    for _ in 0..5 {
        store
            .enqueue(JobKind::DeliverActivity, serde_json::json!({}), now)
            .expect("pending");
    }
    let done = store
        .enqueue(JobKind::DeliverActivity, serde_json::json!({}), now)
        .expect("done");
    store.complete(done.id).expect("complete");

    run_table_prunes(&*store, now, 6 * 3_600_000);

    let jobs = store.all_jobs().expect("jobs");
    let quarantined = jobs
        .iter()
        .filter(|job| job.state == JobState::Quarantined)
        .count();
    assert_eq!(quarantined, 5, "one batch reaped, five remain");
    assert_eq!(
        jobs.iter()
            .filter(|job| job.state == JobState::Pending)
            .count(),
        5,
        "pending work untouched"
    );
    assert!(
        jobs.iter().any(|job| job.id == done.id),
        "fresh succeeded row untouched"
    );
}

/// An oversize payload never lands in the store: the row is quarantined on
/// insert with a recorded reason and a bounded marker, and it holds no
/// scheduling-index entry (it can never run).
#[test]
fn oversize_payload_is_quarantined_on_insert() {
    let (_dir, store) = temp_store();
    let now = now_ms();
    let big = serde_json::json!({"blob": "y".repeat(QUARANTINE_MAX_PAYLOAD_BYTES + 1)});
    let job = store
        .enqueue(JobKind::DeliverActivity, big, now)
        .expect("enqueue");
    assert_eq!(job.state, JobState::Quarantined);
    let reason = job.last_error.expect("recorded reason");
    assert!(reason.contains("exceeded"), "{reason}");
    assert_eq!(
        job.payload,
        serde_json::json!({
            "truncated": true,
            "bytes": QUARANTINE_MAX_PAYLOAD_BYTES + 1 + serde_json::json!({"blob": ""}).to_string().len(),
            "kept_bytes": 0,
        })
    );
    // No index entry: the worker can never claim it.
    assert!(
        store
            .claim_due(now + 1, 100)
            .expect("claim")
            .iter()
            .all(|claimed| claimed.id != job.id)
    );

    // Control: a bounded payload still enqueues pending.
    let small = store
        .enqueue(JobKind::DeliverActivity, serde_json::json!({"a": 1}), now)
        .expect("enqueue");
    assert_eq!(small.state, JobState::Pending);
}

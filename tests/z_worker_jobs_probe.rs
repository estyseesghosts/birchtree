//! Probe for background-job retention (`JobQueue::prune_succeeded`).
//!
//! Guards the retention contract: only old `Succeeded` rows are deleted,
//! the prune is bounded, and `complete()` still leaves a `Succeeded` row
//! behind for `get`.

mod common;

use birchtree::application::ports::JobQueue;
use birchtree::domain::{JobKind, JobState};
use birchtree::jobs::worker::{SUCCEEDED_PRUNE_BATCH, SUCCEEDED_RETENTION_MS};

use common::{T0, temp_store};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// Far enough past `T0` that old rows are unambiguously past retention.
const NOW: u64 = T0 + 30 * DAY_MS;
const CUTOFF: u64 = NOW - SUCCEEDED_RETENTION_MS;

fn complete_at(
    store: &birchtree::persistence::redb::RedbStore,
    at_ms: u64,
) -> birchtree::domain::Job {
    let job = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), at_ms)
        .expect("enqueue probe job");
    let claimed = store.claim_due(at_ms, 10).expect("claim probe job");
    assert!(
        claimed.iter().any(|claimed| claimed.id == job.id),
        "probe job must be claimable at enqueue time"
    );
    store
        .complete(job.id, job.due_at_ms)
        .expect("complete probe job");
    store
        .get(job.id)
        .expect("get probe job")
        .expect("probe job row")
}

#[test]
fn prune_deletes_only_old_succeeded_rows() {
    let (_dir, store) = temp_store();

    let old = complete_at(&store, T0);
    let recent = complete_at(&store, NOW);
    let pending = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .expect("enqueue pending probe job");
    let quarantined = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .expect("enqueue quarantined probe job");
    store
        .quarantine(
            quarantined.id,
            "operator looks at this",
            quarantined.due_at_ms,
        )
        .expect("quarantine probe job");

    let removed = store
        .prune_succeeded(CUTOFF, SUCCEEDED_PRUNE_BATCH)
        .expect("prune succeeded");
    assert_eq!(removed, 1, "only the old Succeeded row is eligible");

    assert!(
        store.get(old.id).expect("get old").is_none(),
        "old Succeeded row must be gone"
    );
    assert_eq!(
        store
            .get(recent.id)
            .expect("get recent")
            .expect("recent row")
            .state,
        JobState::Succeeded,
        "recent Succeeded row must be kept"
    );
    assert_eq!(
        store
            .get(pending.id)
            .expect("get pending")
            .expect("pending row")
            .state,
        JobState::Pending,
        "Pending rows are live work and must be kept"
    );
    assert_eq!(
        store
            .get(quarantined.id)
            .expect("get quarantined")
            .expect("quarantined row")
            .state,
        JobState::Quarantined,
        "Quarantined rows are the operator surface and must be kept"
    );
}

#[test]
fn prune_limit_is_honored() {
    let (_dir, store) = temp_store();

    for _ in 0..5 {
        complete_at(&store, T0);
    }
    assert_eq!(
        store.prune_succeeded(CUTOFF, 0).expect("zero-limit prune"),
        0,
        "a zero limit must remove nothing"
    );
    assert_eq!(
        store.all_jobs().expect("list jobs").len(),
        5,
        "zero-limit prune must keep every row"
    );
    assert_eq!(
        store.prune_succeeded(CUTOFF, 2).expect("capped prune"),
        2,
        "a capped pass removes at most the limit"
    );
    assert_eq!(
        store.all_jobs().expect("list jobs").len(),
        3,
        "three old Succeeded rows must remain"
    );
    assert_eq!(
        store
            .prune_succeeded(CUTOFF, SUCCEEDED_PRUNE_BATCH)
            .expect("drain prune"),
        3,
        "a follow-up pass reaps the rest"
    );
    assert!(
        store.all_jobs().expect("list jobs").is_empty(),
        "no Succeeded rows must remain"
    );
}

#[test]
fn get_after_complete_still_succeeded() {
    let (_dir, store) = temp_store();

    let done = complete_at(&store, NOW);
    assert_eq!(done.state, JobState::Succeeded);
    assert!(
        done.due_at_ms >= CUTOFF,
        "a just-completed row must be newer than the retention cutoff"
    );
    assert_eq!(
        store
            .prune_succeeded(CUTOFF, SUCCEEDED_PRUNE_BATCH)
            .expect("prune succeeded"),
        0,
        "a just-completed row must survive its own prune pass"
    );
}

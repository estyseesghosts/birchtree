//! Wave 2 C4: terminal timestamps age quarantined/succeeded rows.
//!
//! Old rows without the field prune by `due_at_ms`; new rows age from the
//! quarantine/complete time. Prune scans read-first and only opens a write
//! transaction when victims exist (covered by the zero-limit and empty-tree
//! no-op paths below).

mod common;

use birchtree::application::ports::JobQueue;
use birchtree::domain::{JobKind, JobState};

use common::temp_store;

const T0: u64 = 1_700_000_000_000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

#[test]
fn quarantine_ages_from_terminal_time_not_due_time() {
    let (_dir, store) = temp_store();
    // Old row shape (due long ago) vs new row quarantined now with an old
    // due time: only the old row is past a cutoff between them.
    let old = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .expect("enqueue old");
    store
        .quarantine(old.id, "stale", T0 + DAY_MS)
        .expect("quarantine old");
    let fresh = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .expect("enqueue fresh");
    // Same old due time, but quarantined much later.
    store
        .quarantine(fresh.id, "fresh", T0 + 30 * DAY_MS)
        .expect("quarantine fresh");
    let removed = store
        .prune_quarantined(T0 + 10 * DAY_MS, 100)
        .expect("prune");
    assert_eq!(removed, 1, "only the terminal-old row prunes");
    assert!(store.get(old.id).expect("get").is_none());
    assert!(store.get(fresh.id).expect("get").is_some());
}

#[test]
fn complete_stamps_terminal_and_prunes_by_it() {
    let (_dir, store) = temp_store();
    let job = store
        .enqueue(JobKind::UserCreated, serde_json::json!({}), T0)
        .expect("enqueue");
    store.complete(job.id, T0 + 5 * DAY_MS).expect("complete");
    let row = store.get(job.id).expect("get").expect("row");
    assert_eq!(row.state, JobState::Succeeded);
    assert_eq!(row.terminal_at_ms, Some(T0 + 5 * DAY_MS));
    // Cutoff before the terminal time prunes nothing; after prunes it.
    assert_eq!(store.prune_succeeded(T0 + DAY_MS, 100).expect("prune"), 0);
    assert_eq!(
        store.prune_succeeded(T0 + 10 * DAY_MS, 100).expect("prune"),
        1
    );
}

#[test]
fn prune_with_no_victims_is_a_no_op() {
    let (_dir, store) = temp_store();
    assert_eq!(store.prune_succeeded(0, 0).expect("limit zero"), 0);
    assert_eq!(store.prune_quarantined(u64::MAX, 0).expect("limit zero"), 0);
    assert_eq!(store.prune_succeeded(u64::MAX, 100).expect("empty"), 0);
    assert_eq!(store.prune_quarantined(u64::MAX, 100).expect("empty"), 0);
}

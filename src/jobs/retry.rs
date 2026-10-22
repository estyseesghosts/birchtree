//! Lease, backoff and quarantine policy (S8 slice A).
//!
//! Re-exports the canonical scheduling constants so the worker settle path
//! and its tests agree on them. Values are defined once in `domain::job`
//! and re-exported here with no timing changes: `LEASE_MS` is 30 s,
//! backoff runs 1 s to 300 s, and the retry budget is 5 attempts.
//!
//! Worker-owned quarantine caps stay where they are until the parent moves
//! them: `QUARANTINE_RETENTION_MS`, `QUARANTINE_MAX_PAYLOAD_BYTES`,
//! `QUARANTINE_PRUNE_BATCH`, `QUARANTINE_ERROR_CAP_BYTES` and
//! `truncate_quarantine_error` (`src/jobs/worker.rs`).

pub use crate::domain::job::{BACKOFF_BASE_MS, BACKOFF_CAP_MS, LEASE_MS, backoff_ms};

use crate::domain::job::Job;

/// Default retry budget: a job is quarantined after this many attempts.
///
/// Mirrors [`Job::DEFAULT_MAX_ATTEMPTS`]; kept as a plain constant so the
/// settle path can name the policy without reaching into the envelope type.
pub const MAX_ATTEMPTS: u32 = Job::DEFAULT_MAX_ATTEMPTS;

//! Durable job types.
//!
//! The canonical envelope lives in `domain::job`; this module re-exports it so
//! callers can use `crate::jobs::types::*` alongside the queue and worker.

pub use crate::domain::job::{
    backoff_ms, Job, JobKind, JobState, BACKOFF_BASE_MS, BACKOFF_CAP_MS, LEASE_MS,
};

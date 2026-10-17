//! Job handlers.
//!
//! Each handler performs one bounded piece of post-commit work. Handlers must
//! be idempotent: a job may be retried after a crash between commit and
//! completion. Only kinds produced by the current phase are expected; the
//! remaining kinds become reachable as their phases land.

use crate::domain::{Error, Job, JobKind, Result};

/// Execute a claimed job.
pub fn handle(job: &Job) -> Result<()> {
    match job.kind {
        JobKind::UserCreated => {
            let account_id = job
                .payload
                .get("account_id")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            tracing::info!(job = %job.id, account_id, "user_created follow-up");
            Ok(())
        }
        JobKind::ProcessMedia => Err(Error::Internal(
            "process_media handler arrives in Phase 4".into(),
        )),
        JobKind::DeliverActivity => Err(Error::Internal(
            "deliver_activity handler arrives in Phase 6".into(),
        )),
    }
}

//! Job handlers.
//!
//! Each handler performs one bounded piece of post-commit work. Handlers must
//! be idempotent: a job may be retried after a crash between commit and
//! completion. Only kinds produced by the current phase are expected; the
//! remaining kinds become reachable as their phases land.

use crate::application::crypto;
use crate::application::ports::MailSender;
use crate::domain::{Error, Id, Job, JobKind, Result};

/// Execute a claimed job.
pub fn handle(job: &Job, mailer: &dyn MailSender) -> Result<()> {
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
        JobKind::SendConfirmationEmail => {
            let account_id = job
                .payload
                .get("account_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| Error::Internal("send_confirmation_email: missing account_id".into()))?;
            let email = job
                .payload
                .get("email")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Internal("send_confirmation_email: missing email".into()))?;
            // The confirmation token is derived deterministically from the job
            // id so a retried job never mints a different value. A real adapter
            // would persist/redeem it; Phase 2 ships a logging adapter.
            let token = crypto::sha256_hex(format!("confirmation:{}", job.id).as_bytes());
            mailer.send_confirmation(email, account_id, &token)
        }
        JobKind::ProcessMedia => Err(Error::Internal(
            "process_media handler arrives in Phase 4".into(),
        )),
        JobKind::DeliverActivity => Err(Error::Internal(
            "deliver_activity handler arrives in Phase 6".into(),
        )),
    }
}

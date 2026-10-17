//! Job handlers.
//!
//! Each handler performs one bounded piece of post-commit work. Handlers must
//! be idempotent: a job may be retried after a crash between commit and
//! completion. Only kinds produced by the current phase are expected; the
//! remaining kinds become reachable as their phases land.

use crate::application::crypto;
use crate::application::notifications;
use crate::application::ports::{BlobStore, MailSender, Store, StreamingSink};
use crate::application::streaming::StreamEvent;
use crate::domain::{Error, Id, Job, JobKind, NotificationType, Result};

/// Execute a claimed job. Streaming publication happens here, after the job's
/// own commit: a create that returns an id publishes it once.
pub fn handle(
    job: &Job,
    store: &dyn Store,
    mailer: &dyn MailSender,
    blobs: &dyn BlobStore,
    streaming: &dyn StreamingSink,
) -> Result<()> {
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
        JobKind::FanOutNotification => {
            // Idempotent: the notification store dedups by
            // (recipient, type, actor, status), so retries are harmless. A
            // dedup hit creates nothing and publishes nothing.
            if let Some(notification_id) = notifications::fan_out_from_payload(store, &job.payload)? {
                streaming.publish(StreamEvent::Notification { notification_id });
            }
            Ok(())
        }
        JobKind::PollExpired => {
            let poll_id = job
                .payload
                .get("poll_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| Error::Internal("poll_expired: missing poll_id".into()))?;
            // Sticky flag: repeating the call is a no-op.
            store.mark_poll_expired(poll_id, job.due_at_ms)?;
            Ok(())
        }
        JobKind::ProcessMedia => {
            let media_id = job
                .payload
                .get("media_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| Error::Internal("process_media: missing media_id".into()))?;
            crate::application::media::process_media_job(store, blobs, media_id, job.due_at_ms)
        }
        JobKind::PublishScheduledStatus => {
            let scheduled_id = job
                .payload
                .get("scheduled_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| {
                    Error::Internal("publish_scheduled_status: missing scheduled_id".into())
                })?;
            let base_url = job
                .payload
                .get("base_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if let Some(status_id) = crate::application::scheduled_statuses::publish_scheduled_job(
                store, scheduled_id, &base_url, job.due_at_ms,
            )? {
                streaming.publish(StreamEvent::StatusCreated { status_id });
            }
            Ok(())
        }
        JobKind::DeliverActivity => Err(Error::Internal(
            "deliver_activity handler arrives in Phase 6".into(),
        )),
    }
}

/// Parse a `FanOutNotification` payload (used by tests and the handler).
pub fn notification_payload(
    recipient_id: Id,
    kind: NotificationType,
    actor_id: Option<Id>,
    status_id: Option<Id>,
) -> serde_json::Value {
    notifications::fan_out_payload(recipient_id, kind, actor_id, status_id, 0)
}

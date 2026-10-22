//! Scheduled-status and poll-expiry handlers (S8 slice group B).
//!
//! Post-commit follow-ups: publishing a due scheduled status (announced on
//! the streaming sink when a row is produced) and marking a poll expired
//! (sticky flag, plus a `question_update` event for local followers).

use crate::application::ports::{Store, StreamingSink};
use crate::application::streaming::StreamEvent;
use crate::domain::{Error, Id, Job, JobKind, Result};

/// Handle a scheduling job with the worker context's store and streaming.
///
/// Mirrors `handlers::handle_with_transcode` for
/// [`JobKind::PublishScheduledStatus`] and [`JobKind::PollExpired`]; any
/// other kind is a caller error.
pub fn handle(job: &Job, store: &dyn Store, streaming: &dyn StreamingSink) -> Result<()> {
    match job.kind {
        JobKind::PollExpired => {
            let poll_id = job
                .payload
                .get("poll_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| Error::Internal("poll_expired: missing poll_id".into()))?;
            // Sticky flag: repeating the call is a no-op.
            if let Some(poll) = store.mark_poll_expired(poll_id, job.due_at_ms)? {
                // The origin tells its followers the poll closed (a no-op
                // when the poll's author is remote).
                store.enqueue_event(
                    poll.account_id,
                    serde_json::json!({
                        "event": "question_update",
                        "status_id": poll.status_id.value()
                    }),
                    job.due_at_ms,
                )?;
            }
            Ok(())
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
                &*store,
                scheduled_id,
                &base_url,
                job.due_at_ms,
            )? {
                streaming.publish(StreamEvent::StatusCreated { status_id });
            }
            Ok(())
        }
        _ => Err(Error::Internal("scheduled: unexpected job kind".into())),
    }
}

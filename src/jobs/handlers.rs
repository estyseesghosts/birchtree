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
    handle_with_transcode(job, store, mailer, blobs, streaming, None)
}

/// Config-aware dispatch (Phase 7.1): the worker threads its toolchain so
/// deferred transcodes finish with the operator's knobs; the sync drain
/// (tests) passes `None` and keeps the legacy media finish.
pub fn handle_with_transcode(
    job: &Job,
    store: &dyn Store,
    mailer: &dyn MailSender,
    blobs: &dyn BlobStore,
    streaming: &dyn StreamingSink,
    transcode: Option<&crate::media::transcode::TranscodeConfig>,
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
            // The resend endpoint puts an explicit random token in the
            // payload; registration jobs carry none, so fall back to the
            // job-id derivation (a retried job never mints a different
            // value). The bearer is persisted before sending so the mailed
            // link is redeemable; overwrite = idempotent across retries.
            let token;
            let token = match job.payload.get("token").and_then(|v| v.as_str()) {
                Some(explicit) if !explicit.is_empty() => explicit,
                _ => {
                    token = crypto::sha256_hex(format!("confirmation:{}", job.id).as_bytes());
                    token.as_str()
                }
            };
            // D12: persist the bearer before sending so the mailed link is redeemable.
            // Overwrite = idempotent: a retried job re-stores the same deterministic token.
            if let Err(err) = store.store_email_token(token, crate::domain::EmailToken {
                account_id,
                kind: crate::domain::EmailTokenKind::Confirmation,
                created_at_ms: job.created_at_ms,
            }) {
                tracing::warn!(error = %err, "confirmation token store failed");
            }
            mailer.send_confirmation(email, account_id, token)
        }
        JobKind::SendPasswordResetEmail => {
            let account_id = job
                .payload
                .get("account_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| Error::Internal("send_password_reset_email: missing account_id".into()))?;
            let email = job
                .payload
                .get("email")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Internal("send_password_reset_email: missing email".into()))?;
            let token = job
                .payload
                .get("token")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty())
                .ok_or_else(|| Error::Internal("send_password_reset_email: missing token".into()))?;
            mailer.send_password_reset(email, account_id, token)
        }
        JobKind::FanOutNotification => {
            // Idempotent: the notification store dedups by
            // (recipient, type, actor, status), so retries are harmless. A
            // dedup hit creates nothing and publishes nothing.
            if let Some(notification_id) = notifications::fan_out_from_payload(store, &job.payload)? {
                streaming.publish(StreamEvent::Notification { notification_id });
                // Phase 7: fan out one durable Web Push delivery per
                // subscription of the recipient. Best-effort: a lookup
                // failure here must never fail the notification itself.
                if let Some(recipient_id) = job
                    .payload
                    .get("recipient_id")
                    .and_then(|v| v.as_u64())
                    .map(Id::new)
                {
                    if let Ok(subs) = store.push_subscriptions_for_user(recipient_id) {
                        for sub in subs {
                            let payload = serde_json::json!({
                                "subscription_id": sub.id.value(),
                                "notification_id": notification_id.value(),
                            });
                            if let Err(err) =
                                store.enqueue(JobKind::WebPushDelivery, payload, job.created_at_ms)
                            {
                                tracing::warn!(error = %err, "web_push_delivery enqueue failed");
                            }
                        }
                    }
                }
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
            if let Some(poll) = store.mark_poll_expired(poll_id, job.due_at_ms)? {
                // The origin tells its followers the poll closed (a no-op
                // when the poll's author is remote).
                store.enqueue_event(
                    poll.account_id,
                    serde_json::json!({"event": "question_update", "status_id": poll.status_id.value()}),
                    job.due_at_ms,
                )?;
            }
            Ok(())
        }
        JobKind::ProcessMedia => {
            let media_id = job
                .payload
                .get("media_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| Error::Internal("process_media: missing media_id".into()))?;
            crate::application::media::process_media_job_with_config(
                store,
                blobs,
                media_id,
                job.due_at_ms,
                transcode,
            )
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
        // The job worker dispatches these kinds to the federation handlers and never
        // calls this function for them. The arm exists for a synchronous drain that
        // does not federate (in-process tests): there is no network to reach here,
        // so the drain completes the job without sending it.
        JobKind::FederationEvent
        | JobKind::ResolveParent
        | JobKind::CrawlReplies
        | JobKind::FetchRemoteMedia
        | JobKind::FetchRemoteEmoji
        | JobKind::PreviewCardFetch => Ok(()),
        JobKind::GenerateAnnualReport => {            let account_id = job
                .payload
                .get("account_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| {
                    Error::Internal("generate_annual_report: missing account_id".into())
                })?;
            let year = job
                .payload
                .get("year")
                .and_then(|v| v.as_i64())
                .and_then(|y| i32::try_from(y).ok())
                .ok_or_else(|| Error::Internal("generate_annual_report: missing year".into()))?;
            crate::application::annual_reports::run_generation_job(
                store,
                account_id,
                year,
                job.due_at_ms,
            )?;
            Ok(())
        }
        JobKind::SearchIndexStatus => {
            crate::jobs::search_index::apply_global(store, &job.payload)
        }
        JobKind::SearchBackfill => {
            let mut queued: Vec<(serde_json::Value, u64)> = Vec::new();
            let done = crate::jobs::search_index::run_backfill_global(
                store,
                &job.payload,
                job.due_at_ms,
                &mut |payload, due| {
                    queued.push((payload, due));
                    Ok(())
                },
            )?;
            for (payload, due) in queued {
                store.enqueue(JobKind::SearchBackfill, payload, due)?;
            }
            let _ = done;
            Ok(())
        }
        // D25 tag-normalization backfill: one bounded, idempotent pass per
        // job; the chain continues while a full batch was seen (there may be
        // more), and stops at the tail. A crashed chain resumes from the
        // last enqueued cursor; a fresh pass only runs when seeded.
        JobKind::TagBackfill => {
            use crate::jobs::tag_backfill as backfill;
            let (cursor, batch) =
                crate::jobs::search_index::parse_backfill_payload(&job.payload)?;
            let batch = batch.min(backfill::BACKFILL_BATCH_CAP);
            let rows = store.backfill_tag_batch(cursor, batch)?;
            let ops = backfill::plan_backfill_batch(&rows);
            for op in &ops {
                store.rewrite_status_tags(op.status_id, &op.new_tags)?;
            }
            if rows.len() >= batch {
                if let Some((last_id, _, _)) = rows.last() {
                    store.enqueue(
                        JobKind::TagBackfill,
                        crate::jobs::search_index::backfill_payload(Some(*last_id), batch),
                        job.due_at_ms,
                    )?;
                }
            }
            Ok(())
        }
        // D38 account deletion: one bounded, idempotent batch per job; the
        // chain continues while a full page was seen. A crashed chain
        // resumes from the last enqueued cursor (or from this job's own
        // cursor when the crash predates the follow-up enqueue, via the
        // job lease); tombstones are idempotent so replays are harmless.
        // The batch clock is the job's creation time, not its lease-shifted
        // due time, so follow-ups stay immediately claimable and retries
        // stamp deterministic times.
        JobKind::AccountDeletionBatch => {
            crate::jobs::account_deletion::run_batch(
                store,
                streaming,
                &job.payload,
                job.created_at_ms,
            )?;
            Ok(())
        }
        // `TranscodeMedia` carries the same `{media_id}` payload as
        // `ProcessMedia` and funnels through the same finish: rows that need
        // a transcode take the ffmpeg pipeline when a toolchain is threaded
        // (the worker always threads one), and every other row — including
        // every row in a toolchain-less sync drain — takes the legacy
        // pure-Rust finish. No dead dispatch: both media kinds do work here.
        JobKind::TranscodeMedia => {
            let media_id = crate::media::transcode::TranscodeJobPayload::media_id_from_json(
                &job.payload,
            )
            .map_err(|err| Error::Internal(err.to_string()))?;
            crate::application::media::process_media_job_with_config(
                store,
                blobs,
                media_id,
                job.due_at_ms,
                transcode,
            )
        }
        // Push delivery needs the async HTTP client, which the sync drain
        // does not have: the worker owns `WebPushDelivery`, and offline
        // drains complete it without doing work.
        JobKind::WebPushDelivery => Ok(()),
        // Storage overhaul skeletons (Step 0): agents B/C own the bodies.
        // No-op until their batches land; payloads stay < 64 KiB, batches
        // bounded to 500 rows, no blobs in the transaction.
        JobKind::CacheEvict | JobKind::ReconcileStorage => Ok(()),
        // Gaps batch skeletons (`docs/gaps.md` Step 0): G-C owns
        // `CollectionFanout`, G-D owns `QuoteReverify` + `RelayForward`.
        // `CollectionFanout` runs here (sync use case over the same store);
        // the federation tail kinds stay worker-owned no-ops in the drain.
        JobKind::CollectionFanout => {
            crate::application::collections::handle_fanout(store, store, job)
        }
        // `SyncFollowers` needs the async HTTP client, which the sync drain
        // does not have: the worker owns it, and offline drains complete it
        // without doing work (same shape as `WebPushDelivery`).
        JobKind::QuoteReverify | JobKind::RelayForward | JobKind::SyncFollowers => Ok(()),
        JobKind::DeliverActivity => Err(Error::Internal(
            "deliver_activity runs in the job worker (federation::delivery), not here".into(),
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

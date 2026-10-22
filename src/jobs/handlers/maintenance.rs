//! Maintenance, search-index and fan-out handlers (S8 slice group B).
//!
//! Sync-drain side of the maintenance chain: notification fan-out, cache
//! lifecycle errors, search index/backfill passes, annual reports, account
//! deletion batches, collection fanout, and the worker-owned no-ops
//! (`PreviewCardFetch`, `RelayForward`, `WebPushDelivery`).

use crate::application::ports::{Store, StreamingSink};
use crate::application::streaming::StreamEvent;
use crate::domain::{Error, Id, Job, JobKind, Result};

/// Handle a maintenance-family job with the worker context.
///
/// Mirrors the corresponding `handlers::handle_with_transcode` arms; any
/// other kind is a caller error. The cache arms stay errors here because the
/// worker owns `CacheMaintenance` (see `jobs::worker`).
pub fn handle(job: &Job, store: &dyn Store, streaming: &dyn StreamingSink) -> Result<()> {
    match job.kind {
        JobKind::FanOutNotification => {
            // Idempotent: the notification store dedups by
            // (recipient, type, actor, status), so retries are harmless.
            if let Some(notification_id) =
                crate::application::notifications::fan_out_from_payload(&*store, &job.payload)?
            {
                streaming.publish(StreamEvent::Notification { notification_id });
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
        JobKind::CacheEvict | JobKind::ReconcileStorage => Err(Error::Internal(
            "cache maintenance runs in the job worker (CacheMaintenance), not here".into(),
        )),
        JobKind::SearchIndexStatus => {
            crate::jobs::search_index::apply_global(&*store, &job.payload)
        }
        JobKind::SearchBackfill => {
            let mut queued: Vec<(serde_json::Value, u64)> = Vec::new();
            let done = crate::jobs::search_index::run_backfill_global(
                &*store,
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
        JobKind::TagBackfill => {
            use crate::jobs::tag_backfill as backfill;
            let (cursor, batch) = crate::jobs::search_index::parse_backfill_payload(&job.payload)?;
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
        JobKind::PreviewCardFetch => Ok(()),
        JobKind::GenerateAnnualReport => {
            let account_id = job
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
                &*store,
                account_id,
                year,
                job.due_at_ms,
            )?;
            Ok(())
        }
        JobKind::AccountDeletionBatch => {
            crate::jobs::account_deletion::run_batch(
                &*store,
                &*streaming,
                &job.payload,
                job.created_at_ms,
            )?;
            Ok(())
        }
        JobKind::CollectionFanout => {
            crate::application::collections::handle_fanout(&*store, &*store, job)
        }
        JobKind::RelayForward => Ok(()),
        JobKind::WebPushDelivery => Ok(()),
        _ => Err(Error::Internal("maintenance: unexpected job kind".into())),
    }
}

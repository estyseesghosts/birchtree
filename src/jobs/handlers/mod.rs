//! Job handlers.
//!
//! Each handler performs one bounded piece of post-commit work. Handlers must
//! be idempotent: a job may be retried after a crash between commit and
//! completion. Only kinds produced by the current phase are expected; the
//! remaining kinds become reachable as their phases land.

pub mod deliver;
pub mod federation_event;
pub mod mail;
pub mod maintenance;
pub mod media;
pub mod remote_media;
pub mod scheduled;
pub mod threads;

use crate::application::notifications;
use crate::application::ports::{BlobStore, MailSender, Store, StreamingSink};
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
        JobKind::SendConfirmationEmail => mail::handle(job, store, mailer),
        JobKind::SendPasswordResetEmail => mail::handle(job, store, mailer),
        JobKind::FanOutNotification => maintenance::handle(job, store, streaming),
        JobKind::PollExpired => scheduled::handle(job, store, streaming),
        JobKind::ProcessMedia => media::handle(job, store, blobs, transcode),
        JobKind::PublishScheduledStatus => scheduled::handle(job, store, streaming),
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
        JobKind::GenerateAnnualReport => maintenance::handle(job, store, streaming),
        JobKind::SearchIndexStatus => maintenance::handle(job, store, streaming),
        JobKind::SearchBackfill => maintenance::handle(job, store, streaming),
        // D25 tag-normalization backfill: one bounded, idempotent pass per
        // job; the chain continues while a full batch was seen (there may be
        // more), and stops at the tail. A crashed chain resumes from the
        // last enqueued cursor; a fresh pass only runs when seeded.
        JobKind::TagBackfill => maintenance::handle(job, store, streaming),
        // D38 account deletion: one bounded, idempotent batch per job; the
        // chain continues while a full page was seen. A crashed chain
        // resumes from the last enqueued cursor (or from this job's own
        // cursor when the crash predates the follow-up enqueue, via the
        // job lease); tombstones are idempotent so replays are harmless.
        // The batch clock is the job's creation time, not its lease-shifted
        // due time, so follow-ups stay immediately claimable and retries
        // stamp deterministic times.
        JobKind::AccountDeletionBatch => maintenance::handle(job, store, streaming),
        // `TranscodeMedia` carries the same `{media_id}` payload as
        // `ProcessMedia` and funnels through the same finish: rows that need
        // a transcode take the ffmpeg pipeline when a toolchain is threaded
        // (the worker always threads one), and every other row Ã¢â‚¬â€ including
        // every row in a toolchain-less sync drain Ã¢â‚¬â€ takes the legacy
        // pure-Rust finish. No dead dispatch: both media kinds do work here.
        JobKind::TranscodeMedia => media::handle(job, store, blobs, transcode),
        // Push delivery needs the async HTTP client, which the sync drain
        // does not have: the worker owns `WebPushDelivery`, and offline
        // drains complete it without doing work.
        JobKind::WebPushDelivery => Ok(()),
        // Cache maintenance runs in the job worker, which owns the
        // `CacheMaintenance` service (async fetch + blocking redb writes).
        // The sync drain has no such service: completing the job here
        // would silently drop eviction/reconciliation work, so fail loudly.
        JobKind::CacheEvict | JobKind::ReconcileStorage => Err(Error::Internal(
            "cache maintenance runs in the job worker (CacheMaintenance), not here".into(),
        )),
        // Gaps batch skeletons (`docs/gaps.md` Step 0): G-C owns
        // `CollectionFanout`, G-D owns `QuoteReverify` + `RelayForward`.
        // `CollectionFanout` runs here (sync use case over the same store);
        // the federation tail kinds stay worker-owned no-ops in the drain.
        JobKind::CollectionFanout => maintenance::handle(job, store, streaming),
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

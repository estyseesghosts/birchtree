//! Thread-completion and quote/follower tail handlers (S8 slice A).
//!
//! Moves the `JobKind::ResolveParent`, `JobKind::CrawlReplies`,
//! `JobKind::QuoteReverify` and `JobKind::SyncFollowers` arms of
//! `jobs::worker::process_one` (`src/jobs/worker.rs`) here verbatim. Scalar
//! payload logic is unchanged; only the resource plumbing comes from the
//! shared [`WorkerCtx`].

use crate::domain::Job;
use crate::domain::JobKind;
use crate::federation::delivery::DeliveryError;
use crate::federation::threads;
use crate::jobs::worker_ctx::WorkerCtx;

/// Run one thread-tail job, dispatching on [`JobKind`].
///
/// Returns [`DeliveryError::Retry`] for transient failures and
/// [`DeliveryError::Quarantine`] for terminal ones; the worker settles the
/// lease from this outcome.
pub async fn handle(ctx: &WorkerCtx, job: &Job) -> Result<(), DeliveryError> {
    match job.kind {
        JobKind::ResolveParent => threads::resolve_parent(&ctx.federation, job).await,
        JobKind::CrawlReplies => threads::crawl_replies(&ctx.federation, job).await,
        JobKind::QuoteReverify => threads::run_quote_reverify(&ctx.federation, job).await,
        JobKind::SyncFollowers => threads::run_sync_followers(&ctx.federation, job).await,
        other => Err(DeliveryError::Quarantine(format!(
            "threads: unexpected job kind {}",
            other.as_str()
        ))),
    }
}

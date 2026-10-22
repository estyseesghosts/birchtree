//! `FederationEvent` job handler (S8 slice A).
//!
//! Moves the `JobKind::FederationEvent` arm of `jobs::worker::process_one`
//! (`src/jobs/worker.rs`) here verbatim. Scalar payload logic is unchanged;
//! only the resource plumbing comes from the shared [`WorkerCtx`].

use crate::domain::Job;
use crate::federation::delivery::DeliveryError;
use crate::federation::events;
use crate::jobs::worker_ctx::WorkerCtx;

/// Run one `FederationEvent` job: build its activity and enqueue one
/// `DeliverActivity` job per destination inbox.
///
/// Returns [`DeliveryError::Retry`] for transient failures and
/// [`DeliveryError::Quarantine`] for terminal ones; the worker settles the
/// lease from this outcome.
pub async fn handle(ctx: &WorkerCtx, job: &Job) -> Result<(), DeliveryError> {
    events::run(&ctx.federation, job).await
}

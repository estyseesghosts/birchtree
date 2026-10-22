//! `DeliverActivity` job handler (S8 slice A).
//!
//! Moves the `JobKind::DeliverActivity` arm of `jobs::worker::process_one`
//! (`src/jobs/worker.rs`) here verbatim. Scalar payload logic is unchanged;
//! only the resource plumbing comes from the shared [`WorkerCtx`].

use crate::domain::Job;
use crate::federation::delivery;
use crate::federation::delivery::DeliveryError;
use crate::jobs::worker_ctx::WorkerCtx;

/// Run one `DeliverActivity` job: sign and send the fixed activity bytes.
///
/// Returns [`DeliveryError::Retry`] for transient failures and
/// [`DeliveryError::Quarantine`] for terminal ones; the worker settles the
/// lease from this outcome.
pub async fn handle(ctx: &WorkerCtx, job: &Job) -> Result<(), DeliveryError> {
    delivery::run(&ctx.federation, job).await
}

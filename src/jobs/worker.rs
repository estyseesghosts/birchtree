//! In-process durable-job worker.
//!
//! Claims due jobs under a lease, executes each handler, and settles the lease
//! by success or retry-with-backoff (terminal quarantine after the retry
//! budget). A worker that dies mid-job leaves the lease to expire; the job
//! becomes claimable again without any recovery scan.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::application::ports::{BlobStore, MailSender, Store, StreamingSink};
use crate::domain::Clock;

use super::handlers;

/// How many jobs a worker claims per poll.
pub const DEFAULT_BATCH: usize = 32;

/// Run the worker until `shutdown` becomes `true`.
pub async fn run(
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    mailer: Arc<dyn MailSender>,
    blobs: Arc<dyn BlobStore>,
    streaming: Arc<dyn StreamingSink>,
    poll_interval: Duration,
    batch: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    tracing::info!("job worker started");
    if *shutdown.borrow() {
        return;
    }
    loop {
        match store.claim_due(clock.now_ms(), batch) {
            Ok(jobs) if jobs.is_empty() => {}
            Ok(jobs) => {
                tracing::debug!(count = jobs.len(), "claimed jobs");
                for job in jobs {
                    match handlers::handle(&job, &*store, &*mailer, &*blobs, &*streaming) {
                        Ok(()) => {
                            if let Err(err) = store.complete(job.id) {
                                tracing::error!(job = %job.id, error = %err, "complete failed");
                            }
                        }
                        Err(err) => match store.retry(job.id, &err.to_string(), clock.now_ms()) {
                            Ok(updated) => tracing::warn!(
                                job = %job.id,
                                state = ?updated.state,
                                attempts = updated.attempts,
                                "job retry scheduled"
                            ),
                            Err(retry_err) => {
                                tracing::error!(job = %job.id, error = %retry_err, "retry failed");
                            }
                        },
                    }
                }
            }
            Err(err) => tracing::error!(error = %err, "claim_due failed"),
        }

        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    tracing::info!("job worker stopped");
}

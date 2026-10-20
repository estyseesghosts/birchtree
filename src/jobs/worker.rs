//! In-process durable-job worker.
//!
//! Claims due jobs under a lease, executes each handler, and settles the lease
//! by success or retry-with-backoff (terminal quarantine after the retry
//! budget). A worker that dies mid-job leaves the lease to expire; the job
//! becomes claimable again without any recovery scan.

use std::sync::Arc;
use std::time::Duration;

use activitypub_federation::config::FederationConfig;
use tokio::sync::watch;

use crate::activitypub::FedContext;
use crate::application::ports::{BlobStore, MailSender, Store, StreamingSink};
use crate::domain::{Clock, JobKind};
use crate::federation::delivery::{self, DeliveryError};
use crate::federation::{events, media, threads};

use super::handlers;

/// How many jobs a worker claims per poll.
pub const DEFAULT_BATCH: usize = 32;

/// Create the signing key of the account a `user_created` job names, in a detached task.
fn spawn_user_keys(federation: FederationConfig<FedContext>, job: &crate::domain::Job) {
    let Some(account_id) = job.payload.get("account_id").and_then(serde_json::Value::as_u64) else {
        tracing::warn!(job = %job.id, "user_created job has no account id; keys stay lazy");
        return;
    };
    tokio::spawn(async move {
        if let Err(err) =
            crate::activitypub::actor::ensure_local_keys_for_account(&federation, crate::domain::Id::new(account_id))
                .await
        {
            tracing::warn!(account_id, error = %err, "local signing key not created ahead of time");
        }
    });
}

/// Run the worker until `shutdown` becomes `true`.
pub async fn run(
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    mailer: Arc<dyn MailSender>,
    blobs: Arc<dyn BlobStore>,
    streaming: Arc<dyn StreamingSink>,
    federation: FederationConfig<FedContext>,
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
                    // Deliveries await the network, so the worker runs them itself.
                    // Every other kind is synchronous and goes through the handlers.
                    // The outcome is `(quarantine, message)` on failure.
                    let outcome = match job.kind {
                        JobKind::DeliverActivity => delivery::run(&federation, &job).await,
                        JobKind::FederationEvent => events::run(&federation, &job).await,
                        JobKind::ResolveParent => threads::resolve_parent(&federation, &job).await,
                        JobKind::CrawlReplies => threads::crawl_replies(&federation, &job).await,
                        JobKind::FetchRemoteMedia => media::fetch(&federation, &job).await,
                        _ => {
                            let sync = handlers::handle(&job, &*store, &*mailer, &*blobs, &*streaming)
                                .map_err(|err| DeliveryError::Retry(err.to_string()));
                            if job.kind == JobKind::UserCreated && sync.is_ok() {
                                // The local signing key is made ahead of the first remote request.
                                // It takes seconds in a debug build, so it runs detached: the
                                // worker must not wait on it. A failure leaves the lazy path in place.
                                spawn_user_keys(federation.clone(), &job);
                            }
                            sync
                        }
                    };
                    let outcome = outcome.map_err(|e| match e {
                        DeliveryError::Retry(msg) => (false, msg),
                        DeliveryError::Quarantine(msg) => (true, msg),
                    });
                    match outcome {
                        Ok(()) => {
                            if let Err(err) = store.complete(job.id) {
                                tracing::error!(job = %job.id, error = %err, "complete failed");
                            }
                        }
                        Err((true, msg)) => {
                            tracing::warn!(job = %job.id, error = %msg, "job quarantined");
                            if let Err(err) = store.quarantine(job.id, &msg) {
                                tracing::error!(job = %job.id, error = %err, "quarantine failed");
                            }
                        }
                        Err((false, msg)) => match store.retry(job.id, &msg, clock.now_ms()) {
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

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
use crate::federation::{events, media, preview, threads};
use crate::media::cache_maintenance::CacheMaintenance;

use super::handlers;

/// How many jobs a worker claims per poll.
pub const DEFAULT_BATCH: usize = 32;

/// Latency of the most recently finished media job (`ProcessMedia` /
/// `TranscodeMedia`) in milliseconds, for the `/metrics`
/// `birchtree_media_job_latency_ms` gauge. Zero when no media job has run
/// yet in this process.
static LAST_MEDIA_JOB_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The media-processing latency gauge value (see [`LAST_MEDIA_JOB_MS`]).
pub fn last_media_job_latency_ms() -> u64 {
    LAST_MEDIA_JOB_MS.load(std::sync::atomic::Ordering::Relaxed)
}

fn note_media_job_latency(started: std::time::Instant) {
    LAST_MEDIA_JOB_MS.store(
        started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Worker-threaded resources for the async job kinds (Phase 7): the push
/// delivery client + VAPID identity, and the ffmpeg toolchain config.
/// Built once in `main.rs` from `Config`.
/// (`CacheMaintenance` holds pool handles without a `Debug` impl, so this
/// struct is `Clone` but not `Debug`.)
#[derive(Clone)]
pub struct WorkerExtra {
    pub push_client: reqwest::Client,
    pub vapid: crate::application::push::VapidKeys,
    /// VAPID `sub` claim (contact URI of the instance).
    pub push_subject: String,
    pub transcode: crate::media::transcode::TranscodeConfig,
    /// Cache lifecycle service (`CacheEvict` / `ReconcileStorage` jobs).
    pub maintenance: Arc<CacheMaintenance>,
    /// Password-reset bearer TTL in ms, for the maintenance email-token prune.
    pub reset_token_ttl_ms: u64,
    /// Liveness stamp beaten every loop iteration for `/ready`.
    pub heartbeat: Arc<crate::api::mastodon::router::readiness::WorkerHeartbeat>,
}

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
    extra: WorkerExtra,
    poll_interval: Duration,
    batch: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    tracing::info!("job worker started");
    if *shutdown.borrow() {
        return;
    }
    loop {
        extra.heartbeat.beat();
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
                        JobKind::QuoteReverify => {
                            threads::run_quote_reverify(&federation, &job).await
                        }
                        JobKind::RelayForward => {
                            delivery::run_relay_forward(&federation, &job).await
                        }
                        JobKind::SyncFollowers => {
                            threads::run_sync_followers(&federation, &job).await
                        }
                        JobKind::FetchRemoteEmoji => {
                            let ctx: &FedContext = &federation;
                            // Expression-style like the sibling arms (the
                            // match evaluates to the outcome; `?` cannot be
                            // used at arm level).
                            (async {
                                let shortcode = job
                                    .payload
                                    .get("shortcode")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                let domain = job
                                    .payload
                                    .get("domain")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                let source = job
                                    .payload
                                    .get("source_url")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                let url =
                                    crate::federation::media::validate_remote_emoji(
                                        &shortcode,
                                        &domain,
                                        &source,
                                    )
                                    .map_err(DeliveryError::Quarantine)?;
                                let (bytes, mime) =
                                    crate::federation::media::fetch_remote_emoji_bytes(
                                        &ctx.media_client,
                                        &url,
                                        ctx.http.allow_http,
                                        &ctx.http.allowlist,
                                    )
                                    .await?;
                                let key =
                                    crate::federation::media::remote_emoji_shortcode(
                                        &shortcode, &domain,
                                    );
                                crate::federation::media::store_remote_emoji(
                                    &*store,
                                    &*blobs,
                                    &key,
                                    &domain,
                                    &bytes,
                                    mime,
                                    clock.now_ms(),
                                )
                                .map_err(DeliveryError::Quarantine)?;
                                Ok::<(), DeliveryError>(())
                            })
                            .await
                        }
                        JobKind::PreviewCardFetch => {
                            let ctx: &FedContext = &federation;
                            preview::fetch(&*store, &ctx.media_client, &ctx.http, &job).await
                        }
                        JobKind::WebPushDelivery => {
                            crate::application::push::run_push_job_record(
                                &*store,
                                &extra.push_client,
                                &extra.vapid,
                                &extra.push_subject,
                                &job,
                                clock.now_ms(),
                            )
                            .await
                            .map_err(|(quarantine, message)| {
                                if quarantine {
                                    DeliveryError::Quarantine(message)
                                } else {
                                    DeliveryError::Retry(message)
                                }
                            })
                        }
                        JobKind::TranscodeMedia => {
                            // ffmpeg blocks: run it off the async runtime.
                            // The job is self-describing (`{media_id}`); the
                            // toolchain comes from the worker context.
                            let started = std::time::Instant::now();
                            let store = Arc::clone(&store);
                            let blobs = Arc::clone(&blobs);
                            let job = job.clone();
                            let due_at_ms = clock.now_ms();
                            let config = extra.transcode.clone();
                            let sync = tokio::task::spawn_blocking(move || {
                                let media_id = crate::media::transcode::TranscodeJobPayload::media_id_from_json(&job.payload)
                                    .map_err(|err| DeliveryError::Quarantine(err.to_string()))?;
                                crate::media::transcode::run_transcode_job(
                                    &*store, &*blobs, media_id, due_at_ms, &config,
                                )
                                .map_err(|err| DeliveryError::Retry(err.to_string()))
                            })
                            .await
                            .unwrap_or_else(|err| {
                                Err(DeliveryError::Retry(format!("transcode join: {err}")))
                            });
                            note_media_job_latency(started);
                            // `ProcessMedia` also funnels here on rows that
                            // need a transcode, so keep the arm total.
                            sync
                        }
                        _ => {
                            // Cache lifecycle jobs run through the
                            // `CacheMaintenance` service on a blocking
                            // thread (redb writes must not block the
                            // async runtime). Any failure retries under
                            // the normal policy; the chain re-seeds its
                            // follow-up on success.
                            if matches!(
                                job.kind,
                                JobKind::CacheEvict | JobKind::ReconcileStorage
                            ) {
                                let maint = Arc::clone(&extra.maintenance);
                                let store = Arc::clone(&store);
                                let job = job.clone();
                                let due_at_ms = clock.now_ms();
                                let reset_ttl_ms = extra.reset_token_ttl_ms;
                                tokio::task::spawn_blocking(move || {
                                    maint
                                        .handle_job(&job, &*store, due_at_ms)
                                        .map_err(|err| {
                                            DeliveryError::Retry(err.to_string())
                                        })?;
                                    // Expired email-token rows are pruned with
                                    // the maintenance chain (bounded batch),
                                    // so confirmation/reset bearers never
                                    // accumulate as never-cleaned rows.
                                    {
                                        match store.prune_email_secrets(
                                            due_at_ms,
                                            reset_ttl_ms,
                                            500,
                                        ) {
                                            Ok(removed) if removed > 0 => {
                                                tracing::info!(
                                                    removed,
                                                    "pruned expired email tokens"
                                                );
                                            }
                                            Ok(_) => {}
                                            Err(err) => {
                                                tracing::warn!(
                                                    error = %err,
                                                    "email token prune failed"
                                                );
                                            }
                                        }
                                    }
                                    Ok(())
                                })
                                .await
                                .unwrap_or_else(|err| {
                                    Err(DeliveryError::Retry(format!(
                                        "maintenance join: {err}"
                                    )))
                                })
                            } else {
                                let is_media_job =
                                    matches!(job.kind, JobKind::ProcessMedia);
                                let started = std::time::Instant::now();
                                let sync = handlers::handle_with_transcode(
                                    &job,
                                    &*store,
                                    &*mailer,
                                    &*blobs,
                                    &*streaming,
                                    Some(&extra.transcode),
                                )
                                .map_err(|err| DeliveryError::Retry(err.to_string()));
                                if is_media_job {
                                    note_media_job_latency(started);
                                }
                                if job.kind == JobKind::UserCreated && sync.is_ok() {
                                    // The local signing key is made ahead of the first remote request.
                                    // It takes seconds in a debug build, so it runs detached: the
                                    // worker must not wait on it. A failure leaves the lazy path in place.
                                    spawn_user_keys(federation.clone(), &job);
                                }
                                sync
                            }
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

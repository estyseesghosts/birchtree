//! In-process durable-job worker.
//!
//! Claims due jobs under a lease, executes each handler, and settles the lease
//! by success or retry-with-backoff (terminal quarantine after the retry
//! budget). A worker that dies mid-job leaves the lease to expire; the job
//! becomes claimable again without any recovery scan.

use std::sync::Arc;
use std::time::Duration;

use activitypub_federation::config::FederationConfig;
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::activitypub::FedContext;
use crate::application::ports::{BlobStore, MailSender, Store, StreamingSink};
use crate::domain::{Clock, JobKind};
use crate::federation::delivery::{self, DeliveryError};
use crate::federation::{events, media, preview, threads};
use crate::media::cache_maintenance::CacheMaintenance;

use super::handlers;

/// How many jobs a worker claims per poll.
pub const DEFAULT_BATCH: usize = 32;

/// Maximum network-bound jobs executed concurrently within one claimed batch.
///
/// Network-bound arms (`DeliverActivity`, `FetchRemoteMedia`,
/// `WebPushDelivery`, ...) dominate worker time, so a small amount of
/// overlap hides remote latency; `redb` serializes write transactions
/// internally, so raising this far past the CPU count only adds lock
/// contention without more throughput. Eight is the compromise: enough to
/// keep several slow inboxes in flight while the store-side settle
/// (`complete`/`retry`/`quarantine`, one short write txn each) drains
/// quickly.
pub const MAX_IN_FLIGHT_NETWORK: usize = 8;

/// Maximum local (store-only) jobs executed concurrently. Local arms are
/// short write transactions, so this bound mostly caps JoinSet growth; it
/// is separate from [`MAX_IN_FLIGHT_NETWORK`] so a flood of slow federation
/// jobs cannot starve local work (and vice versa).
pub const MAX_IN_FLIGHT_LOCAL: usize = 8;

/// Which concurrency pool a claimed job runs in (see [`MAX_IN_FLIGHT_NETWORK`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPool {
    /// Waits on remote hosts; bounded by [`MAX_IN_FLIGHT_NETWORK`].
    Network,
    /// Touches only the local store; bounded by [`MAX_IN_FLIGHT_LOCAL`].
    Local,
    /// Already runs on the blocking thread pool (`TranscodeMedia`,
    /// `CacheEvict`/`ReconcileStorage`); needs no async semaphore.
    Blocking,
}

/// Classify a job into its concurrency pool. CPU-heavy and maintenance
/// arms bypass the async semaphores because they already execute on the
/// blocking thread pool.
///
/// `pub` so scheduling tests can assert the partition covers every
/// [`JobKind`] exactly once (see `tests/fix_worker_sched.rs`).
pub fn job_pool(kind: JobKind) -> JobPool {
    match kind {
        JobKind::DeliverActivity
        | JobKind::FederationEvent
        | JobKind::ResolveParent
        | JobKind::CrawlReplies
        | JobKind::FetchRemoteMedia
        | JobKind::FetchRemoteEmoji
        | JobKind::QuoteReverify
        | JobKind::RelayForward
        | JobKind::SyncFollowers
        | JobKind::PreviewCardFetch
        | JobKind::WebPushDelivery => JobPool::Network,
        JobKind::TranscodeMedia | JobKind::CacheEvict | JobKind::ReconcileStorage => {
            JobPool::Blocking
        }
        _ => JobPool::Local,
    }
}

/// How long a `Succeeded` job row is kept before the maintenance pass may
/// prune it (7 days in ms). `Quarantined` rows age out separately through
/// `prune_quarantined` (see [`QUARANTINE_RETENTION_MS`]).
pub const SUCCEEDED_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Maximum `Succeeded` rows removed per maintenance pass (bounded batch).
pub const SUCCEEDED_PRUNE_BATCH: usize = 500;

/// How long a quarantined row is kept before it becomes a prune victim (90
/// days in ms). Quarantine is the operator inspection surface, so this is
/// deliberately far longer than [`SUCCEEDED_RETENTION_MS`].
pub const QUARANTINE_RETENTION_MS: u64 = 90 * 24 * 60 * 60 * 1000;

/// Largest job payload (serialized JSON bytes) stored at enqueue.
/// `persistence::redb` quarantines oversize payloads on insert with a
/// recorded reason and a bounded marker (untrusted remote input must never
/// accumulate as unbounded JSON in the store).
pub const QUARANTINE_MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Maximum quarantined rows removed per maintenance pass (bounded batch,
///
/// mirroring [`SUCCEEDED_PRUNE_BATCH`]).
pub const QUARANTINE_PRUNE_BATCH: usize = 500;

/// Cap on a failure message stored on a quarantined row. The settle path
/// truncates through [`truncate_quarantine_error`] so one verbose remote
/// cannot bloat the row; kind/attempts/timestamps are untouched.
pub const QUARANTINE_ERROR_CAP_BYTES: usize = 4 * 1024;

/// Truncate a quarantine failure message to [`QUARANTINE_ERROR_CAP_BYTES`]
/// on a character boundary, marking the cut. Short messages pass through
/// unchanged; diagnostic metadata (kind, attempts, timestamps) is never part
/// of the message and is unaffected.
pub fn truncate_quarantine_error(message: &str) -> String {
    if message.len() <= QUARANTINE_ERROR_CAP_BYTES {
        return message.to_string();
    }
    let mut end = QUARANTINE_ERROR_CAP_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}...[truncated {} bytes]",
        &message[..end],
        message.len() - end
    )
}

/// Bounded table prunes that ride the `CacheEvict` maintenance chain (one
/// call per pass, every arm bounded and failure-isolated): expired
/// email-token rows, old `Succeeded` job rows, and old `Quarantined` job
/// rows. Quarantine prune is delete-only — the row's `last_error` and
/// redacted payload were the diagnostics, and they age out with the row
/// (no archive export).
pub fn run_table_prunes(store: &dyn Store, now_ms: u64, reset_ttl_ms: u64) {
    // Expired email-token rows are pruned with the maintenance chain
    // (bounded batch), so confirmation/reset bearers never accumulate as
    // never-cleaned rows.
    match store.prune_email_secrets(now_ms, reset_ttl_ms, 500) {
        Ok(removed) if removed > 0 => {
            tracing::info!(removed, "pruned expired email tokens");
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(error = %err, "email token prune failed");
        }
    }
    // Old `Succeeded` rows are pruned with the same bounded-batch
    // maintenance chain, so completed job records stop accumulating
    // metadata forever.
    match store.prune_succeeded(
        now_ms.saturating_sub(SUCCEEDED_RETENTION_MS),
        SUCCEEDED_PRUNE_BATCH,
    ) {
        Ok(removed) if removed > 0 => {
            tracing::info!(removed, "pruned old succeeded jobs");
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(error = %err, "succeeded job prune failed");
        }
    }
    // Old `Quarantined` rows are pruned with the same bounded-batch chain
    // (delete-only). Pending/leased rows are live work and are never
    // touched.
    match store.prune_quarantined(
        now_ms.saturating_sub(QUARANTINE_RETENTION_MS),
        QUARANTINE_PRUNE_BATCH,
    ) {
        Ok(removed) if removed > 0 => {
            tracing::info!(removed, "pruned old quarantined jobs");
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(error = %err, "quarantined job prune failed");
        }
    }
}

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
    let Some(account_id) = job
        .payload
        .get("account_id")
        .and_then(serde_json::Value::as_u64)
    else {
        tracing::warn!(job = %job.id, "user_created job has no account id; keys stay lazy");
        return;
    };
    tokio::spawn(async move {
        if let Err(err) = crate::activitypub::actor::ensure_local_keys_for_account(
            &federation,
            crate::domain::Id::new(account_id),
        )
        .await
        {
            tracing::warn!(account_id, error = %err, "local signing key not created ahead of time");
        }
    });
}

/// Execute one claimed job and settle its lease, concurrently with the
/// rest of the batch (see [`MAX_IN_FLIGHT`]).
///
/// Settlement is the same claim-then-settle path the sequential loop used:
/// `complete` on success, `quarantine` on terminal failure, `retry`
/// otherwise. Every arm is idempotent, so a task lost to a crash simply
/// leaves its lease to expire and be reclaimed.
#[allow(clippy::too_many_arguments)]
async fn process_one(
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    mailer: Arc<dyn MailSender>,
    blobs: Arc<dyn BlobStore>,
    streaming: Arc<dyn StreamingSink>,
    federation: FederationConfig<FedContext>,
    extra: WorkerExtra,
    job: crate::domain::Job,
) {
    // Deliveries await the network, so the worker runs them itself.
    // Every other kind is synchronous and goes through the handlers.
    // The outcome is `(quarantine, message)` on failure.
    let outcome = match job.kind {
        JobKind::DeliverActivity => delivery::run(&federation, &job).await,
        JobKind::FederationEvent => events::run(&federation, &job).await,
        JobKind::ResolveParent => threads::resolve_parent(&federation, &job).await,
        JobKind::CrawlReplies => threads::crawl_replies(&federation, &job).await,
        JobKind::FetchRemoteMedia => media::fetch(&federation, &job).await,
        JobKind::QuoteReverify => threads::run_quote_reverify(&federation, &job).await,
        JobKind::RelayForward => delivery::run_relay_forward(&federation, &job).await,
        JobKind::SyncFollowers => threads::run_sync_followers(&federation, &job).await,
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
                    crate::federation::media::validate_remote_emoji(&shortcode, &domain, &source)
                        .map_err(DeliveryError::Quarantine)?;
                let (bytes, _mime) = crate::federation::media::fetch_remote_emoji_bytes(
                    &ctx.media_client,
                    &url,
                    ctx.http.allow_http,
                    &ctx.http.allowlist,
                )
                .await?;
                let key = crate::federation::media::remote_emoji_shortcode(&shortcode, &domain);
                crate::federation::media::store_remote_emoji(
                    &*store,
                    &*blobs,
                    &key,
                    &domain,
                    url.as_str(),
                    &bytes,
                    clock.now_ms(),
                    &ctx.remote_cache,
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
        JobKind::WebPushDelivery => crate::application::push::run_push_job_record(
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
        }),
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
                let media_id =
                    crate::media::transcode::TranscodeJobPayload::media_id_from_json(&job.payload)
                        .map_err(|err| DeliveryError::Quarantine(err.to_string()))?;
                crate::media::transcode::run_transcode_job(
                    &*store, &*blobs, media_id, due_at_ms, &config,
                )
                .map_err(|err| DeliveryError::Retry(err.to_string()))
            })
            .await
            .unwrap_or_else(|err| Err(DeliveryError::Retry(format!("transcode join: {err}"))));
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
            if matches!(job.kind, JobKind::CacheEvict | JobKind::ReconcileStorage) {
                let maint = Arc::clone(&extra.maintenance);
                let store = Arc::clone(&store);
                let job = job.clone();
                let due_at_ms = clock.now_ms();
                let reset_ttl_ms = extra.reset_token_ttl_ms;
                tokio::task::spawn_blocking(move || {
                    maint
                        .handle_job(&job, &*store, due_at_ms)
                        .map_err(|err| DeliveryError::Retry(err.to_string()))?;
                    // Table prunes ride the maintenance chain (bounded
                    // batches): email tokens, succeeded jobs, quarantined
                    // jobs. Failures are isolated per table inside.
                    run_table_prunes(&*store, due_at_ms, reset_ttl_ms);
                    Ok(())
                })
                .await
                .unwrap_or_else(|err| Err(DeliveryError::Retry(format!("maintenance join: {err}"))))
            } else if job.kind == JobKind::ProcessMedia {
                // Image decode/resize/BlurHash are CPU-bound blocking
                // work: running them on the async executor would stall
                // every other task, and a burst of remote fetches
                // enqueueing `ProcessMedia` rows would starve Tokio
                // without a bound. Route through the shared bounded
                // blocking pool (see
                // [`crate::media::processing::run_bounded_blocking`]):
                // the same gate the HTTP upload path uses, so remote
                // influx and local uploads share one global bound.
                let started = std::time::Instant::now();
                let store = Arc::clone(&store);
                let mailer = Arc::clone(&mailer);
                let blobs = Arc::clone(&blobs);
                let streaming = Arc::clone(&streaming);
                let owned = job.clone();
                let transcode = extra.transcode.clone();
                let gated = crate::media::processing::run_bounded_blocking(move || {
                    handlers::handle_with_transcode(
                        &owned,
                        &*store,
                        &*mailer,
                        &*blobs,
                        &*streaming,
                        Some(&transcode),
                    )
                })
                .await
                .map_err(|err| DeliveryError::Retry(err.to_string()));
                let sync = match gated {
                    Ok(inner) => inner.map_err(|err| DeliveryError::Retry(err.to_string())),
                    Err(gate_err) => Err(gate_err),
                };
                note_media_job_latency(started);
                sync
            } else {
                let sync = handlers::handle_with_transcode(
                    &job,
                    &*store,
                    &*mailer,
                    &*blobs,
                    &*streaming,
                    Some(&extra.transcode),
                )
                .map_err(|err| DeliveryError::Retry(err.to_string()));
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
            if let Err(err) = store.quarantine(job.id, &truncate_quarantine_error(&msg)) {
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
                // Bounded fan-out: one task per claimed job, gated by a
                // per-class semaphore. Network-bound arms dominate worker
                // time, so sequential execution let one slow inbox stall
                // the whole batch behind it; separate pools additionally
                // keep a flood of slow federation jobs from starving local
                // work (and vice versa). CPU-heavy arms already run on
                // the blocking thread pool and bypass the semaphores.
                //
                // The permit is acquired INSIDE the spawned task, never in
                // this loop: acquiring here would let a saturated pool block
                // spawning (and therefore running) jobs of the other pool
                // (head-of-line blocking: 8 slow network jobs stall every
                // local job behind them). Every task is spawned first, then
                // contends only for its own pool's bound.
                //
                // Ordering: same-destination deliveries serialize on the
                // per-host lock in `federation::delivery` (see
                // [`crate::federation::delivery::destination_lock`]), so
                // Follow/Undo and Create/Delete pairs to one inbox cannot
                // interleave; different hosts still proceed in parallel.
                // Every arm stays idempotent and retries/cooldowns are
                // tracked per job and per destination.
                //
                // Shutdown drains: a claimed batch always settles before
                // the next poll; `shutdown` is checked between batches
                // below. A task lost to a crash leaves its lease to
                // expire and be reclaimed, as before.
                let network_semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT_NETWORK));
                let local_semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT_LOCAL));
                let mut in_flight = JoinSet::new();
                for job in jobs {
                    // CPU-heavy arms already run on the blocking thread
                    // pool and take no async permit.
                    let semaphore = match job_pool(job.kind) {
                        JobPool::Network => Some(Arc::clone(&network_semaphore)),
                        JobPool::Local => Some(Arc::clone(&local_semaphore)),
                        JobPool::Blocking => None,
                    };
                    let store = Arc::clone(&store);
                    let clock = Arc::clone(&clock);
                    let mailer = Arc::clone(&mailer);
                    let blobs = Arc::clone(&blobs);
                    let streaming = Arc::clone(&streaming);
                    let federation = federation.clone();
                    let extra = extra.clone();
                    in_flight.spawn(async move {
                        let _permit = match semaphore {
                            Some(sem) => match sem.acquire_owned().await {
                                Ok(permit) => Some(permit),
                                Err(_) => return,
                            },
                            None => None,
                        };
                        process_one(
                            store, clock, mailer, blobs, streaming, federation, extra, job,
                        )
                        .await;
                    });
                }
                while in_flight.join_next().await.is_some() {}
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

//! Remote-media tail handlers (S8 slice A).
//!
//! Moves the `JobKind::FetchRemoteMedia` arm and the inline
//! `JobKind::FetchRemoteEmoji` block of `jobs::worker::process_one`
//! (`src/jobs/worker.rs`) here verbatim. Scalar payload logic is unchanged;
//! only the resource plumbing comes from the shared [`WorkerCtx`].

use crate::activitypub::FedContext;
use crate::domain::{Job, JobKind};
use crate::federation::delivery::DeliveryError;
use crate::federation::media;
use crate::jobs::worker_ctx::WorkerCtx;

/// Parse a `FetchRemoteEmoji` payload into `(shortcode, domain, source_url)`.
///
/// Verbatim copy of the payload reads at the top of the worker's
/// `FetchRemoteEmoji` arm (`src/jobs/worker.rs`).
pub fn parse_emoji_job(job: &Job) -> (String, String, String) {
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
    (shortcode, domain, source)
}

/// Run one remote-media job (`FetchRemoteMedia` or `FetchRemoteEmoji`).
///
/// Returns [`DeliveryError::Retry`] for transient failures and
/// [`DeliveryError::Quarantine`] for terminal ones; the worker settles the
/// lease from this outcome.
pub async fn handle(ctx: &WorkerCtx, job: &Job) -> Result<(), DeliveryError> {
    match job.kind {
        JobKind::FetchRemoteMedia => media::fetch(&ctx.federation, job).await,
        JobKind::FetchRemoteEmoji => {
            let fed: &FedContext = &ctx.federation;
            let (shortcode, domain, source) = parse_emoji_job(job);
            let url = media::validate_remote_emoji(&shortcode, &domain, &source)
                .map_err(DeliveryError::Quarantine)?;
            let (bytes, _mime) = media::fetch_remote_emoji_bytes(
                &fed.media_client,
                &url,
                fed.http.allow_http,
                &fed.http.allowlist,
            )
            .await?;
            let key = media::remote_emoji_shortcode(&shortcode, &domain);
            media::store_remote_emoji(
                &*ctx.store,
                &*ctx.blobs,
                &*ctx.store,
                crate::media::remote_cache::process_ledger(),
                &key,
                &domain,
                url.as_str(),
                &bytes,
                ctx.clock.now_ms(),
                &fed.remote_cache,
            )
            .map_err(DeliveryError::Quarantine)?;
            Ok(())
        }
        other => Err(DeliveryError::Quarantine(format!(
            "remote_media: unexpected job kind {}",
            other.as_str()
        ))),
    }
}

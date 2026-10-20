//! Remote media (Phase 6): fetch an attachment's bytes from the server that
//! published it, check them, and stage them for the existing processing job.
//!
//! The fetch applies the same URL policy as every federation fetch (HTTPS only,
//! no private addresses, unless the operator allowlist names them), the connect
//! checks of the resolver, a size cap, and a content check by sniffing. A failure
//! that another try will not fix is quarantined; the attachment then stays
//! pending, and is never shown as processed.

use futures_util::StreamExt;
use url::Url;

use crate::activitypub::FedContext;
use crate::domain::{Id, Job};
use crate::federation::delivery::DeliveryError;
use crate::federation::policy;
use crate::media::ingest;

/// The largest remote attachment accepted (Mastodon's default upload limit: 40 MiB).
pub const MAX_REMOTE_MEDIA_BYTES: u64 = 40 * 1024 * 1024;

/// Fetch and stage one remote attachment.
pub async fn fetch(config: &activitypub_federation::config::FederationConfig<FedContext>, job: &Job) -> std::result::Result<(), DeliveryError> {
    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let media_id = job
        .payload
        .get("media_id")
        .and_then(serde_json::Value::as_u64)
        .map(Id::new)
        .ok_or_else(|| DeliveryError::Quarantine("media job has no media".into()))?;
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let source = match store.remote_media_source(media_id).map_err(retry)? {
        Some(s) => s,
        None => {
            let _ = store.mark_remote_media_failed(media_id, "attachment has no source", ctx.app.now_ms());
            return Err(DeliveryError::Quarantine("attachment has no source".into()));
        }
    };
    let url = Url::parse(&source).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
    if let Err(e) = policy::check_url(&url, ctx.http.allow_http, &ctx.http.allowlist) {
        let _ = store.mark_remote_media_failed(media_id, &format!("attachment url refused: {e}"), ctx.app.now_ms());
        return Err(DeliveryError::Quarantine(format!("attachment url refused: {e}")));
    }

    let response = ctx
        .media_client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "*/*")
        .send()
        .await
        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    let status = response.status();
    if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(DeliveryError::Retry(format!("attachment server answered {status}")));
    }
    if !status.is_success() {
        let _ = store.mark_remote_media_failed(media_id, &format!("attachment server answered {status}"), ctx.app.now_ms());
        return Err(DeliveryError::Quarantine(format!("attachment server answered {status}")));
    }
    if response.content_length().is_some_and(|len| len > MAX_REMOTE_MEDIA_BYTES) {
        let _ = store.mark_remote_media_failed(media_id, "attachment is larger than the limit", ctx.app.now_ms());
        return Err(DeliveryError::Quarantine("attachment is larger than the limit".into()));
    }
    let mut body: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DeliveryError::Retry(e.to_string()))?;
        if body.len() as u64 + chunk.len() as u64 > MAX_REMOTE_MEDIA_BYTES {
            let _ = store.mark_remote_media_failed(media_id, "attachment is larger than the limit", ctx.app.now_ms());
            return Err(DeliveryError::Quarantine("attachment is larger than the limit".into()));
        }
        body.extend_from_slice(&chunk);
    }
    let sniffed = match ingest::sniff(&body).filter(|s| ingest::validate(*s)) {
        Some(s) => s,
        None => {
            let _ = store.mark_remote_media_failed(media_id, "attachment is not a supported media file", ctx.app.now_ms());
            return Err(DeliveryError::Quarantine("attachment is not a supported media file".into()));
        }
    };

    let key = format!("remote/{}", media_id.value());
    ctx.app
        .blobs
        .put(&key, &body)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    let now = ctx.app.now_ms();
    match store
        .stage_remote_media(media_id, &key, sniffed.mime_type, body.len() as u64, now)
        .map_err(retry)?
    {
        Some(_) => {
            tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "FetchRemoteMedia", media = media_id.value(), bytes = body.len(), mime = sniffed.mime_type, outcome = "staged", "media");
            Ok(())
        }
        None => {
            let _ = store.mark_remote_media_failed(media_id, "attachment row is missing", ctx.app.now_ms());
            Err(DeliveryError::Quarantine("attachment row is missing".into()))
        }
    }
}

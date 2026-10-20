//! Remote custom-emoji fetch (Phase 6 tail): fetch one emoji image from the
//! server that published it, check it, and hand the bytes to
//! [`crate::application::federation_inbound::ingest_remote_emojis`].
//!
//! The fetch applies the same URL policy as every federation fetch (HTTPS
//! only, no private addresses unless the operator allowlist names them),
//! the connect checks of the resolver (callers pass the shared outbound
//! client built by [`crate::federation::net`], which carries the timeouts
//! and the policy-checked resolver), a 256 KiB cap
//! (`CustomEmoji::IMAGE_LIMIT`, Mastodon's `CustomEmoji::LIMIT`), and an
//! emoji content check by sniffing. Anything off is refused before any byte
//! is staged; the Note ingests without that emoji.

use futures_util::StreamExt;
use url::Url;

use crate::domain::CustomEmoji;
use crate::federation::policy::{self, Cidr};

/// The largest remote emoji image accepted (Mastodon's `CustomEmoji::LIMIT`).
pub const MAX_REMOTE_EMOJI_BYTES: u64 = CustomEmoji::IMAGE_LIMIT;

/// Why an emoji fetch failed. `Refused` will not heal on retry; `Retry`
/// might (5xx, 429, timeout, transport error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmojiFetchError {
    Refused(String),
    Retry(String),
}

impl std::fmt::Display for EmojiFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "emoji refused: {reason}"),
            Self::Retry(reason) => write!(f, "emoji retry: {reason}"),
        }
    }
}

/// Fetch one emoji image with the shared outbound client. Redirects are not
/// followed here: an emoji URL that redirects is refused, like attachment
/// URLs in [`crate::federation::media`].
pub async fn fetch_emoji_bytes(
    client: &reqwest::Client,
    url: &Url,
    allow_http: bool,
    allowlist: &[Cidr],
) -> Result<Vec<u8>, EmojiFetchError> {
    policy::check_url(url, allow_http, allowlist)
        .map_err(|e| EmojiFetchError::Refused(format!("emoji url refused: {e}")))?;
    let response = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "image/png,image/gif,image/webp")
        .send()
        .await
        .map_err(|e| EmojiFetchError::Retry(e.to_string()))?;
    let status = response.status();
    if status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err(EmojiFetchError::Retry(format!("emoji server answered {status}")));
    }
    if !status.is_success() {
        return Err(EmojiFetchError::Refused(format!("emoji server answered {status}")));
    }
    if response.content_length().is_some_and(|len| len > MAX_REMOTE_EMOJI_BYTES) {
        return Err(EmojiFetchError::Refused("emoji is larger than the limit".into()));
    }
    let mut body: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| EmojiFetchError::Retry(e.to_string()))?;
        if body.len() as u64 + chunk.len() as u64 > MAX_REMOTE_EMOJI_BYTES {
            return Err(EmojiFetchError::Refused("emoji is larger than the limit".into()));
        }
        body.extend_from_slice(&chunk);
    }
    if !crate::media::ingest::is_emoji_image(&body) {
        return Err(EmojiFetchError::Refused("emoji is not a supported emoji image".into()));
    }
    Ok(body)
}

/// Durable handler for `JobKind::FetchRemoteEmoji` (payload
/// `{shortcode, domain, url}`), mirroring [`crate::federation::media::fetch`].
///
/// The emoji row commits post-fetch (network I/O never joins the ingest
/// transaction, per `birchtree.md` §5). Re-ingest replaces the row and its
/// bytes, so a redelivered job is a no-op. A refusal quarantines the job
/// without failing the status that named the emoji.
pub async fn fetch(
    config: &activitypub_federation::config::FederationConfig<crate::activitypub::FedContext>,
    job: &crate::domain::Job,
) -> std::result::Result<(), crate::federation::delivery::DeliveryError> {
    use crate::federation::delivery::DeliveryError;

    let ctx: &crate::activitypub::FedContext = config;
    let shortcode = job
        .payload
        .get("shortcode")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let domain = job
        .payload
        .get("domain")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let url_raw = job
        .payload
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if shortcode.is_empty() || domain.is_empty() || url_raw.is_empty() {
        return Err(DeliveryError::Quarantine("emoji job is missing shortcode/domain/url".into()));
    }
    let url = Url::parse(&url_raw).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
    let bytes = match fetch_emoji_bytes(&ctx.media_client, &url, ctx.http.allow_http, &ctx.http.allowlist).await
    {
        Ok(bytes) => bytes,
        Err(EmojiFetchError::Retry(reason)) => return Err(DeliveryError::Retry(reason)),
        Err(EmojiFetchError::Refused(reason)) => return Err(DeliveryError::Quarantine(reason)),
    };
    let emoji = crate::application::federation_inbound::RemoteEmoji {
        shortcode,
        url: url_raw,
        static_url: None,
        domain,
    };
    crate::application::federation_inbound::ingest_remote_emojis(
        &*ctx.app.store,
        &*ctx.app.blobs,
        std::slice::from_ref(&emoji),
        &|_| Some(bytes.clone()),
    )
    .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    Ok(())
}

//! Remote media fetch stages (S7 slice group A, step 1).
//!
//! Stage helpers extracted verbatim in behavior from
//! `src/federation/media.rs` (`fetch`, `get_signed`). The `fetch` job
//! itself stays in `media.rs`; the parent rewires it to a ~50-line
//! orchestrator that calls these stages in order:
//! `parse_job` → `load_source` → `gate_url` → `fast_path_if_cached` → live
//! reservation → `signed_get` → `enforce_caps` → `commit_bytes`.
//!
//! Source spans (read-only audit):
//! `media.rs:44-52` constants, `media.rs:67-132` `get_signed`,
//! `media.rs:427-766` `fetch`.
//!
//! Constants below are byte-identical to `media.rs` so either path enforces
//! the same ceilings. The canonical definitions stay in `media.rs` until the
//! parent dedups them; this module duplicates the values (not the logic) to
//! keep each stage self-contained and reviewable.

use activitypub_federation::config::FederationConfig;
use axum::body::Bytes;
use url::Url;

use crate::activitypub::FedContext;
use crate::application::ports::BlobStore;
use crate::application::ports::Store;
use crate::config::RemoteCacheConfig;
use crate::domain::Id;
use crate::domain::Job;
use crate::federation::delivery::DeliveryError;
use crate::federation::policy;
use crate::federation::policy::Cidr;
use crate::media::ingest;
use crate::media::remote_cache;

/// The largest remote attachment accepted (Mastodon's default upload limit: 40 MiB).
/// Stays the per-file ceiling inside the class/global budgets (not a config key).
pub const MAX_REMOTE_MEDIA_BYTES: u64 = 40 * 1024 * 1024;

/// Redirect hops followed for one attachment (each hop is revalidated).
pub const MAX_REDIRECT_HOPS: usize = 5;

/// Attachments are fetched as full originals (`variant "original"`).
const ATTACHMENT_VARIANT: &str = "original";

/// Whether a redirect hop keeps the instance-actor HTTP signature.
///
/// Policy: sign only same-origin hops, where same-origin means the hop's URL
/// shares the initial URL's origin (scheme, host, port). A hop that leaves
/// the origin is still fetched when the SSRF URL policy allows it — it is
/// public, just unrelated — but without the `Signature` header, so a
/// third-party host never receives an authentication token minted for someone
/// else. There is no opt-in to re-sign cross-origin hops.
pub fn redirect_keeps_signature(first: &Url, next: &Url) -> bool {
    first.origin() == next.origin()
}

/// Parse the `FetchRemoteMedia` job payload into the target media id.
///
/// Verbatim from `fetch`: a missing or non-`u64` `media_id` quarantines with
/// the same message, so a malformed job never retries in a loop.
pub fn parse_job(job: &Job) -> Result<Id, DeliveryError> {
    job.payload
        .get("media_id")
        .and_then(serde_json::Value::as_u64)
        .map(Id::new)
        .ok_or_else(|| DeliveryError::Quarantine("media job has no media".into()))
}

/// Load the durable source URL for a remote attachment.
///
/// Verbatim from `fetch`: a store error retries, a missing source marks the
/// row failed with the same reason and quarantines with the same message.
pub fn load_source(store: &dyn Store, media_id: Id, now_ms: u64) -> Result<String, DeliveryError> {
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    match store.remote_media_source(media_id).map_err(retry)? {
        Some(source) => Ok(source),
        None => {
            let _ = store.mark_remote_media_failed(media_id, "attachment has no source", now_ms);
            Err(DeliveryError::Quarantine("attachment has no source".into()))
        }
    }
}

/// Gate a parsed attachment URL against the SSRF URL policy.
///
/// Pure check with the same refusal string as `fetch`
/// (`"attachment url refused: {e}"`). The caller marks the row failed with
/// that string, exactly as `fetch` does, so the observable outcome is
/// unchanged when the orchestrator composes `gate_url` + `mark_failed`.
pub fn gate_url(url: &Url, allow_http: bool, allowlist: &[Cidr]) -> Result<(), DeliveryError> {
    policy::check_url(url, allow_http, allowlist)
        .map_err(|e| DeliveryError::Quarantine(format!("attachment url refused: {e}")))
}

/// Outcome of the cache fast path: a hit is fully staged, a miss must fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastPath {
    /// Bytes were already cached and the row is staged; the job is done.
    Hit,
    /// No usable bytes; the caller must reserve and fetch.
    Miss,
}

/// Fast path: bytes already cached (first request won, or a retried job).
///
/// Verbatim from `fetch` (disabled-mode refusal first, then ledger
/// accounting): counts uncounted bytes, refreshes last-use, drops
/// never-persisted audio/video originals, and heals a torn write by staging.
/// Returns [`FastPath::Hit`] when the job is complete, [`FastPath::Miss`]
/// when the caller must fetch. Error strings and the
/// `"remote audio/video streams from its origin and is never cached"` reason
/// are byte-identical.
pub fn fast_path_if_cached(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    source: &str,
    media_id: Id,
    key: &str,
    now_ms: u64,
    cfg: &RemoteCacheConfig,
) -> Result<FastPath, DeliveryError> {
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let persist = remote_cache::process_ledger()
        .lock()
        .map(|ledger| ledger.persistence_allowed())
        .unwrap_or(false);
    if !(persist && blobs.get(key).map_err(retry)?.is_some()) {
        return Ok(FastPath::Miss);
    }
    {
        let mut ledger = remote_cache::process_ledger()
            .lock()
            .map_err(|e| DeliveryError::Retry(format!("cache ledger poisoned: {e}")))?;
        let cache_key =
            crate::persistence::redb::schema::remote_cache_key(source, ATTACHMENT_VARIANT);
        let seen = ledger.get(&cache_key).is_some_and(|e| e.has_bytes);
        if !seen {
            if let Ok(Some(bytes)) = blobs.get(key).map_err(|_| ()) {
                let streams = ingest::sniff(&bytes)
                    .filter(|sniffed| ingest::validate(*sniffed))
                    .is_some_and(|sniffed| {
                        remote_cache::class_for_fetch(sniffed.mime_type, ATTACHMENT_VARIANT)
                            == remote_cache::MediaClass::Stream
                    });
                if streams {
                    let _ = blobs.delete(key);
                    let reason = "remote audio/video streams from its origin and is never cached";
                    let _ = store.mark_remote_media_failed(media_id, reason, now_ms);
                    return Err(DeliveryError::Quarantine(reason.into()));
                }
                let entry = remote_cache::RemoteCacheEntry::stored(
                    source,
                    ATTACHMENT_VARIANT,
                    remote_cache::MediaClass::Full,
                    key,
                    bytes.len() as u64,
                    now_ms,
                    cfg,
                );
                let _ = ledger.commit(remote_cache::MediaClass::Full, 0, entry);
            }
        } else {
            ledger.touch(&cache_key, now_ms);
        }
    }
    if let Ok(Some(row)) = store.media_by_id(media_id).map_err(|_| None::<()>) {
        if !row.file_key.is_empty() {
            return Ok(FastPath::Hit);
        }
    }
    if let Ok(Some(bytes)) = blobs.get(key).map_err(|_| None::<()>) {
        if bytes.len() as u64 <= MAX_REMOTE_MEDIA_BYTES
            && let Some(sniffed) = ingest::sniff(&bytes).filter(|s| ingest::validate(*s))
        {
            let staged = store
                .stage_remote_media(media_id, key, sniffed.mime_type, bytes.len() as u64, now_ms)
                .map_err(retry)?;
            if staged.is_some() {
                return Ok(FastPath::Hit);
            }
        }
    }
    Ok(FastPath::Miss)
}

/// `GET` a remote attachment, signed with the instance actor on same-origin
/// hops, following redirects manually under the URL policy.
///
/// Verbatim from `get_signed` in `media.rs`: every hop is revalidated with
/// `policy::check_url`, same-origin hops carry the HTTP signature while
/// cross-origin hops fetch unsigned (the strip is logged), and the redirect
/// chain stops at [`MAX_REDIRECT_HOPS`] with the same quarantine strings.
pub async fn signed_get(
    config: &FederationConfig<FedContext>,
    first_url: Url,
) -> Result<reqwest::Response, DeliveryError> {
    let ctx: &FedContext = config;
    let data = config.to_request_data();
    let first = first_url.clone();
    let mut url = first_url;
    for _hop in 0..=MAX_REDIRECT_HOPS {
        policy::check_url(&url, ctx.http.allow_http, &ctx.http.allowlist)
            .map_err(|e| DeliveryError::Quarantine(format!("attachment url refused: {e}")))?;
        let same_origin = redirect_keeps_signature(&first, &url);
        if data.can_sign_requests() && !same_origin {
            tracing::info!(
                target: "birchtree::federation",
                direction = "inbound",
                kind = "FetchRemoteMedia",
                from_host = first.host_str().unwrap_or("-"),
                to_host = url.host_str().unwrap_or("-"),
                outcome = "signature_stripped",
                "attachment redirect leaves the initial origin, fetching unsigned"
            );
        }
        let response = if data.can_sign_requests() && same_origin {
            let builder = reqwest_middleware::ClientWithMiddleware::from(ctx.media_client.clone())
                .get(url.as_str())
                .header(reqwest::header::ACCEPT, "*/*");
            let request = data
                .sign_request(builder, Bytes::new())
                .await
                .map_err(|e| {
                    DeliveryError::Retry(format!("signing the attachment fetch failed: {e}"))
                })?;
            ctx.media_client.execute(request).await
        } else {
            ctx.media_client
                .get(url.clone())
                .header(reqwest::header::ACCEPT, "*/*")
                .send()
                .await
        };
        let response = response.map_err(|e| DeliveryError::Retry(e.to_string()))?;
        if !response.status().is_redirection() {
            return Ok(response);
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                DeliveryError::Quarantine("attachment redirect has no location".into())
            })?;
        url = url.join(location).map_err(|e| {
            DeliveryError::Quarantine(format!("attachment redirect is invalid: {e}"))
        })?;
    }
    Err(DeliveryError::Quarantine(
        "attachment redirected too many times".into(),
    ))
}

/// Enforce the `Content-Length` advisory cap.
///
/// Verbatim from `fetch`: a declared size past [`MAX_REMOTE_MEDIA_BYTES`]
/// marks the row with `"attachment is larger than the limit"` and quarantines
/// with the same message. Returns the declared size (`None` when absent) so
/// the orchestrator can shrink the live reservation exactly as `fetch` does.
pub fn enforce_declared_cap(
    store: &dyn Store,
    media_id: Id,
    declared: Option<u64>,
    now_ms: u64,
) -> Result<Option<u64>, DeliveryError> {
    if let Some(declared) = declared {
        if declared > MAX_REMOTE_MEDIA_BYTES {
            let _ = store.mark_remote_media_failed(
                media_id,
                "attachment is larger than the limit",
                now_ms,
            );
            return Err(DeliveryError::Quarantine(
                "attachment is larger than the limit".into(),
            ));
        }
        return Ok(Some(declared));
    }
    Ok(None)
}

/// Whether one more stream chunk would overshoot the live reservation cap.
///
/// Pure predicate matching both `fetch` streaming arms byte-for-byte:
/// `collected + chunk_len > stream_cap` aborts with
/// `"attachment is larger than the limit"`. The caller performs the same
/// `mark_remote_media_failed` + `Quarantine` the original inline code does.
pub fn chunk_over_cap(collected: u64, chunk_len: u64, stream_cap: u64) -> bool {
    collected.saturating_add(chunk_len) > stream_cap
}

/// Settle staged bytes into stored state.
///
/// Thin verbatim delegate to `media::settle_staged`: N2 top-up, durable byte
/// row, ledger settlement, then the media-row staging that enqueues
/// `ProcessMedia`, with the same rollback of blob, durable row and ledger
/// mapping on every failure path. Keeping the call here (rather than copying
/// the ~100-line body) guarantees byte-identical commit behavior while the
/// parent owns the orchestration.
#[allow(clippy::too_many_arguments)]
pub fn commit_bytes<StoreT, BlobsT>(
    store: &StoreT,
    blobs: &BlobsT,
    reservation: &mut remote_cache::Reservation<'_>,
    source_url: &str,
    media_id: Id,
    blob_key: &str,
    staged: u64,
    mime_type: &str,
    now_ms: u64,
    cfg: &RemoteCacheConfig,
) -> Result<(), DeliveryError>
where
    StoreT: crate::federation::media::SettleStore + ?Sized,
    BlobsT: BlobStore + ?Sized,
{
    crate::federation::media::settle_staged(
        store,
        blobs,
        reservation,
        source_url,
        ATTACHMENT_VARIANT,
        media_id,
        blob_key,
        staged,
        mime_type,
        now_ms,
        cfg,
    )
}

/// Reserve worst-case budget for one fetch, evicting durable-first on pressure.
///
/// Verbatim from `media::fetch`: holds `MAX_REMOTE_MEDIA_BYTES` of `Full`
/// budget up front; only `BudgetExhausted` triggers the shared durable-first
/// eviction (unmap rows+counters in one transaction, delete blobs, ledger
/// catches up) followed by one retry of the reservation; any other refusal
/// maps through `media::ledger_outcome` with no eviction. The guard frees the
/// holding on every return below, so no path needs a manual release.
pub fn reserve_room(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    now_ms: u64,
) -> Result<remote_cache::Reservation<'static>, DeliveryError> {
    match remote_cache::Reservation::reserve(
        remote_cache::process_ledger(),
        remote_cache::MediaClass::Full,
        MAX_REMOTE_MEDIA_BYTES,
    ) {
        Ok(guard) => Ok(guard),
        Err(remote_cache::CacheError::BudgetExhausted { .. }) => {
            crate::media::cache_maintenance::evict_pressure_durable_first(
                store,
                blobs,
                None,
                remote_cache::process_ledger(),
                remote_cache::MediaClass::Full,
                MAX_REMOTE_MEDIA_BYTES,
                now_ms,
                crate::media::cache_maintenance::MAINTENANCE_BATCH,
            )
            .map_err(|err| DeliveryError::Retry(err.to_string()))?;
            remote_cache::Reservation::reserve(
                remote_cache::process_ledger(),
                remote_cache::MediaClass::Full,
                MAX_REMOTE_MEDIA_BYTES,
            )
            .map_err(crate::federation::media::ledger_outcome)
        }
        Err(other) => Err(crate::federation::media::ledger_outcome(other)),
    }
}

/// Map the attachment response head to a live stream cap.
///
/// Verbatim from `media::fetch`: 5xx/408/429 retry with
/// `"attachment server answered {status}"`; any other non-success marks the
/// row with the same string and quarantines; otherwise the declared size is
/// enforced by [`enforce_declared_cap`] and the reservation shrinks by
/// `remaining - declared` (a dishonest length cannot overshoot the budget
/// because the body arms still cap the stream at the live reservation).
/// Returns the live reservation: the cap both body arms stream under.
pub fn head_status_and_cap(
    store: &dyn Store,
    media_id: Id,
    status: reqwest::StatusCode,
    declared: Option<u64>,
    reservation: &mut remote_cache::Reservation<'_>,
    now_ms: u64,
) -> Result<u64, DeliveryError> {
    if status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err(DeliveryError::Retry(format!(
            "attachment server answered {status}"
        )));
    }
    if !status.is_success() {
        let _ = store.mark_remote_media_failed(
            media_id,
            &format!("attachment server answered {status}"),
            now_ms,
        );
        return Err(DeliveryError::Quarantine(format!(
            "attachment server answered {status}"
        )));
    }
    // `Content-Length` is advisory: shrink the reservation to the declared
    // size, but cap the body stream at the live reservation below.
    if let Some(declared) = enforce_declared_cap(store, media_id, declared, now_ms)? {
        reservation.shrink(reservation.remaining().saturating_sub(declared));
    }
    Ok(reservation.remaining())
}

/// Stream the response body to a staging tmp file under the live cap, then settle.
///
/// Verbatim from the staging-root arm of `media::fetch`: the body streams to
/// `tmp/{rand}.part` under `stream_cap` and renames onto the key — no `Vec`
/// buffering. The classifier only reads a 64 KiB prefix (magic + `ftyp`
/// brand classifies exactly like the full body). Audio/video originals are
/// `Stream` — never persisted: the tmp is removed, the row is marked with the
/// shared never-cached reason, and the job completes successfully with no blob
/// and no ledger movement. Otherwise the tmp is published first (a crash
/// leaves at most an uncounted orphan blob, never counted bytes without a
/// blob) and [`commit_bytes`] settles.
pub async fn stage_streamed(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    response: reqwest::Response,
    staging_root: std::path::PathBuf,
    stream_cap: u64,
    reservation: &mut remote_cache::Reservation<'_>,
    source_url: &str,
    media_id: Id,
    blob_key: &str,
    now_ms: u64,
    cfg: &RemoteCacheConfig,
) -> Result<(), DeliveryError> {
    use tokio::io::AsyncReadExt as _;

    let tmp = crate::media::fetch_stream::tmp_path_for(&staging_root)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    let stream = response.bytes_stream();
    let staged = crate::media::fetch_stream::copy_stream_capped(stream, &tmp, stream_cap)
        .await
        .map_err(|e| match e {
            crate::domain::Error::InvalidInput(_) => {
                let _ = store.mark_remote_media_failed(
                    media_id,
                    "attachment is larger than the limit",
                    now_ms,
                );
                DeliveryError::Quarantine("attachment is larger than the limit".into())
            }
            other => DeliveryError::Retry(other.to_string()),
        })?;
    // Sniff from a bounded prefix: the classifier only reads leading bytes.
    let prefix = {
        let file = tokio::fs::File::open(&tmp).await.map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DeliveryError::Retry(e.to_string())
        })?;
        let mut buf = Vec::new();
        file.take(65536).read_to_end(&mut buf).await.map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DeliveryError::Retry(e.to_string())
        })?;
        buf
    };
    let sniffed = match ingest::sniff(&prefix).filter(|s| ingest::validate(*s)) {
        Some(s) => s,
        None => {
            let _ = std::fs::remove_file(&tmp);
            let _ = store.mark_remote_media_failed(
                media_id,
                "attachment is not a supported media file",
                now_ms,
            );
            return Err(DeliveryError::Quarantine(
                "attachment is not a supported media file".into(),
            ));
        }
    };
    // Classify AFTER the sniff, never assume `Full` (adversarial-2 D2 stage 1).
    if remote_cache::class_for_fetch(sniffed.mime_type, ATTACHMENT_VARIANT)
        == remote_cache::MediaClass::Stream
    {
        let _ = std::fs::remove_file(&tmp);
        let reason = "remote audio/video streams from its origin and is never cached";
        let _ = store.mark_remote_media_failed(media_id, reason, now_ms);
        return Ok(());
    }
    blobs.publish_tmp(&tmp, blob_key).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DeliveryError::Retry(e.to_string())
    })?;
    commit_bytes(
        store,
        blobs,
        reservation,
        source_url,
        media_id,
        blob_key,
        staged,
        sniffed.mime_type,
        now_ms,
        cfg,
    )
}

/// Buffer the response body under the live cap, then settle.
///
/// Verbatim from the legacy arm of `media::fetch` (handles without a staging
/// root, e.g. in-memory doubles): chunk loop aborts past `stream_cap` with
/// `"attachment is larger than the limit"`; sniff + `validate` else
/// `"attachment is not a supported media file"`; audio/video originals take
/// the same never-persisted `Stream` outcome as the staged path (no `put`, no
/// commit, job `Ok`, row marked); otherwise `put` then [`commit_bytes`].
pub async fn stage_buffered(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    response: reqwest::Response,
    stream_cap: u64,
    reservation: &mut remote_cache::Reservation<'_>,
    source_url: &str,
    media_id: Id,
    blob_key: &str,
    now_ms: u64,
    cfg: &RemoteCacheConfig,
) -> Result<(), DeliveryError> {
    use futures_util::StreamExt as _;

    let mut body: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DeliveryError::Retry(e.to_string()))?;
        if chunk_over_cap(body.len() as u64, chunk.len() as u64, stream_cap) {
            let _ = store.mark_remote_media_failed(
                media_id,
                "attachment is larger than the limit",
                now_ms,
            );
            return Err(DeliveryError::Quarantine(
                "attachment is larger than the limit".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let sniffed = match ingest::sniff(&body).filter(|s| ingest::validate(*s)) {
        Some(s) => s,
        None => {
            let _ = store.mark_remote_media_failed(
                media_id,
                "attachment is not a supported media file",
                now_ms,
            );
            return Err(DeliveryError::Quarantine(
                "attachment is not a supported media file".into(),
            ));
        }
    };
    // Same classification as the streamed path: audio/video originals are
    // `Stream` and are never persisted (no put, no commit). The guard frees
    // the reservation. The job completes successfully with the row marked
    // non-cached; clients use `remote_url`.
    if remote_cache::class_for_fetch(sniffed.mime_type, ATTACHMENT_VARIANT)
        == remote_cache::MediaClass::Stream
    {
        let reason = "remote audio/video streams from its origin and is never cached";
        let _ = store.mark_remote_media_failed(media_id, reason, now_ms);
        return Ok(());
    }
    blobs
        .put(blob_key, &body)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    commit_bytes(
        store,
        blobs,
        reservation,
        source_url,
        media_id,
        blob_key,
        body.len() as u64,
        sniffed.mime_type,
        now_ms,
        cfg,
    )
}

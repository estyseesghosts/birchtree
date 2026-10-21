//! Remote media on demand (Phase 6 + storage overhaul, Agent B).
//!
//! Ingest records attachment *metadata* only (source URL + a metadata shell in
//! the cache ledger; the durable `REMOTE_CACHE` row lands with the parent's
//! redb functions). Bytes are fetched on first request — or by scoped warming,
//! which reuses the same `FetchRemoteMedia` job behind the config flag — with
//! an in-flight reservation so concurrent downloads cannot overshoot the
//! global cap. Eviction removes the mapping first, updates accounting, then
//! deletes the row's own `remote/{media_id}` key (per-row keys retained, no
//! sharing, no refcount; durable local media is never touched).
//!
//! The enforcement shape is unchanged: SSRF URL policy, the resolver's
//! connect checks, `redirect(Policy::none)` with 5s/10s timeouts
//! (`federation::net`), `Content-Length` advisory check, streaming byte-count
//! abort, `ingest::sniff` + `validate`. `MAX_REMOTE_MEDIA_BYTES` stays the
//! per-file ceiling inside the class/global budgets.
//!
//! Open gap (carried, not absorbed): this fetch sends an unsigned `GET` with
//! `Accept: */*` while `federation-contract.md` requires instance-actor-signed
//! dereferences — see `docs/database.md` §"Open gap".

use futures_util::StreamExt;
use url::Url;

use crate::activitypub::FedContext;
use crate::application::federation_ports::FederationContentStore;
use crate::application::ports::BlobStore;
use crate::config::RemoteCacheConfig;
use crate::domain::{Id, Job};
use crate::federation::delivery::DeliveryError;
use crate::federation::policy;
use crate::media::ingest;
use crate::media::remote_cache::{self, CacheError, CacheLedger, MediaClass};

/// The largest remote attachment accepted (Mastodon's default upload limit: 40 MiB).
/// Stays the per-file ceiling inside the class/global budgets (not a config key).
pub const MAX_REMOTE_MEDIA_BYTES: u64 = 40 * 1024 * 1024;

/// Attachments are fetched as full originals (`variant "original"`).
const ATTACHMENT_VARIANT: &str = "original";

/// Per-row blob key for a remote attachment (retained v1 decision: no
/// content-addressed sharing, no refcount; safe eviction deletes this key).
pub fn cache_key_for(media_id: Id) -> String {
    remote_cache::blob_key_for_media(media_id.value())
}

/// The cache store. Today this is the shared blob root; the parent repoints
/// it at Agent A's `media_cache/` root after the store split lands. The key
/// namespace (`remote/…`) already isolates remote bytes either way.
fn cache_store(ctx: &FedContext) -> &dyn BlobStore {
    &*ctx.app.blobs
}

fn lock_ledger() -> Result<std::sync::MutexGuard<'static, CacheLedger>, DeliveryError> {
    remote_cache::process_ledger()
        .lock()
        .map_err(|e| DeliveryError::Retry(format!("cache ledger poisoned: {e}")))
}

/// Map a ledger refusal to a job outcome: budget/store/origin pressure is
/// transient (retry with backoff); oversize/unsupported/missing is permanent
/// (quarantine, like the existing failure arms below).
fn ledger_outcome(err: CacheError) -> DeliveryError {
    match &err {
        CacheError::BudgetExhausted { .. }
        | CacheError::Store(_)
        | CacheError::OriginDown(_) => DeliveryError::Retry(err.message()),
        CacheError::Oversize { .. } | CacheError::Unsupported(_) | CacheError::Missing(_) => {
            DeliveryError::Quarantine(err.message())
        }
    }
}

/// Record attachment metadata first (metadata-first ingest): the durable
/// source URL plus a metadata shell in the process ledger. No bytes, no job
/// enqueue here — the fetch happens on first request (miss path) or via
/// scoped warming, both of which enqueue `FetchRemoteMedia`.
///
/// Called by the ingest trigger conversion (`federation_inbound.rs`,
/// parent-owned); kept here so the metadata shape lives with the fetch path.
pub fn record_metadata_first(
    store: &dyn FederationContentStore,
    media_id: Id,
    source_url: &str,
    now_ms: u64,
) -> Result<remote_cache::RemoteCacheEntry, CacheError> {
    if source_url.is_empty() {
        return Err(CacheError::Missing("attachment has no source".into()));
    }
    store
        .set_remote_media_source(media_id, source_url)
        .map_err(|e| CacheError::Store(e.to_string()))?;
    let mut ledger = remote_cache::process_ledger()
        .lock()
        .map_err(|e| CacheError::Store(format!("cache ledger poisoned: {e}")))?;
    let entry = remote_cache::RemoteCacheEntry::metadata_only(
        source_url,
        ATTACHMENT_VARIANT,
        MediaClass::Full,
        &remote_cache::blob_key_for_media(media_id.value()),
        now_ms,
        &RemoteCacheConfig::default(),
    );
    ledger.record_metadata(entry.clone());
    Ok(entry)
}

/// Scoped-warming gate: off by default, always relay-excluded. Warming reuses
/// `FetchRemoteMedia` (same job, same fetch below); only the enqueue site
/// differs (parent-owned trigger + endpoints).
pub fn should_warm(cfg: &RemoteCacheConfig, is_relay: bool) -> bool {
    remote_cache::should_warm(cfg, is_relay)
}

/// Fetch and stage one remote attachment (the `FetchRemoteMedia` job).
///
/// Metadata-first + fetch-on-first-request: a present blob is a hit (the
/// losing racer / retried job does no download). Otherwise the worst case
/// (`MAX_REMOTE_MEDIA_BYTES`) is reserved before the first byte is read and
/// shrunk once `Content-Length` is known, so concurrent fetches cannot
/// overshoot the global cap.
pub async fn fetch(
    config: &activitypub_federation::config::FederationConfig<FedContext>,
    job: &Job,
) -> std::result::Result<(), DeliveryError> {
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

    let key = cache_key_for(media_id);
    let now = ctx.app.now_ms();

    // Fast path: bytes already cached (first request won, or a retried job).
    // Count them if the ledger has not seen them (restart / pre-split rows),
    // refresh last-use, and make sure the row is staged.
    if cache_store(ctx).get(&key).map_err(retry)?.is_some() {
        {
            let mut ledger = lock_ledger()?;
            let cache_key =
                crate::persistence::redb::schema::remote_cache_key(&source, ATTACHMENT_VARIANT);
            let seen = ledger.get(&cache_key).is_some_and(|e| e.has_bytes);
            if !seen {
                // Size is advisory here (already on disk); count, don't refuse.
                if let Ok(Some(bytes)) = cache_store(ctx).get(&key).map_err(|_| ()) {
                    let entry = remote_cache::RemoteCacheEntry {
                        source_url: source.clone(),
                        variant: ATTACHMENT_VARIANT.to_string(),
                        blob_key: key.clone(),
                        class: MediaClass::Full,
                        size: bytes.len() as u64,
                        last_use_ms: now,
                        expiry_ms: now.saturating_add(MediaClass::Full.retention_ms(
                            &RemoteCacheConfig::default(),
                        )),
                        etag: None,
                        has_bytes: true,
                    };
                    let _ = ledger.commit(MediaClass::Full, 0, entry);
                }
            } else {
                ledger.touch(&cache_key, now);
            }
        }
        if let Ok(Some(row)) = store.media_by_id(media_id).map_err(|_| None::<()>) {
            if !row.file_key.is_empty() {
                return Ok(());
            }
        }
        // Row unstaged but bytes present (torn write): heal below by staging.
        if let Ok(Some(bytes)) = cache_store(ctx).get(&key).map_err(|_| None::<()>) {
            if bytes.len() as u64 <= MAX_REMOTE_MEDIA_BYTES {
                if let Some(sniffed) = ingest::sniff(&bytes).filter(|s| ingest::validate(*s)) {
                    let staged = store
                        .stage_remote_media(
                            media_id,
                            &key,
                            sniffed.mime_type,
                            bytes.len() as u64,
                            now,
                        )
                        .map_err(retry)?;
                    if staged.is_some() {
                        return Ok(());
                    }
                }
            }
        }
    }

    // Worst-case reservation first: concurrent fetches each hold their ceiling
    // until headers shrink it, so the committed total never overshoots.
    let mut reserved = MAX_REMOTE_MEDIA_BYTES;
    {
        let mut ledger = lock_ledger()?;
        if let Err(err) = ledger.reserve(MediaClass::Full, reserved) {
            // Make room (mapping first, accounting, then delete own keys),
            // then reserve once. Refusal after full eviction is transient.
            match ledger.evict_for(MediaClass::Full, reserved, now) {
                Ok(victims) => {
                    drop(ledger);
                    for victim in &victims {
                        cache_store(ctx)
                            .delete(victim)
                            .map_err(|e| DeliveryError::Retry(e.to_string()))?;
                    }
                    let mut ledger = lock_ledger()?;
                    ledger.reserve(MediaClass::Full, reserved).map_err(ledger_outcome)?;
                }
                Err(evict_err) => {
                    let _ = err;
                    return Err(ledger_outcome(evict_err));
                }
            }
        }
    }
    let release = |ledger: &mut CacheLedger, amount: u64| {
        ledger.release(MediaClass::Full, amount);
    };

    let response = ctx
        .media_client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "*/*")
        .send()
        .await
        .map_err(|e| {
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            DeliveryError::Retry(e.to_string())
        })?;
    let status = response.status();
    if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
            release(&mut ledger, reserved);
        }
        return Err(DeliveryError::Retry(format!("attachment server answered {status}")));
    }
    if !status.is_success() {
        if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
            release(&mut ledger, reserved);
        }
        let _ = store.mark_remote_media_failed(media_id, &format!("attachment server answered {status}"), ctx.app.now_ms());
        return Err(DeliveryError::Quarantine(format!("attachment server answered {status}")));
    }
    // `Content-Length` is advisory: shrink the reservation to the declared
    // size, but keep the streaming abort below for dishonest lengths.
    if let Some(declared) = response.content_length() {
        if declared > MAX_REMOTE_MEDIA_BYTES {
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            let _ = store.mark_remote_media_failed(media_id, "attachment is larger than the limit", ctx.app.now_ms());
            return Err(DeliveryError::Quarantine("attachment is larger than the limit".into()));
        }
        if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
            release(&mut ledger, reserved.saturating_sub(declared));
        }
        reserved = declared;
    }
    // Stream-to-disk (storage overhaul, Agent C): when the blob handle
    // exposes its filesystem root, the body streams to `tmp/{rand}.part`
    // under the cap and renames onto the key — no `Vec` buffering. Handles
    // without a root (in-memory doubles) keep the legacy buffered path.
    if let Some(root) = cache_store(ctx).root_path() {
        use tokio::io::AsyncReadExt as _;

        let tmp = crate::media::fetch_stream::tmp_path_for(&root).map_err(|e| {
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            DeliveryError::Retry(e.to_string())
        })?;
        let stream = response.bytes_stream();
        let staged =
            crate::media::fetch_stream::copy_stream_capped(stream, &tmp, MAX_REMOTE_MEDIA_BYTES)
                .await
                .map_err(|e| match e {
                    crate::domain::Error::InvalidInput(_) => {
                        if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                            release(&mut ledger, reserved);
                        }
                        let _ = store.mark_remote_media_failed(
                            media_id,
                            "attachment is larger than the limit",
                            ctx.app.now_ms(),
                        );
                        DeliveryError::Quarantine("attachment is larger than the limit".into())
                    }
                    other => {
                        if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                            release(&mut ledger, reserved);
                        }
                        DeliveryError::Retry(other.to_string())
                    }
                })?;
        // Sniff from a bounded prefix: the classifier only reads leading
        // bytes (magic + `ftyp` brand), so 64 KiB classifies every accepted
        // format exactly like the full body did.
        let prefix = {
            let file = tokio::fs::File::open(&tmp).await.map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                    release(&mut ledger, reserved);
                }
                DeliveryError::Retry(e.to_string())
            })?;
            let mut buf = Vec::new();
            file.take(65536).read_to_end(&mut buf).await.map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                    release(&mut ledger, reserved);
                }
                DeliveryError::Retry(e.to_string())
            })?;
            buf
        };
        let sniffed = match ingest::sniff(&prefix).filter(|s| ingest::validate(*s)) {
            Some(s) => s,
            None => {
                let _ = std::fs::remove_file(&tmp);
                if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                    release(&mut ledger, reserved);
                }
                let _ = store.mark_remote_media_failed(
                    media_id,
                    "attachment is not a supported media file",
                    ctx.app.now_ms(),
                );
                return Err(DeliveryError::Quarantine(
                    "attachment is not a supported media file".into(),
                ));
            }
        };
        // Publish first, then settle the reservation into stored bytes: a
        // crash between leaves at most an uncounted orphan blob for the
        // reconciler, never counted bytes without a blob.
        cache_store(ctx).publish_tmp(&tmp, &key).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            DeliveryError::Retry(e.to_string())
        })?;
        // Settle the reservation into stored bytes, then stage the row (which
        // enqueues `ProcessMedia`, preserving the existing chain).
        let entry = remote_cache::RemoteCacheEntry::stored(
            &source,
            ATTACHMENT_VARIANT,
            MediaClass::Full,
            &key,
            staged,
            now,
            &RemoteCacheConfig::default(),
        );
        if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
            if let Err(err) = ledger.commit(MediaClass::Full, reserved, entry) {
                release(&mut ledger, 0);
                let _ = cache_store(ctx).delete(&key);
                return Err(ledger_outcome(err));
            }
        }
        let now = ctx.app.now_ms();
        match store
            .stage_remote_media(media_id, &key, sniffed.mime_type, staged, now)
            .map_err(retry)?
        {
            Some(_) => {
                tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "FetchRemoteMedia", media = media_id.value(), bytes = staged, mime = sniffed.mime_type, outcome = "staged", "media");
                return Ok(());
            }
            None => {
                // Row gone after the bytes landed: drop the bytes (own key
                // only) and the ledger mapping so no orphan survives uncounted.
                let _ = cache_store(ctx).delete(&key);
                if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                    let cache_key = crate::persistence::redb::schema::remote_cache_key(
                        &source,
                        ATTACHMENT_VARIANT,
                    );
                    ledger.evict_one(&cache_key);
                }
                let _ = store.mark_remote_media_failed(
                    media_id,
                    "attachment row is missing",
                    ctx.app.now_ms(),
                );
                return Err(DeliveryError::Quarantine("attachment row is missing".into()));
            }
        }
    }
    let mut body: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            DeliveryError::Retry(e.to_string())
        })?;
        if body.len() as u64 + chunk.len() as u64 > MAX_REMOTE_MEDIA_BYTES {
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            let _ = store.mark_remote_media_failed(media_id, "attachment is larger than the limit", ctx.app.now_ms());
            return Err(DeliveryError::Quarantine("attachment is larger than the limit".into()));
        }
        body.extend_from_slice(&chunk);
    }
    let sniffed = match ingest::sniff(&body).filter(|s| ingest::validate(*s)) {
        Some(s) => s,
        None => {
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            let _ = store.mark_remote_media_failed(media_id, "attachment is not a supported media file", ctx.app.now_ms());
            return Err(DeliveryError::Quarantine("attachment is not a supported media file".into()));
        }
    };

    cache_store(ctx)
        .put(&key, &body)
        .map_err(|e| {
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                release(&mut ledger, reserved);
            }
            DeliveryError::Retry(e.to_string())
        })?;
    // Settle the reservation into stored bytes, then stage the row (which
    // enqueues `ProcessMedia`, preserving the existing chain).
    let entry = remote_cache::RemoteCacheEntry::stored(
        &source,
        ATTACHMENT_VARIANT,
        MediaClass::Full,
        &key,
        body.len() as u64,
        now,
        &RemoteCacheConfig::default(),
    );
    if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
        if let Err(err) = ledger.commit(MediaClass::Full, reserved, entry) {
            release(&mut ledger, 0);
            let _ = cache_store(ctx).delete(&key);
            return Err(ledger_outcome(err));
        }
    }
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
            // Row gone after the bytes landed: drop the bytes (own key only)
            // and the ledger mapping so no orphan survives uncounted.
            let _ = cache_store(ctx).delete(&key);
            if let Ok(mut ledger) = remote_cache::process_ledger().lock() {
                let cache_key =
                    crate::persistence::redb::schema::remote_cache_key(&source, ATTACHMENT_VARIANT);
                ledger.evict_one(&cache_key);
            }
            let _ = store.mark_remote_media_failed(media_id, "attachment row is missing", ctx.app.now_ms());
            Err(DeliveryError::Quarantine("attachment row is missing".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_matches_ledger_policy() {
        assert_eq!(MAX_REMOTE_MEDIA_BYTES, remote_cache::PER_FILE_CEILING_BYTES);
    }

    #[test]
    fn per_row_keys_have_no_sharing() {
        let a = cache_key_for(Id::new(7));
        let b = cache_key_for(Id::new(8));
        assert_eq!(a, "remote/7");
        assert_ne!(a, b);
        assert!(remote_cache::is_cache_key(&a));
        assert!(!remote_cache::is_cache_key("000/000/007/original/a.jpg"));
    }

    #[test]
    fn warming_is_off_and_relay_excluded() {
        let cfg = RemoteCacheConfig::default();
        assert!(!cfg.warming);
        assert!(!should_warm(&cfg, false));
        let on = RemoteCacheConfig { warming: true, ..RemoteCacheConfig::default() };
        assert!(should_warm(&on, false));
        assert!(!should_warm(&on, true));
    }
}

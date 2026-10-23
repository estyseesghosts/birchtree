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
//! The fetch is instance-actor-signed on same-origin hops
//! ([`media_fetch::signed_get`]):
//! when an instance actor is configured each `GET` whose URL shares the
//! initial URL's origin carries its HTTP signature, so authorized-fetch peers
//! accept the dereference; without a configured actor the fetch is an unsigned
//! `GET` with `Accept: */*`. A redirect that leaves the initial origin is
//! still followed while the SSRF URL policy allows it, but unsigned (the
//! strip is logged): the signature authenticates us to the media's own host,
//! and an unrelated third party must never receive it. Every hop is
//! revalidated against the SSRF URL policy before it is requested.
//!
//! The enforcement shape is unchanged: SSRF URL policy, the resolver's
//! connect checks, `redirect(Policy::none)` with 5s/10s timeouts
//! (`federation::net`), `Content-Length` advisory check, streaming byte-count
//! abort, `ingest::sniff` + `validate`. `MAX_REMOTE_MEDIA_BYTES` stays the
//! per-file ceiling inside the class/global budgets.

use std::sync::Mutex;

use url::Url;

use crate::activitypub::FedContext;
use crate::application::federation_ports::FederationContentStore;
use crate::application::ports::{BlobStore, CacheStore};
use crate::config::RemoteCacheConfig;
use crate::domain::{Id, Job};
use crate::federation::delivery::DeliveryError;
use crate::federation::media_fetch;
use crate::federation::policy;
use crate::media::ingest;
use crate::media::remote_cache::{self, CacheError, MediaClass};

/// The largest remote attachment accepted (Mastodon's default upload limit: 40 MiB).
/// Stays the per-file ceiling inside the class/global budgets (not a config key).
pub const MAX_REMOTE_MEDIA_BYTES: u64 = 40 * 1024 * 1024;

/// Attachments are fetched as full originals (`variant "original"`).
const ATTACHMENT_VARIANT: &str = "original";

/// Redirect hops followed for one attachment (each hop is revalidated).
pub const MAX_REDIRECT_HOPS: usize = 5;

/// Whether a redirect hop keeps the instance-actor HTTP signature.
///
/// Policy: sign only same-origin hops, where same-origin means the hop's URL
/// shares the initial URL's origin (scheme, host, port). A hop that leaves
/// the origin is still fetched when the SSRF URL policy allows it — it is
/// public, just unrelated — but without the `Signature` header, so a
/// third-party host never receives an authentication token minted for someone
/// else. There is no opt-in to re-sign cross-origin hops.
///
/// This is distinct from SSRF refusal: a non-public hop is never requested at
/// all ([`crate::federation::policy::check_url`] quarantines it first); a
/// cross-origin public hop is requested unsigned, and the strip is logged by
/// [`media_fetch::signed_get`].
pub fn redirect_keeps_signature(first: &Url, next: &Url) -> bool {
    first.origin() == next.origin()
}

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

/// Map a ledger refusal to a job outcome: budget/store/origin pressure is
/// transient (retry with backoff); oversize/unsupported/missing is permanent
/// (quarantine, like the existing failure arms below).
pub(crate) fn ledger_outcome(err: CacheError) -> DeliveryError {
    match &err {
        CacheError::BudgetExhausted { .. } | CacheError::Store(_) | CacheError::OriginDown(_) => {
            DeliveryError::Retry(err.message())
        }
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
    let entry_ctx = remote_cache::CacheEntryCtx {
        source_url,
        variant: ATTACHMENT_VARIANT,
        class: MediaClass::Full,
        blob_key: &remote_cache::blob_key_for_media(media_id.value()),
        now_ms,
        cfg: &RemoteCacheConfig::default(),
    };
    let entry = remote_cache::RemoteCacheEntry::metadata_only(&entry_ctx);
    ledger.record_metadata(entry.clone());
    Ok(entry)
}

/// Scoped-warming gate: off by default, always relay-excluded. Warming reuses
/// `FetchRemoteMedia` (same job, same fetch below); only the enqueue site
/// differs (parent-owned trigger + endpoints).
pub fn should_warm(cfg: &RemoteCacheConfig, is_relay: bool) -> bool {
    remote_cache::should_warm(cfg, is_relay)
}

/// The durable writes the fetch settle step needs: byte-row accounting plus
/// the media-row staging. Blanket-implemented for every content store, so
/// production passes the job store straight through while tests pass a
/// fault-injecting wrapper through the same seams instead of a 300-method
/// full-`Store` double.
pub trait SettleStore: Send + Sync {
    fn cache_mark_stored(
        &self,
        source_url: &str,
        variant: &str,
        blob_key: &str,
        size: u64,
        etag: Option<String>,
        now_ms: u64,
        cfg: &RemoteCacheConfig,
    ) -> crate::domain::Result<()>;
    fn cache_mark_evicted(
        &self,
        source_url: &str,
        variant: &str,
    ) -> crate::domain::Result<Option<String>>;
    fn stage_remote_media(
        &self,
        media_id: Id,
        file_key: &str,
        mime_type: &str,
        file_size: u64,
        now_ms: u64,
    ) -> crate::domain::Result<Option<crate::domain::MediaAttachment>>;
    fn mark_remote_media_failed(
        &self,
        media_id: Id,
        reason: &str,
        now_ms: u64,
    ) -> crate::domain::Result<()>;
}

impl<T> SettleStore for T
where
    T: CacheStore + FederationContentStore + ?Sized,
{
    fn cache_mark_stored(
        &self,
        source_url: &str,
        variant: &str,
        blob_key: &str,
        size: u64,
        etag: Option<String>,
        now_ms: u64,
        cfg: &RemoteCacheConfig,
    ) -> crate::domain::Result<()> {
        CacheStore::cache_mark_stored(self, source_url, variant, blob_key, size, etag, now_ms, cfg)
    }

    fn cache_mark_evicted(
        &self,
        source_url: &str,
        variant: &str,
    ) -> crate::domain::Result<Option<String>> {
        CacheStore::cache_mark_evicted(self, source_url, variant)
    }

    fn stage_remote_media(
        &self,
        media_id: Id,
        file_key: &str,
        mime_type: &str,
        file_size: u64,
        now_ms: u64,
    ) -> crate::domain::Result<Option<crate::domain::MediaAttachment>> {
        FederationContentStore::stage_remote_media(
            self, media_id, file_key, mime_type, file_size, now_ms,
        )
    }

    fn mark_remote_media_failed(
        &self,
        media_id: Id,
        reason: &str,
        now_ms: u64,
    ) -> crate::domain::Result<()> {
        FederationContentStore::mark_remote_media_failed(self, media_id, reason, now_ms)
    }
}

/// Undo a settle: blob, durable row and ledger mapping (all best-effort).
fn rollback_settle<StoreT, BlobsT>(
    store: &StoreT,
    blobs: &BlobsT,
    cache_key: &[u8],
    reservation: &remote_cache::Reservation<'_>,
    source_url: &str,
    variant: &str,
    blob_key: &str,
) where
    StoreT: SettleStore + ?Sized,
    BlobsT: BlobStore + ?Sized,
{
    let _ = blobs.delete(blob_key);
    let _ = store.cache_mark_evicted(source_url, variant);
    reservation.evict_key(cache_key);
}

/// Settle staged bytes into stored state: N2 top-up, durable byte row (F6),
/// ledger settlement (F6), then the media-row staging that enqueues
/// `ProcessMedia`. Every failure rolls back what the earlier steps wrote —
/// blob, durable row and ledger mapping — and frees the reservation through
/// the guard, so a failed fetch leaves no orphan blob, no row claiming
/// bytes and no in-flight budget behind.
// WHY: settle is one atomic commit (ledger + durable row + media row) with a single
// rollback shape; the 11 params are the row identity and S7 owns any further split.
#[allow(clippy::too_many_arguments)]
pub fn settle_staged<StoreT, BlobsT>(
    store: &StoreT,
    blobs: &BlobsT,
    reservation: &mut remote_cache::Reservation<'_>,
    source_url: &str,
    variant: &str,
    media_id: Id,
    blob_key: &str,
    staged: u64,
    mime_type: &str,
    now_ms: u64,
    cfg: &RemoteCacheConfig,
) -> std::result::Result<(), DeliveryError>
where
    StoreT: SettleStore + ?Sized,
    BlobsT: BlobStore + ?Sized,
{
    // N2: the body stream was capped at the live reservation; a larger
    // staged size tops the reservation back up, or fails loudly.
    if staged > reservation.remaining() {
        if let Err(err) = reservation.grow(staged - reservation.remaining()) {
            let _ = blobs.delete(blob_key);
            return Err(ledger_outcome(err));
        }
    }
    // Durable byte row first (mapping + counters in one transaction):
    // admission eviction selects victims from these rows, so bytes without
    // a row could never be freed. On failure the published blob goes too.
    if let Err(err) =
        store.cache_mark_stored(source_url, variant, blob_key, staged, None, now_ms, cfg)
    {
        let _ = blobs.delete(blob_key);
        return Err(DeliveryError::Retry(err.to_string()));
    }
    // Ledger settlement. On refusal the durable row goes too, not just the
    // blob: a row claiming deleted bytes would never heal.
    let entry = remote_cache::RemoteCacheEntry::stored(
        &remote_cache::CacheEntryCtx {
            source_url,
            variant,
            class: MediaClass::Full,
            blob_key,
            now_ms,
            cfg,
        },
        staged,
    );
    let cache_key = entry.cache_key();
    let reserved = reservation.remaining();
    if let Err(err) = reservation.commit_entry(reserved, entry) {
        rollback_settle(
            store,
            blobs,
            &cache_key,
            reservation,
            source_url,
            variant,
            blob_key,
        );
        return Err(ledger_outcome(err));
    }
    // Stage the row (which enqueues `ProcessMedia`, preserving the existing
    // chain). Any failure unwinds blob, durable row and ledger mapping.
    let staged_row = match store.stage_remote_media(media_id, blob_key, mime_type, staged, now_ms) {
        Ok(row) => row,
        Err(err) => {
            rollback_settle(
                store,
                blobs,
                &cache_key,
                reservation,
                source_url,
                variant,
                blob_key,
            );
            return Err(DeliveryError::Retry(err.to_string()));
        }
    };
    match staged_row {
        Some(_) => {
            tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "FetchRemoteMedia", media = media_id.value(), bytes = staged, mime = mime_type, outcome = "staged", "media");
            Ok(())
        }
        None => {
            // Row gone after the bytes landed: drop the bytes (own key
            // only) and the ledger mapping so no orphan survives uncounted.
            rollback_settle(
                store,
                blobs,
                &cache_key,
                reservation,
                source_url,
                variant,
                blob_key,
            );
            let _ = store.mark_remote_media_failed(media_id, "attachment row is missing", now_ms);
            Err(DeliveryError::Quarantine(
                "attachment row is missing".into(),
            ))
        }
    }
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
    let blobs = cache_store(ctx);
    let now = ctx.app.now_ms();
    let media_id = media_fetch::parse_job(job)?;
    let source = media_fetch::load_source(store, media_id, now)?;
    let url = Url::parse(&source).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
    if let Err(e) = media_fetch::gate_url(&url, ctx.http.allow_http, &ctx.http.allowlist) {
        if let DeliveryError::Quarantine(reason) = &e {
            let _ = store.mark_remote_media_failed(media_id, reason, now);
        }
        return Err(e);
    }
    let key = cache_key_for(media_id);
    if media_fetch::fast_path_if_cached(
        store,
        blobs,
        &source,
        media_id,
        &key,
        now,
        &ctx.remote_cache,
    )? == media_fetch::FastPath::Hit
    {
        return Ok(());
    }
    let mut reservation = media_fetch::reserve_room(store, blobs, now)?;
    let response = media_fetch::signed_get(config, url).await?;
    let stream_cap = media_fetch::head_status_and_cap(
        store,
        media_id,
        response.status(),
        response.content_length(),
        &mut reservation,
        now,
    )?;
    if let Some(root) = blobs.staging_root() {
        return media_fetch::stage_streamed(
            store,
            blobs,
            response,
            root,
            stream_cap,
            &mut reservation,
            &source,
            media_id,
            &key,
            now,
            &ctx.remote_cache,
        )
        .await;
    }
    media_fetch::stage_buffered(
        store,
        blobs,
        response,
        stream_cap,
        &mut reservation,
        &source,
        media_id,
        &key,
        now,
        &ctx.remote_cache,
    )
    .await
}

// ---------------------------------------------------------------------------
// Remote emoji ingest (federation tail, G-D): the tail arm that turns one
// parsed [`crate::activitypub::objects::RemoteEmoji`] into bytes + a row.
// ---------------------------------------------------------------------------

/// Largest accepted remote emoji image (Mastodon `CustomEmoji::LIMIT`, the
/// same 256 KiB as [`crate::domain::CustomEmoji::IMAGE_LIMIT`]).
pub const MAX_REMOTE_EMOJI_BYTES: u64 = 256 * 1024;
/// Longest accepted remote emoji source URL (untrusted input bound).
pub const MAX_REMOTE_EMOJI_URL_LEN: usize = 2048;

/// Storage key for a remote emoji row, namespaced by origin domain so a
/// remote `:party:` never collides with a local one. Interim shape: the
/// `CustomEmoji` table is keyed by shortcode only, so remote rows live under
/// `{shortcode}@{domain}` (a real `(domain, shortcode)` table is the
/// parent-merge snippet; the payload renderer strips the suffix back).
pub fn remote_emoji_shortcode(shortcode: &str, domain: &str) -> String {
    format!("{}@{}", shortcode, domain.to_ascii_lowercase())
}

/// Split an interim remote-emoji key back into `(shortcode, domain)`.
pub fn split_remote_emoji_shortcode(stored: &str) -> Option<(&str, &str)> {
    let (shortcode, domain) = stored.rsplit_once('@')?;
    if shortcode.is_empty() || domain.is_empty() {
        return None;
    }
    Some((shortcode, domain))
}

/// Validate one remote emoji reference before any fetch: shortcode shape
/// (Mastodon `SHORTCODE_RE_FRAGMENT`), a non-empty origin domain, and an
/// `http(s)` source URL without credentials. Returns the parsed source URL.
pub fn validate_remote_emoji(
    shortcode: &str,
    domain: &str,
    source_url: &str,
) -> Result<Url, String> {
    if !crate::domain::CustomEmoji::shortcode_valid(shortcode) {
        return Err("remote emoji shortcode is not valid".into());
    }
    if domain.is_empty() || domain.len() > 253 {
        return Err("remote emoji domain is not valid".into());
    }
    if source_url.len() > MAX_REMOTE_EMOJI_URL_LEN {
        return Err("remote emoji url is too long".into());
    }
    let url = Url::parse(source_url).map_err(|e| format!("remote emoji url is not a URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("remote emoji url is not http(s)".into()),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("remote emoji url carries credentials".into());
    }
    if url.host_str().is_none() {
        return Err("remote emoji url has no host".into());
    }
    Ok(url)
}

/// Fetch one remote emoji's bytes (the tail arm behind the ingest trigger).
/// SSRF policy first (`policy::check_url`, same HTTP-allowlist as attachment
/// fetches), then a bounded `GET`: 5xx/429/408 retry, anything else
/// non-2xx quarantines (a missing emoji never retries in a loop), the
/// `Content-Length` pre-check plus a streaming cap enforce
/// [`MAX_REMOTE_EMOJI_BYTES`], and only PNG/GIF/WebP bytes
/// (`is_emoji_image`, Mastodon's `CustomEmoji` content types) are returned.
/// Returns `(bytes, mime_type)`.
pub async fn fetch_remote_emoji_bytes(
    client: &reqwest::Client,
    url: &Url,
    allow_http: bool,
    allowlist: &[crate::federation::policy::Cidr],
) -> std::result::Result<(Vec<u8>, &'static str), DeliveryError> {
    use futures_util::StreamExt as _;
    if let Err(e) = policy::check_url(url, allow_http, allowlist) {
        return Err(DeliveryError::Quarantine(format!("emoji url refused: {e}")));
    }
    let response = client
        .get(url.clone())
        .header(
            reqwest::header::ACCEPT,
            "image/png,image/gif,image/webp,image/*,*/*",
        )
        .send()
        .await
        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    let status = response.status();
    if status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err(DeliveryError::Retry(format!(
            "emoji server answered {status}"
        )));
    }
    if !status.is_success() {
        return Err(DeliveryError::Quarantine(format!(
            "emoji server answered {status}"
        )));
    }
    if let Some(declared) = response.content_length()
        && declared > MAX_REMOTE_EMOJI_BYTES
    {
        return Err(DeliveryError::Quarantine(
            "emoji is larger than the limit".into(),
        ));
    }
    let mut body: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DeliveryError::Retry(e.to_string()))?;
        if body.len() as u64 + chunk.len() as u64 > MAX_REMOTE_EMOJI_BYTES {
            return Err(DeliveryError::Quarantine(
                "emoji is larger than the limit".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let mime = match ingest::sniff(&body) {
        Some(sniffed) if ingest::is_emoji_image(&body) => sniffed.mime_type,
        _ => {
            return Err(DeliveryError::Quarantine(
                "emoji is not a supported image".into(),
            ));
        }
    };
    Ok((body, mime))
}

/// Undo an emoji settle: both blobs, both durable rows, and any ledger
/// mapping a first commit already recorded (all best-effort).
fn rollback_emoji(
    cache: &dyn CacheStore,
    blobs: &dyn BlobStore,
    ledger: &Mutex<remote_cache::CacheLedger>,
    source_url: &str,
    image_key: &str,
    static_key: &str,
) {
    let _ = blobs.delete(image_key);
    let _ = blobs.delete(static_key);
    let _ = cache.cache_mark_evicted(source_url, remote_cache::EMOJI_VARIANT);
    let _ = cache.cache_mark_evicted(source_url, remote_cache::EMOJI_STATIC_VARIANT);
    if let Ok(mut guard) = ledger.lock() {
        guard.evict_one(&crate::persistence::redb::schema::remote_cache_key(
            source_url,
            remote_cache::EMOJI_VARIANT,
        ));
        guard.evict_one(&crate::persistence::redb::schema::remote_cache_key(
            source_url,
            remote_cache::EMOJI_STATIC_VARIANT,
        ));
    }
}

/// Stage fetched remote-emoji bytes: one blob key for the image and one for
/// the static variant (same bytes; remote originals are served as-is, never
/// transcoded), then upsert the row under the interim domain-namespaced key.
/// Remote rows are never picker-visible and never listed (upstream
/// `CustomEmoji.listed` is local-only); renderers serve them through the
/// `emojis` arrays only.
///
/// Admission runs before any byte is written: one reservation for both
/// physical copies under the reserved avatar/emoji class (adversarial-2 F2).
/// Disabled mode is honoured by the same refusal that gates every other
/// remote persist. Bytes land in the evictable [`remote_cache::remote_emoji_keys`]
/// namespace — never under `custom_emojis/images/…`, the durable pool with no
/// eviction — through the pool-routed blob handle (the caller passes the
/// runtime handle, so `remote/…` keys reach the cache pool). Every failure
/// after staging rolls back what was written (blobs, durable rows, ledger
/// mapping) and frees the reservation through the guard. `cache` is the
/// durable accounting facet of `store`, passed separately so tests can fault
/// it; `ledger` is the admission ledger (the process ledger in production).
///
/// Both copies are registered as durable `REMOTE_CACHE` rows
/// ([`EMOJI_VARIANT`] and [`EMOJI_STATIC_VARIANT`] under the fetch
/// `source_url`), so `cache_evict_batch` can reclaim them; eviction drops
/// bytes but the shells (and the render row below) survive for refetch.
pub fn store_remote_emoji(
    store: &dyn crate::application::ports::Store,
    blobs: &dyn BlobStore,
    cache: &dyn CacheStore,
    ledger: &Mutex<remote_cache::CacheLedger>,
    namespaced_shortcode: &str,
    origin_domain: &str,
    source_url: &str,
    bytes: &[u8],
    now_ms: u64,
    cfg: &RemoteCacheConfig,
) -> std::result::Result<crate::domain::CustomEmoji, String> {
    let mime_type = crate::application::emoji::validate_remote_emoji_bytes(bytes)
        .map_err(|err| err.to_string())?;
    let (shortcode, namespaced_domain) = split_remote_emoji_shortcode(namespaced_shortcode)
        .ok_or_else(|| "remote emoji key is not namespaced".to_string())?;
    if namespaced_domain != origin_domain.to_ascii_lowercase() {
        return Err("remote emoji domain does not match its key".into());
    }
    let (image_key, static_key) = remote_cache::remote_emoji_keys(origin_domain, shortcode)
        .ok_or_else(|| "remote emoji domain or shortcode is not a servable shape".to_string())?;
    let size = bytes.len() as u64;
    // Admission first: one reservation for both physical copies under the
    // reserved class. Disabled mode and the class cap refuse here, before
    // any byte is written.
    let mut reservation =
        remote_cache::Reservation::reserve(ledger, MediaClass::AvatarEmoji, size.saturating_mul(2))
            .map_err(|err| err.to_string())?;
    // Stage both blobs; unwind what was written on any failure (the guard
    // frees the reservation on every return below).
    blobs
        .put(&image_key, bytes)
        .map_err(|err| err.to_string())?;
    if let Err(err) = blobs.put(&static_key, bytes) {
        let _ = blobs.delete(&image_key);
        return Err(err.to_string());
    }
    // Durable byte rows first (mapping + counters together): a later failure
    // leaves counted bytes the reconciler can see, never phantom blobs.
    if let Err(err) = cache.cache_mark_stored(
        source_url,
        remote_cache::EMOJI_VARIANT,
        &image_key,
        size,
        None,
        now_ms,
        cfg,
    ) {
        let _ = blobs.delete(&image_key);
        let _ = blobs.delete(&static_key);
        return Err(err.to_string());
    }
    if let Err(err) = cache.cache_mark_stored(
        source_url,
        remote_cache::EMOJI_STATIC_VARIANT,
        &static_key,
        size,
        None,
        now_ms,
        cfg,
    ) {
        let _ = blobs.delete(&image_key);
        let _ = blobs.delete(&static_key);
        let _ = cache.cache_mark_evicted(source_url, remote_cache::EMOJI_VARIANT);
        return Err(err.to_string());
    }
    // Ledger settlement consumes the reservation, one commit per copy. A
    // refusal unwinds blobs, durable rows, and whatever the first commit
    // already recorded.
    let image_entry = remote_cache::RemoteCacheEntry::stored(
        &remote_cache::CacheEntryCtx {
            source_url,
            variant: remote_cache::EMOJI_VARIANT,
            class: MediaClass::AvatarEmoji,
            blob_key: &image_key,
            now_ms,
            cfg,
        },
        size,
    );
    if let Err(err) = reservation.commit_entry(size, image_entry) {
        rollback_emoji(cache, blobs, ledger, source_url, &image_key, &static_key);
        return Err(err.to_string());
    }
    let static_entry = remote_cache::RemoteCacheEntry::stored(
        &remote_cache::CacheEntryCtx {
            source_url,
            variant: remote_cache::EMOJI_STATIC_VARIANT,
            class: MediaClass::AvatarEmoji,
            blob_key: &static_key,
            now_ms,
            cfg,
        },
        size,
    );
    if let Err(err) = reservation.commit_entry(size, static_entry) {
        rollback_emoji(cache, blobs, ledger, source_url, &image_key, &static_key);
        return Err(err.to_string());
    }
    let _ = mime_type;
    let row = store
        .upsert_emoji(crate::domain::CustomEmoji {
            shortcode: namespaced_shortcode.to_string(),
            image_key: image_key.clone(),
            static_image_key: static_key,
            visible_in_picker: false,
            disabled: false,
            category: Some(format!("remote:{origin_domain}")),
        })
        .map_err(|e| e.to_string())?;
    // `REMOTE_EMOJI` render row alongside the interim namespaced row: the
    // status `emojis` arrays look rows up by bare shortcode (parent merge of
    // G-D S10) and serve the staged bytes through the `system/remote/emojis/`
    // asset route. The interim row stays for fetch-stage bookkeeping. The
    // origin `source_url` rides along so a byte miss enqueues a refetch
    // instead of answering 500.
    store
        .upsert_remote_emoji(crate::application::ports::RemoteEmojiRow {
            shortcode: shortcode.to_string(),
            domain: origin_domain.to_string(),
            url: format!("system/{image_key}"),
            source_url: source_url.to_string(),
        })
        .map_err(|e| e.to_string())?;
    Ok(row)
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
        let on = RemoteCacheConfig {
            warming: true,
            ..RemoteCacheConfig::default()
        };
        assert!(should_warm(&on, false));
        assert!(!should_warm(&on, true));
    }
}

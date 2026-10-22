//! Remote-media cache accounting + eviction (storage overhaul, Agent B).
//!
//! The cache is a byte-bounded pool for remote bytes (`docs/database.md` §"Decision",
//! `docs/remote-media-cache-plan.md` §§2-5). This module owns the admission,
//! accounting and eviction *policy*; durable rows live in redb
//! (`REMOTE_CACHE`/`CACHE_ACCOUNTING`, parent-owned) and bytes live in the
//! cache `BlobStore` root (Agent A). The policy here is pure and runs both in
//! the fetch path and in tests:
//!
//! - **Accounting:** stored bytes per class plus in-flight reservations, so
//!   concurrent downloads cannot overshoot the cap without bound. A fetch
//!   reserves its worst case *before* the first byte is read and shrinks the
//!   reservation once `Content-Length` is known.
//! - **Eviction:** byte-weighted LRU with per-class TTL. Expired entries go
//!   first, then least-recently-used. Order of operations per eviction is
//!   `remove mapping -> update accounting -> delete own blob key` (the caller
//!   deletes the returned keys; a crash between leaves an orphan blob for the
//!   reconciler, never a dangling mapping).
//! - **Reserve:** the avatar/emoji class is never borrowed by images. Image
//!   pressure evicts image bytes only; avatar pressure may displace image
//!   bytes, but avatars themselves are hard-capped at their class budget. The
//!   global cap is hard in both directions.
//! - **Classes:** avatar/emoji (config retention, default 30d), previews
//!   (7d), full originals (3d). Audio/video originals are `Stream`: fetched
//!   on demand, never persisted (0 MiB persistent).
//! - **Keys:** per-row `remote/{media_id}` keys are retained (v1, no
//!   content-addressed sharing, no refcount). Safe eviction deletes the row's
//!   own key. Anything outside `remote/` is refused here, so durable local
//!   uploads can never be evicted through this path.
//! - **Errors:** Mastodon shapes only (`422` oversize/unsupported, `404`
//!   missing, `503` origin down / budget / store). Never a Misskey Drive
//!   envelope.
//!
//! Warming stays off by default and is always relay-excluded
//! ([`should_warm`]); when enabled it reuses `FetchRemoteMedia` behind the
//! config flag (the trigger conversion lives in `federation_inbound.rs`,
//! parent-owned).

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::config::{RemoteCacheConfig, RemoteCacheMode};
use crate::domain::Error;

/// Per-file ceiling for one remote fetch. Mirrors
/// `federation::media::MAX_REMOTE_MEDIA_BYTES` (Mastodon's default upload
/// limit, 40 MiB); the two constants are asserted equal in tests. Kept here
/// (rather than imported) so this policy module does not depend on the
/// federation fetch path.
pub const PER_FILE_CEILING_BYTES: u64 = 40 * 1024 * 1024;

/// Closed media-class set for the cache budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaClass {
    /// Avatars, headers, custom emoji. Reserved: never borrowed by images.
    AvatarEmoji,
    /// Image previews / thumbnails.
    Preview,
    /// Full image originals.
    Full,
    /// Audio/video originals. Streamed on demand, never persisted.
    Stream,
}

impl MediaClass {
    /// Class iterator (stable order: reserved class first).
    pub fn all() -> [MediaClass; 4] {
        [
            MediaClass::AvatarEmoji,
            MediaClass::Preview,
            MediaClass::Full,
            MediaClass::Stream,
        ]
    }

    /// Hard byte ceiling for the class. `Stream` is 0: never persisted.
    pub fn cap_bytes(&self, cfg: &RemoteCacheConfig) -> u64 {
        match self {
            MediaClass::AvatarEmoji => cfg.avatar_emoji_cap_bytes,
            MediaClass::Preview => cfg.preview_cap_bytes,
            MediaClass::Full => cfg.full_cap_bytes,
            MediaClass::Stream => 0,
        }
    }

    /// TTL for the class in milliseconds. Avatar/emoji comes from config
    /// (default 30d); previews 7d, full originals 3d, streams expire at once.
    pub fn retention_ms(&self, cfg: &RemoteCacheConfig) -> u64 {
        const DAY_MS: u64 = 24 * 60 * 60 * 1000;
        match self {
            MediaClass::AvatarEmoji => cfg.avatar_retention_days.saturating_mul(DAY_MS),
            MediaClass::Preview => 7 * DAY_MS,
            MediaClass::Full => 3 * DAY_MS,
            MediaClass::Stream => 0,
        }
    }

    /// True for the reserved class: image pressure must never evict it.
    pub fn is_reserved(&self) -> bool {
        matches!(self, MediaClass::AvatarEmoji)
    }
}

/// Map a cache variant name to its class. Unknown variants are full
/// originals (the attachment path stores `original`).
pub fn class_for_variant(variant: &str) -> MediaClass {
    match variant {
        "avatar" | "header" | "emoji" => MediaClass::AvatarEmoji,
        "preview" | "thumbnail" | "small" | "card" => MediaClass::Preview,
        "stream" | "segment" => MediaClass::Stream,
        _ => MediaClass::Full,
    }
}

/// Map a sniffed content type to its class: audio/video originals are
/// `Stream` (served on demand, never persisted — 0 MiB persistent), every
/// other attachment original is `Full`.
pub fn class_for_mime(mime_type: &str) -> MediaClass {
    let top: &str = mime_type.split_once('/').map_or(mime_type, |(top, _)| top);
    match top {
        "audio" | "video" => MediaClass::Stream,
        _ => MediaClass::Full,
    }
}

/// The class for a fetched remote object: variant-specific classes
/// (avatars, previews, emoji, segments) win; attachment originals (and
/// anything unknown) are classified by sniffed content type, so audio/video
/// originals land in `Stream` instead of being silently persisted as `Full`.
///
/// The fetch path must call this with the sniffed MIME — never assume
/// `MediaClass::Full` — and must thread the operator-configured
/// [`RemoteCacheConfig`] (not `RemoteCacheConfig::default()`) into the
/// ledger entry, so expiry and budgets follow configuration.
pub fn class_for_fetch(mime_type: &str, variant: &str) -> MediaClass {
    match variant {
        "avatar" | "header" | "emoji" => MediaClass::AvatarEmoji,
        "preview" | "thumbnail" | "small" | "card" => MediaClass::Preview,
        "stream" | "segment" => MediaClass::Stream,
        _ => class_for_mime(mime_type),
    }
}

/// Whether the configured mode allows persisting remote bytes at all.
/// `Disabled` means no persistence: the ledger refuses every reservation
/// and commit loudly (never a silent persist), and startup config validation
/// rejects the mode until the non-persisting streaming design exists (see
/// the proposed `Config::validate` snippet in the parent merge notes).
pub fn persistence_allowed(cfg: &RemoteCacheConfig) -> bool {
    cfg.mode != RemoteCacheMode::Disabled
}

fn disabled_refusal() -> CacheError {
    CacheError::Unsupported("remote cache is disabled: refusing to persist bytes".into())
}

/// One cache entry: the durable metadata row in miniature. Metadata survives
/// byte eviction (`has_bytes == false` keeps `source_url` for re-fetch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCacheEntry {
    pub source_url: String,
    pub variant: String,
    pub blob_key: String,
    pub class: MediaClass,
    pub size: u64,
    pub last_use_ms: u64,
    pub expiry_ms: u64,
    pub etag: Option<String>,
    pub has_bytes: bool,
}

impl RemoteCacheEntry {
    /// Metadata-first record: no bytes yet, nothing counted.
    pub fn metadata_only(
        source_url: &str,
        variant: &str,
        class: MediaClass,
        blob_key: &str,
        now_ms: u64,
        cfg: &RemoteCacheConfig,
    ) -> Self {
        Self {
            source_url: source_url.to_string(),
            variant: variant.to_string(),
            blob_key: blob_key.to_string(),
            class,
            size: 0,
            last_use_ms: now_ms,
            expiry_ms: now_ms.saturating_add(class.retention_ms(cfg)),
            etag: None,
            has_bytes: false,
        }
    }

    /// Stored entry with bytes, freshly fetched.
    pub fn stored(
        source_url: &str,
        variant: &str,
        class: MediaClass,
        blob_key: &str,
        size: u64,
        now_ms: u64,
        cfg: &RemoteCacheConfig,
    ) -> Self {
        let mut entry = Self::metadata_only(source_url, variant, class, blob_key, now_ms, cfg);
        entry.size = size;
        entry.has_bytes = true;
        entry
    }

    /// True when the entry holds bytes past its TTL (evict first).
    /// Metadata-only shells are never "expired": there is nothing to free.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.has_bytes && now_ms >= self.expiry_ms
    }

    /// Redb key for this entry: `(source_url, variant)`.
    pub fn cache_key(&self) -> Vec<u8> {
        crate::persistence::redb::schema::remote_cache_key(&self.source_url, &self.variant)
    }
}

/// Cache-shape error. HTTP mapping is Mastodon's (`422`/`404`/`503`); the
/// body is always `{ "error": "..." }`, never a Misskey Drive envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// The file is larger than the per-file ceiling. 422.
    Oversize { size: u64, ceiling: u64 },
    /// Unsupported for the cache (stream class, non-cache key, bad content). 422.
    Unsupported(String),
    /// No source / row / bytes for the request. 404.
    Missing(String),
    /// The origin failed or timed out. 503.
    OriginDown(String),
    /// The caps cannot fit the request even after eviction. 503.
    BudgetExhausted { need: u64, cap: u64 },
    /// The blob store failed. 503.
    Store(String),
}

impl CacheError {
    /// Mastodon HTTP status for the failure.
    pub fn http_status(&self) -> u16 {
        match self {
            CacheError::Oversize { .. } | CacheError::Unsupported(_) => 422,
            CacheError::Missing(_) => 404,
            CacheError::OriginDown(_)
            | CacheError::BudgetExhausted { .. }
            | CacheError::Store(_) => 503,
        }
    }

    /// Human message (also the `error` body value).
    pub fn message(&self) -> String {
        match self {
            CacheError::Oversize { size, ceiling } => {
                format!("attachment is larger than the limit ({size} > {ceiling})")
            }
            CacheError::Unsupported(reason) => reason.clone(),
            CacheError::Missing(what) => what.clone(),
            CacheError::OriginDown(reason) => reason.clone(),
            CacheError::BudgetExhausted { need, cap } => {
                format!("remote cache budget exhausted (need {need}, cap {cap})")
            }
            CacheError::Store(reason) => reason.clone(),
        }
    }

    /// Mastodon error body: `{ "error": "..." }`.
    pub fn error_body(&self) -> serde_json::Value {
        serde_json::json!({ "error": self.message() })
    }
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl From<CacheError> for Error {
    fn from(err: CacheError) -> Self {
        match err {
            CacheError::Oversize { .. } | CacheError::Unsupported(_) => {
                Error::InvalidInput(err.message())
            }
            CacheError::Missing(_) => Error::NotFound(err.message()),
            CacheError::OriginDown(_)
            | CacheError::BudgetExhausted { .. }
            | CacheError::Store(_) => Error::Storage(err.message()),
        }
    }
}

/// Per-row blob key for a remote attachment (v1: no sharing, no refcount).
pub fn blob_key_for_media(media_id: u64) -> String {
    format!("remote/{media_id}")
}

/// True for keys this cache may evict. Durable local uploads live outside
/// `remote/` and are always refused here.
pub fn is_cache_key(key: &str) -> bool {
    key.starts_with("remote/")
}

/// Variant name for remote custom-emoji bytes in the evictable namespace.
pub const EMOJI_VARIANT: &str = "emoji";

/// Blob keys for a remote custom emoji in the evictable namespace:
/// `remote/emojis/{domain}/{shortcode}/original.png` (served bytes) and
/// `remote/emojis/{domain}/{shortcode}/static.png` (static variant).
///
/// Remote emoji must live here — never under `custom_emojis/images/…`,
/// which is the durable pool with no eviction. Both keys satisfy
/// [`is_cache_key`], so the ledger, `CacheEvict` and the reconciler cover
/// them; the ledger row keeps the origin `source_url`, so eviction drops
/// bytes but the shell survives for refetch. `None` when the domain or
/// shortcode is not a strict shape (never serve or store a guessed key).
pub fn remote_emoji_keys(domain: &str, shortcode: &str) -> Option<(String, String)> {
    let domain: String = domain.to_lowercase();
    if !is_remote_emoji_domain(&domain) {
        return None;
    }
    if !crate::domain::CustomEmoji::shortcode_valid(shortcode) {
        return None;
    }
    let base: String = format!("remote/emojis/{domain}/{shortcode}");
    let image_key: String = format!("{base}/original.png");
    let static_key: String = format!("{base}/static.png");
    if crate::media::blob_store::validate_key(&image_key).is_err()
        || crate::media::blob_store::validate_key(&static_key).is_err()
    {
        return None;
    }
    Some((image_key, static_key))
}

/// A remote-emoji origin domain: dot/dash separated lowercase labels, never
/// empty, never leading/trailing punctuation, no `..`.
fn is_remote_emoji_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    if domain.starts_with(['.', '-']) || domain.ends_with(['.', '-']) {
        return false;
    }
    if domain.contains("..") {
        return false;
    }
    domain
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// Warming reuses `FetchRemoteMedia` behind scoped opt-in and never covers
/// relay-originated media (there is no relay path yet; the exclusion is
/// enforced at the call site signature so it cannot be added later without
/// passing through here). Opt-in is either `mode = warm` or the `warming`
/// flag; both mean the same capped per-status warming
/// ([`crate::media::cache_maintenance::WARM_FETCH_CAP_PER_STATUS`]).
pub fn should_warm(cfg: &RemoteCacheConfig, is_relay: bool) -> bool {
    (cfg.warming || cfg.mode == crate::config::RemoteCacheMode::Warm) && !is_relay
}

/// Point-in-time usage snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheUsage {
    pub stored_total: u64,
    pub stored_avatar_emoji: u64,
    pub stored_preview: u64,
    pub stored_full: u64,
    pub inflight_total: u64,
    pub inflight_avatar_emoji: u64,
    pub inflight_preview: u64,
    pub inflight_full: u64,
}

/// In-memory accounting ledger: stored bytes per class plus in-flight
/// reservations. Durable accounting (`CACHE_ACCOUNTING`, parent-owned) mirrors
/// these counters; this ledger is the process-wide admission gate so
/// concurrent fetches cannot overshoot the cap.
#[derive(Debug, Clone)]
pub struct CacheLedger {
    cfg: RemoteCacheConfig,
    entries: BTreeMap<Vec<u8>, RemoteCacheEntry>,
    stored: [u64; 4],
    inflight: [u64; 4],
}

fn class_index(class: MediaClass) -> usize {
    match class {
        MediaClass::AvatarEmoji => 0,
        MediaClass::Preview => 1,
        MediaClass::Full => 2,
        MediaClass::Stream => 3,
    }
}

impl CacheLedger {
    pub fn new(cfg: RemoteCacheConfig) -> Self {
        Self {
            cfg,
            entries: BTreeMap::new(),
            stored: [0; 4],
            inflight: [0; 4],
        }
    }

    /// Replace the budgets (operator reconfig / parent wiring); counters stay.
    pub fn set_caps(&mut self, cfg: RemoteCacheConfig) {
        self.cfg = cfg;
    }

    /// Whether the installed budgets allow persisting bytes at all.
    /// `Disabled` mode refuses every reservation and commit loudly (never a
    /// silent persist); callers that would fetch-then-store (miss refetch,
    /// warming) must check this first and stand down.
    pub fn persistence_allowed(&self) -> bool {
        persistence_allowed(&self.cfg)
    }

    pub fn stored_for(&self, class: MediaClass) -> u64 {
        self.stored[class_index(class)]
    }

    pub fn inflight_for(&self, class: MediaClass) -> u64 {
        self.inflight[class_index(class)]
    }

    pub fn stored_total(&self) -> u64 {
        self.stored.iter().sum()
    }

    pub fn inflight_total(&self) -> u64 {
        self.inflight.iter().sum()
    }

    /// Committed load against the global cap: stored + in-flight.
    pub fn committed_total(&self) -> u64 {
        self.stored_total().saturating_add(self.inflight_total())
    }

    pub fn usage(&self) -> CacheUsage {
        CacheUsage {
            stored_total: self.stored_total(),
            stored_avatar_emoji: self.stored_for(MediaClass::AvatarEmoji),
            stored_preview: self.stored_for(MediaClass::Preview),
            stored_full: self.stored_for(MediaClass::Full),
            inflight_total: self.inflight_total(),
            inflight_avatar_emoji: self.inflight_for(MediaClass::AvatarEmoji),
            inflight_preview: self.inflight_for(MediaClass::Preview),
            inflight_full: self.inflight_for(MediaClass::Full),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &[u8]) -> Option<&RemoteCacheEntry> {
        self.entries.get(key)
    }

    /// Shortfall in bytes if `size` more bytes of `class` were admitted now.
    pub fn shortfall_for(&self, class: MediaClass, size: u64) -> u64 {
        let _ = class;
        self.committed_total()
            .saturating_add(size)
            .saturating_sub(self.cfg.global_cap_bytes)
    }

    /// Order victims to free `bytes_to_free` for `need`: expired entries of
    /// the need class first (LRU within), then its live entries (LRU), then
    /// other classes expired-then-LRU. Avatar entries are never offered to a
    /// non-avatar need (the reserve); an avatar need may displace image
    /// bytes. Metadata-only shells are never victims (nothing to free).
    /// Pure: no mutation, so the caller can check the freed sum first.
    pub fn plan_eviction(&self, need: MediaClass, bytes_to_free: u64, now_ms: u64) -> Vec<Vec<u8>> {
        if bytes_to_free == 0 {
            return Vec::new();
        }
        let mut classes = vec![need];
        for class in MediaClass::all() {
            if class == need {
                continue;
            }
            if need != MediaClass::AvatarEmoji && class == MediaClass::AvatarEmoji {
                continue;
            }
            classes.push(class);
        }
        self.collect_victims(&classes, bytes_to_free, now_ms)
    }

    fn collect_victims(
        &self,
        classes: &[MediaClass],
        bytes_to_free: u64,
        now_ms: u64,
    ) -> Vec<Vec<u8>> {
        let mut victims = Vec::new();
        let mut freed = 0u64;
        for class in classes {
            let mut in_class: Vec<(&Vec<u8>, &RemoteCacheEntry)> = self
                .entries
                .iter()
                .filter(|(_, e)| e.class == *class && e.has_bytes)
                .collect();
            // Expired first (`false` sorts before `true`), then LRU.
            in_class.sort_by_key(|(_, e)| (!e.is_expired(now_ms), e.last_use_ms));
            for (key, entry) in in_class {
                if freed >= bytes_to_free {
                    break;
                }
                victims.push((*key).clone());
                freed = freed.saturating_add(entry.size);
            }
            if freed >= bytes_to_free {
                break;
            }
        }
        victims
    }

    /// Apply a planned victim list: remove the byte mapping and update
    /// accounting, keeping the metadata shell (`source_url`) for re-fetch.
    /// Returns the blob keys the caller must now delete (mapping first, then
    /// the key — a crash between leaves an orphan blob, never a dangling
    /// mapping).
    pub fn evict_planned(&mut self, victims: &[Vec<u8>]) -> Vec<String> {
        let mut blob_keys = Vec::new();
        for key in victims {
            if let Some(entry) = self.entries.get_mut(key) {
                if entry.has_bytes {
                    let idx = class_index(entry.class);
                    self.stored[idx] = self.stored[idx].saturating_sub(entry.size);
                    entry.has_bytes = false;
                    entry.size = 0;
                    blob_keys.push(entry.blob_key.clone());
                }
            }
        }
        blob_keys
    }

    /// Evict one entry by its cache key (the `CacheEvict` bounded batch /
    /// reconciler path). Same order: mapping, accounting, then the caller
    /// deletes the returned key.
    pub fn evict_one(&mut self, key: &[u8]) -> Option<String> {
        let owned = key.to_vec();
        let keys = self.evict_planned(std::slice::from_ref(&owned));
        keys.into_iter().next()
    }

    /// Make room for `size` bytes of `class`: plan victims, refuse when even
    /// full eviction cannot fit, otherwise apply and return the blob keys to
    /// delete. Enforces the avatar class ceiling (own-class victims only)
    /// and the hard global cap.
    pub fn evict_for(
        &mut self,
        class: MediaClass,
        size: u64,
        now_ms: u64,
    ) -> Result<Vec<String>, CacheError> {
        if size > PER_FILE_CEILING_BYTES {
            return Err(CacheError::Oversize {
                size,
                ceiling: PER_FILE_CEILING_BYTES,
            });
        }
        if class == MediaClass::Stream {
            return Err(CacheError::Unsupported(
                "audio/video originals stream on demand and are never persisted".into(),
            ));
        }
        // Avatar ceiling: own-class victims only, never images.
        if class == MediaClass::AvatarEmoji {
            let cap = self.cfg.avatar_emoji_cap_bytes;
            let over = self
                .stored_for(MediaClass::AvatarEmoji)
                .saturating_add(size)
                .saturating_sub(cap);
            if over > 0 {
                let victims = self.collect_victims(&[MediaClass::AvatarEmoji], over, now_ms);
                let freed: u64 = victims
                    .iter()
                    .filter_map(|k| self.entries.get(k))
                    .map(|e| e.size)
                    .sum();
                if freed < over {
                    return Err(CacheError::BudgetExhausted { need: size, cap });
                }
                self.evict_planned(&victims);
            }
        }
        let shortfall = self.shortfall_for(class, size);
        if shortfall == 0 {
            return Ok(Vec::new());
        }
        let victims = self.plan_eviction(class, shortfall, now_ms);
        let freed: u64 = victims
            .iter()
            .filter_map(|k| self.entries.get(k))
            .map(|e| e.size)
            .sum();
        if freed < shortfall {
            return Err(CacheError::BudgetExhausted {
                need: size,
                cap: self.cfg.global_cap_bytes,
            });
        }
        Ok(self.evict_planned(&victims))
    }

    /// Reserve `bytes` of in-flight budget for `class`. Call *before* reading
    /// the body (worst case first, shrink when `Content-Length` is known).
    /// Refusals never mutate.
    pub fn reserve(&mut self, class: MediaClass, bytes: u64) -> Result<(), CacheError> {
        if !persistence_allowed(&self.cfg) {
            return Err(disabled_refusal());
        }
        if bytes > PER_FILE_CEILING_BYTES {
            return Err(CacheError::Oversize {
                size: bytes,
                ceiling: PER_FILE_CEILING_BYTES,
            });
        }
        if class == MediaClass::Stream {
            return Err(CacheError::Unsupported(
                "audio/video originals stream on demand and are never persisted".into(),
            ));
        }
        if class == MediaClass::AvatarEmoji
            && self
                .stored_for(MediaClass::AvatarEmoji)
                .saturating_add(bytes)
                > self.cfg.avatar_emoji_cap_bytes
        {
            return Err(CacheError::BudgetExhausted {
                need: bytes,
                cap: self.cfg.avatar_emoji_cap_bytes,
            });
        }
        if self.committed_total().saturating_add(bytes) > self.cfg.global_cap_bytes {
            return Err(CacheError::BudgetExhausted {
                need: bytes,
                cap: self.cfg.global_cap_bytes,
            });
        }
        let idx = class_index(class);
        self.inflight[idx] = self.inflight[idx].saturating_add(bytes);
        Ok(())
    }

    /// Release in-flight budget without storing (fetch failed / shrank).
    pub fn release(&mut self, class: MediaClass, bytes: u64) {
        let idx = class_index(class);
        self.inflight[idx] = self.inflight[idx].saturating_sub(bytes);
    }

    /// Settle a reservation into stored bytes: drop `reserved_bytes` of
    /// in-flight budget and count `entry.size` as stored. Re-commits replace
    /// the previous mapping (sizes adjusted, no double count). Refuses
    /// non-cache keys, so durable local uploads can never enter the ledger.
    pub fn commit(
        &mut self,
        class: MediaClass,
        reserved_bytes: u64,
        entry: RemoteCacheEntry,
    ) -> Result<(), CacheError> {
        if !is_cache_key(&entry.blob_key) {
            self.release(class, reserved_bytes);
            return Err(CacheError::Unsupported(format!(
                "refusing non-cache key in the remote ledger: {}",
                entry.blob_key
            )));
        }
        if !persistence_allowed(&self.cfg) {
            self.release(class, reserved_bytes);
            return Err(disabled_refusal());
        }
        if entry.class != class {
            self.release(class, reserved_bytes);
            return Err(CacheError::Unsupported(format!(
                "entry class {:?} does not match reservation {:?}",
                entry.class, class
            )));
        }
        self.release(class, reserved_bytes);
        let key = entry.cache_key();
        if let Some(old) = self.entries.get(&key) {
            if old.has_bytes {
                let idx = class_index(old.class);
                self.stored[idx] = self.stored[idx].saturating_sub(old.size);
            }
        }
        let mut entry = entry;
        entry.has_bytes = true;
        let idx = class_index(entry.class);
        self.stored[idx] = self.stored[idx].saturating_add(entry.size);
        self.entries.insert(key, entry);
        Ok(())
    }

    /// Record metadata without bytes (metadata-first ingest): no accounting
    /// change. An existing byte mapping is never clobbered — only its
    /// validators / last-use refresh.
    pub fn record_metadata(&mut self, entry: RemoteCacheEntry) {
        let key = entry.cache_key();
        match self.entries.get_mut(&key) {
            Some(existing) => {
                existing.etag = entry.etag.clone();
                existing.last_use_ms = existing.last_use_ms.max(entry.last_use_ms);
            }
            None => {
                let mut shell = entry;
                shell.has_bytes = false;
                shell.size = 0;
                self.entries.insert(key, shell);
            }
        }
    }

    /// Re-insert a durable ledger row after a restart, without a prior
    /// reservation: byte-holding rows count as stored bytes. Refuses rows
    /// outside the cache namespace so durable media can never enter the
    /// ledger, and refuses everything while persistence is disabled. A row
    /// already present is replaced (no double count).
    pub fn restore_entry(&mut self, entry: RemoteCacheEntry) -> Result<(), CacheError> {
        if !is_cache_key(&entry.blob_key) {
            return Err(CacheError::Unsupported(format!(
                "refusing non-cache key in the remote ledger: {}",
                entry.blob_key
            )));
        }
        if !persistence_allowed(&self.cfg) {
            return Err(disabled_refusal());
        }
        let key = entry.cache_key();
        if let Some(old) = self.entries.get(&key) {
            if old.has_bytes {
                let idx = class_index(old.class);
                self.stored[idx] = self.stored[idx].saturating_sub(old.size);
            }
        }
        if entry.has_bytes {
            let idx = class_index(entry.class);
            self.stored[idx] = self.stored[idx].saturating_add(entry.size);
        }
        self.entries.insert(key, entry);
        Ok(())
    }

    /// Forget every row and stored-byte counter while keeping in-flight
    /// reservations (running downloads still hold their budget). Used to
    /// rebuild the ledger from the durable rows.
    pub fn clear_entries(&mut self) {
        self.entries.clear();
        self.stored = [0; 4];
    }

    /// Drop the byte mapping of the row that owns `blob_key` (the durable
    /// eviction already happened). Returns whether a byte-holding row was
    /// found.
    pub fn evict_by_blob_key(&mut self, blob_key: &str) -> bool {
        let found: Option<Vec<u8>> = self
            .entries
            .iter()
            .find(|(_, entry)| entry.has_bytes && entry.blob_key == blob_key)
            .map(|(key, _)| key.clone());
        match found {
            Some(key) => self.evict_one(&key).is_some(),
            None => false,
        }
    }

    /// Refresh last-use (a served hit). Missing keys are a no-op.
    pub fn touch(&mut self, key: &[u8], now_ms: u64) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_use_ms = now_ms;
        }
    }
}

/// Process-wide admission ledger for the fetch path. Budgets come from
/// operator config via [`configure_process_ledger`] (parent wires it in
/// `main.rs`); until then the plan defaults apply.
static PROCESS_LEDGER: OnceLock<Mutex<CacheLedger>> = OnceLock::new();

/// The process-wide cache ledger.
pub fn process_ledger() -> &'static Mutex<CacheLedger> {
    PROCESS_LEDGER.get_or_init(|| Mutex::new(CacheLedger::new(RemoteCacheConfig::default())))
}

/// Install operator budgets into the process ledger (idempotent).
pub fn configure_process_ledger(cfg: RemoteCacheConfig) {
    if let Ok(mut ledger) = process_ledger().lock() {
        ledger.set_caps(cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_cfg() -> RemoteCacheConfig {
        RemoteCacheConfig {
            mode: crate::config::RemoteCacheMode::OnDemand,
            global_cap_bytes: 1024 * 1024,
            avatar_emoji_cap_bytes: 256 * 1024,
            preview_cap_bytes: 512 * 1024,
            full_cap_bytes: 256 * 1024,
            avatar_retention_days: 30,
            warming: false,
        }
    }

    fn full_entry(source: &str, size: u64, now: u64, cfg: &RemoteCacheConfig) -> RemoteCacheEntry {
        RemoteCacheEntry::stored(
            source,
            "original",
            MediaClass::Full,
            &format!("remote/{}", source.len()),
            size,
            now,
            cfg,
        )
    }

    #[test]
    fn ceiling_mirrors_fetch_path() {
        assert_eq!(
            PER_FILE_CEILING_BYTES,
            crate::federation::media::MAX_REMOTE_MEDIA_BYTES
        );
    }

    #[test]
    fn avatar_class_is_reserved_and_never_offered_to_images() {
        let cfg = small_cfg();
        let mut ledger = CacheLedger::new(cfg.clone());
        ledger
            .reserve(MediaClass::AvatarEmoji, 64 * 1024)
            .expect("avatar fits its reserve");
        let avatar = RemoteCacheEntry::stored(
            "https://cdn.example/a.png",
            "avatar",
            MediaClass::AvatarEmoji,
            "remote/1",
            64 * 1024,
            1000,
            &cfg,
        );
        ledger
            .commit(MediaClass::AvatarEmoji, 64 * 1024, avatar)
            .unwrap();
        // Image pressure for more than the whole global cap: every image byte
        // must go before refusal, avatars stay.
        let victims = ledger.plan_eviction(MediaClass::Full, 4 * 1024 * 1024, 2000);
        assert!(victims.is_empty());
        assert_eq!(ledger.stored_for(MediaClass::AvatarEmoji), 64 * 1024);
    }

    #[test]
    fn stream_class_is_never_persisted() {
        let cfg = small_cfg();
        let mut ledger = CacheLedger::new(cfg);
        assert!(matches!(
            ledger.reserve(MediaClass::Stream, 1024),
            Err(CacheError::Unsupported(_))
        ));
        assert_eq!(MediaClass::Stream.cap_bytes(&small_cfg()), 0);
        assert_eq!(MediaClass::Stream.retention_ms(&small_cfg()), 0);
    }

    #[test]
    fn fetch_class_comes_from_the_sniffed_mime() {
        assert_eq!(class_for_fetch("video/mp4", "original"), MediaClass::Stream);
        assert_eq!(
            class_for_fetch("audio/mpeg", "original"),
            MediaClass::Stream
        );
        assert_eq!(class_for_fetch("image/png", "original"), MediaClass::Full);
        assert_eq!(class_for_fetch("video/mp4", "preview"), MediaClass::Preview);
        assert_eq!(
            class_for_fetch("image/png", "avatar"),
            MediaClass::AvatarEmoji
        );
        assert_eq!(
            class_for_fetch("image/gif", "emoji"),
            MediaClass::AvatarEmoji
        );
    }

    #[test]
    fn disabled_mode_refuses_to_persist_loudly() {
        let cfg = RemoteCacheConfig {
            mode: crate::config::RemoteCacheMode::Disabled,
            ..small_cfg()
        };
        assert!(!persistence_allowed(&cfg));
        assert!(persistence_allowed(&small_cfg()));
        let mut ledger = CacheLedger::new(cfg.clone());
        assert!(matches!(
            ledger.reserve(MediaClass::Full, 64),
            Err(CacheError::Unsupported(_))
        ));
        let entry = RemoteCacheEntry::stored(
            "https://cdn.example/a.png",
            "original",
            MediaClass::Full,
            "remote/1",
            64,
            1000,
            &cfg,
        );
        assert!(matches!(
            ledger.commit(MediaClass::Full, 0, entry),
            Err(CacheError::Unsupported(_))
        ));
        let shell = RemoteCacheEntry::stored(
            "https://cdn.example/b.png",
            "original",
            MediaClass::Full,
            "remote/2",
            64,
            1000,
            &cfg,
        );
        assert!(ledger.restore_entry(shell).is_err());
        assert!(ledger.is_empty());
    }

    #[test]
    fn remote_emoji_keys_live_in_the_evictable_namespace() {
        let (image, static_key) =
            remote_emoji_keys("remote.example", "party").expect("valid remote emoji shape");
        assert_eq!(image, "remote/emojis/remote.example/party/original.png");
        assert_eq!(static_key, "remote/emojis/remote.example/party/static.png");
        assert!(is_cache_key(&image));
        assert!(is_cache_key(&static_key));
        // Domains canonicalize to lowercase.
        let (lower, _) = remote_emoji_keys("REMOTE.EXAMPLE", "party").expect("case folds");
        assert!(lower.starts_with("remote/emojis/remote.example/"));
        for bad_domain in ["", "a..b", ".lead", "trail.", "UP PER", "x/y", "a:b"] {
            assert!(
                remote_emoji_keys(bad_domain, "party").is_none(),
                "refuse domain {bad_domain:?}"
            );
        }
        assert!(remote_emoji_keys("remote.example", "x").is_none());
        assert!(remote_emoji_keys("remote.example", "has space").is_none());
        assert_eq!(class_for_variant(EMOJI_VARIANT), MediaClass::AvatarEmoji);
    }

    #[test]
    fn error_shapes_are_mastodon_never_drive() {
        let cases = [
            (
                CacheError::Oversize {
                    size: 9,
                    ceiling: 8,
                },
                422u16,
            ),
            (CacheError::Unsupported("no".into()), 422),
            (CacheError::Missing("gone".into()), 404),
            (CacheError::OriginDown("down".into()), 503),
            (CacheError::BudgetExhausted { need: 1, cap: 1 }, 503),
            (CacheError::Store("io".into()), 503),
        ];
        for (err, status) in cases {
            assert_eq!(err.http_status(), status);
            let body = err.error_body();
            assert!(body.get("error").is_some(), "Mastodon error key");
            let raw = body.to_string();
            assert!(!raw.to_lowercase().contains("drive"), "never Misskey Drive");
        }
    }
}

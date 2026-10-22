use super::account_store::NewSession;
use crate::domain::{Error, Id, Job, JobKind, LoginSession, Result};

/// Server-side login sessions for the OAuth approval flow.
pub trait SessionStore: Send + Sync {
    fn create_session(&self, input: NewSession) -> Result<LoginSession>;
    fn session_by_plaintext(&self, plaintext: &str) -> Result<Option<LoginSession>>;
    fn revoke_session(&self, plaintext: &str) -> Result<bool>;
}

/// Outbound mail (verification messages). An adapter/job boundary: mail is
/// never sent inside a request transaction.
pub trait MailSender: Send + Sync {
    /// Send an email-confirmation message for a newly registered account.
    fn send_confirmation(&self, to: &str, account_id: Id, token: &str) -> Result<()>;
    /// Send a password-reset message (resend endpoint job).
    fn send_password_reset(&self, to: &str, account_id: Id, token: &str) -> Result<()>;
}

/// Blob/object storage for media bytes and custom emoji images.
///
/// File storage cannot participate in a `redb` transaction: bytes are staged
/// *before* the metadata row commits, and a failed commit leaves only a
/// bounded orphan for cleanup (`birchtree.md` §5).
pub trait BlobStore: Send + Sync {
    /// Stage `bytes` under `key` (creates parents; overwrites).
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()>;
    /// Read a blob (`None` when missing).
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    /// Delete a blob (idempotent).
    fn delete(&self, key: &str) -> Result<()>;
    /// Whether a blob exists.
    fn exists(&self, key: &str) -> Result<bool>;

    /// Filesystem root this handle writes under, when the handle is backed
    /// by the filesystem (storage overhaul, `docs/database.md` Agent C).
    /// The fetch path uses it to stage response bodies to `tmp/{rand}.part`
    /// under the byte cap and atomically rename onto the key. `None` keeps
    /// the legacy buffered path (in-memory test doubles).
    fn root_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Filesystem root of the evictable cache pool, when the handle fronts
    /// both pools (the `PooledBlobStores` runtime handle). The public file
    /// route streams `remote/…` keys from here after the durable root, so a
    /// large remote costs a file-backed response instead of a full in-memory
    /// copy per request. `None` (single-pool handles, in-memory doubles)
    /// leaves the durable root as the only streaming root.
    fn cache_root_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Filesystem root that remote-download staging must use (adversarial-2
    /// F11). The pooled handle answers with the cache root so `publish_tmp`
    /// is a same-volume rename; single-pool handles answer with their own
    /// root. `None` keeps the legacy buffered path (in-memory doubles).
    fn staging_root(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Publish an already-staged tmp file onto `key` (atomic rename; storage
    /// overhaul, Agent C). The default refuses; `FsBlobStore` resolves the
    /// key under its root and renames.
    fn publish_tmp(&self, _tmp: &std::path::Path, _key: &str) -> Result<()> {
        Err(Error::Internal("publish_tmp is not implemented".into()))
    }

    /// Stream `bytes` under `key` without buffering the whole body: the
    /// implementation stages to `tmp/{rand}.part` under `max_bytes`
    /// (`media::fetch_stream::stage_slices_capped` shape) and atomically
    /// renames on success. Default impl buffers (back-compat for test doubles);
    /// `FsBlobStore` overrides with tmp+rename.
    fn put_streamed(&self, key: &str, bytes: &[u8], max_bytes: u64) -> Result<()> {
        if bytes.len() as u64 > max_bytes {
            return Err(Error::InvalidInput(format!(
                "stream body exceeds {max_bytes} bytes"
            )));
        }
        self.put(key, bytes)
    }
}

/// Which byte pool a blob handle addresses (`docs/database.md` Agent A).
/// Durable = local uploads, never evicted; cache = remote fetches, evictable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobPool {
    Durable,
    Cache,
}

/// Access to both blob pools. The durable handle keeps the pre-overhaul
/// `<data_dir>/media` root (lazy-move readable); the cache handle owns the
/// fresh `<data_dir>/media_cache` root. Agent B's fetch path takes `Cache`.
pub trait BlobStores: Send + Sync {
    fn pool(&self, pool: BlobPool) -> std::sync::Arc<dyn BlobStore>;
}

/// Remote-cache metadata + accounting (`docs/database.md` Step 0).
/// Payloads < 64 KiB; evict batches bounded (<= 500 rows).
pub trait CacheStore: Send + Sync {
    fn cache_get(
        &self,
        source_url: &str,
        variant: &str,
    ) -> Result<Option<crate::media::remote_cache::RemoteCacheEntry>>;
    fn cache_put_metadata(&self, entry: crate::media::remote_cache::RemoteCacheEntry)
    -> Result<()>;
    /// Store bytes for `(source_url, variant)` with dedupe sharing
    /// (adversarial-2 B0): a second `blob_key` for the same pair bumps the
    /// row's `refcount` and keeps the first bytes instead of overwriting the
    /// mapping. `cfg` supplies the effective retention (adversarial-2 B3);
    /// callers must thread the operator config, never `default()`.
    fn cache_mark_stored(
        &self,
        source_url: &str,
        variant: &str,
        blob_key: &str,
        size: u64,
        etag: Option<String>,
        now_ms: u64,
        cfg: &crate::config::RemoteCacheConfig,
    ) -> Result<()>;
    /// Drop the byte mapping, keep the metadata shell; returns the blob key
    /// the caller must delete. `None` when already shell-only / missing.
    fn cache_mark_evicted(&self, source_url: &str, variant: &str) -> Result<Option<String>>;
    fn cache_touch(&self, source_url: &str, variant: &str, now_ms: u64) -> Result<()>;
    fn cache_accounting(&self) -> Result<crate::media::remote_cache::CacheUsage>;
    /// Bounded victim batch (<= 500): expired-then-LRU within `need_class`,
    /// then other classes, never avatar unless need is avatar. One txn for
    /// mapping+accounting; caller deletes the returned keys (own keys only).
    fn cache_evict_batch(
        &self,
        need: crate::media::remote_cache::MediaClass,
        bytes_to_free: u64,
        now_ms: u64,
        cap: usize,
    ) -> Result<Vec<String>>;
}

/// Durable job queue.
pub trait JobQueue: Send + Sync {
    fn enqueue(&self, kind: JobKind, payload: serde_json::Value, now_ms: u64) -> Result<Job>;

    /// Enqueue a job that becomes claimable at `due_at_ms` (scheduled-status
    /// publication, delayed retries).
    fn enqueue_at(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
        due_at_ms: u64,
        now_ms: u64,
    ) -> Result<Job>;
    /// Claim up to `limit` due jobs under a lease.
    fn claim_due(&self, now_ms: u64, limit: usize) -> Result<Vec<Job>>;
    /// Delete up to `limit` `Succeeded` jobs whose terminal time
    /// (`terminal_at_ms`, falling back to `due_at_ms` for pre-v24 rows) is
    /// older than `older_than_ms`, returning how many rows were removed. Only
    /// `Succeeded` rows are eligible: `Pending`/`Leased` rows are live work;
    /// `Quarantined` rows age out separately through `prune_quarantined`.
    /// `complete()` keeps the row `Succeeded` (callers prune separately
    /// through this method).
    fn prune_succeeded(&self, older_than_ms: u64, limit: usize) -> Result<u64>;
    /// Delete up to `limit` `Quarantined` jobs whose terminal time
    /// (`terminal_at_ms`, falling back to `due_at_ms` for pre-v24 rows) is
    /// older than `older_than_ms`, returning how many rows were removed. Only
    /// `Quarantined` rows are eligible: `Pending`/`Leased` rows are live
    /// work and `Succeeded` rows age out separately through
    /// `prune_succeeded`. Oldest first, so a capped pass always reaps the
    /// stalest rows. Quarantine is the operator inspection surface with a
    /// long retention (see `jobs::worker::QUARANTINE_RETENTION_MS`); prune is
    /// delete-only (no archive export — the row's `last_error` and redacted
    /// payload are the diagnostics).
    fn prune_quarantined(&self, older_than_ms: u64, limit: usize) -> Result<u64>;
    /// Mark a job terminally succeeded, stamping `terminal_at_ms` for
    /// retention aging (adversarial-2 C4).
    fn complete(&self, id: Id, now_ms: u64) -> Result<()>;
    /// Record a failed attempt; schedules a retry or quarantines.
    fn retry(&self, id: Id, error: &str, now_ms: u64) -> Result<Job>;
    /// Mark a job terminally quarantined, stamping `terminal_at_ms` for
    /// retention aging (adversarial-2 C4).
    fn quarantine(&self, id: Id, error: &str, now_ms: u64) -> Result<()>;
    fn get(&self, id: Id) -> Result<Option<Job>>;
}

/// Readiness probe used by the runtime.
pub trait ReadyCheck: Send + Sync {
    /// True when the store is open and can serve a read.
    fn is_ready(&self) -> bool;
}

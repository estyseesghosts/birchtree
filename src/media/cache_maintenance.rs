//! Remote-media cache lifecycle: startup restore, eviction, reconciliation
//! and their job scheduling.
//!
//! The cache pool holds remote bytes only; durable local media lives in a
//! different tree this module never receives. Four jobs keep the pool and
//! its `redb` ledger (`REMOTE_CACHE` / `CACHE_ACCOUNTING`) consistent:
//!
//! - **restore** ([`CacheMaintenance::restore_at_startup`]) runs once before
//!   traffic: it adopts in-use bytes the ledger lost track of, drops mappings
//!   whose bytes are gone, fixes sizes, recomputes the counters from the rows
//!   and rebuilds the in-memory admission ledger, so an empty ledger never
//!   means "zero bytes stored" after a restart;
//! - **`CacheEvict`** ([`CacheMaintenance::run_cache_evict`]) expires entries
//!   past their TTL and evicts back under the class and global caps, in
//!   batches of at most 500 rows. Order per batch: mapping and counters in
//!   one transaction first, then delete exactly the returned keys from the
//!   cache pool;
//! - **`ReconcileStorage`** ([`CacheMaintenance::run_reconcile`]) walks the
//!   whole keyspace in resumable pages (see [`ReconCursor`]) and converges
//!   orphans, missing blobs and stale rows, then recomputes the counters;
//! - **fetch on miss** ([`enqueue_fetch_on_miss`]) turns a request for evicted
//!   bytes into a single-flight `FetchRemoteMedia` job.
//!
//! [`CacheMaintenance::handle_job`] dispatches both maintenance kinds and
//! re-enqueues the next run, so one seed ([`CacheMaintenance::seed_jobs`])
//! keeps the chains alive for the life of the process.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::application::ports::{BlobStore, CacheStore, JobQueue, Store};
use crate::config::{RemoteCacheConfig, RemoteCacheMode};
use crate::domain::{Error, Id, Job, JobKind, Result};
use crate::media::blob_store::FsBlobStore;
use crate::media::fetch_stream;
use crate::media::public_media;
use crate::media::reconciler::{self, ReconCursor, ReconReport};
use crate::media::remote_cache::{self, CacheLedger, CacheUsage, MediaClass, RemoteCacheEntry};
use crate::persistence::redb::RedbStore;
use crate::persistence::redb::remote_cache::AccountingRepair;

/// Rows or blobs handled per maintenance batch (the job-contract bound).
pub const MAINTENANCE_BATCH: usize = 500;

/// How many attachments of one status are warmed at ingest when warming is
/// enabled (Mastodon allows four attachments per status).
pub const WARM_FETCH_CAP_PER_STATUS: usize = 4;

/// Cadence and bounds of the maintenance chains.
#[derive(Debug, Clone)]
pub struct MaintenanceSettings {
    /// Delay between `CacheEvict` runs once the pool is within its caps.
    pub evict_interval_ms: u64,
    /// Delay between full `ReconcileStorage` passes.
    pub reconcile_interval_ms: u64,
    /// Delay between pages of one reconcile pass (bounded work per job).
    pub reconcile_page_gap_ms: u64,
    /// Age before staging residue counts as abandoned.
    pub residue_min_age: Duration,
    /// Rows or blobs per batch (clamped to [`MAINTENANCE_BATCH`]).
    pub batch: usize,
}

impl Default for MaintenanceSettings {
    fn default() -> Self {
        Self {
            evict_interval_ms: 5 * 60 * 1000,
            reconcile_interval_ms: 60 * 60 * 1000,
            reconcile_page_gap_ms: 5 * 1000,
            residue_min_age: Duration::from_secs(60 * 60),
            batch: MAINTENANCE_BATCH,
        }
    }
}

/// Outcome of one `CacheEvict` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictReport {
    /// Entries evicted for being past their TTL.
    pub expired: usize,
    /// Entries evicted to get back under a class or the global cap.
    pub pressure: usize,
    /// Blob files deleted from the cache pool.
    pub blobs_deleted: usize,
    /// More work remains (a batch filled up); the next run follows quickly.
    pub more_work: bool,
}

/// Outcome of one `ReconcileStorage` page.
#[derive(Debug, Clone)]
pub struct ReconcileOutcome {
    /// What the sweep found and fixed.
    pub report: ReconReport,
    /// Untracked in-use blobs registered in the ledger.
    pub adopted: usize,
    /// Abandoned staging files removed.
    pub residue_removed: usize,
    /// Counter repair after the sweep.
    pub accounting: AccountingRepair,
    /// Resume point, or `None` when the pass is complete.
    pub next: Option<ReconCursor>,
}

/// What the startup restore found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Ledger rows examined.
    pub rows: usize,
    /// In-use blobs the ledger did not know about, now registered.
    pub adopted: usize,
    /// Rows whose bytes were gone; the mapping was dropped (refetch heals).
    pub missing: usize,
    /// Rows whose recorded size differed from the file; fixed.
    pub resized: usize,
    /// Stored bytes in the rebuilt ledger.
    pub stored_bytes: u64,
    /// Abandoned staging files removed.
    pub residue_removed: usize,
    /// Pre-split durable `remote/` shadows moved into the cache pool (with
    /// their ledger rows and accounting); the durable copies are gone.
    pub shadows_migrated: usize,
    /// Migrated shadow bytes now counted in the cache pool.
    pub shadow_bytes: u64,
    /// Stale durable duplicates deleted (the cache pool already held the
    /// authoritative copy).
    pub shadow_duplicates_removed: usize,
    /// Unreferenced durable shadows deleted (no ledger row and no media
    /// row names them; the cache-side equivalent is reconciler-deleted).
    pub shadow_orphans_removed: usize,
}

/// Outcome of one durable-shadow migration pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShadowMigrateReport {
    /// Shadows moved into the cache pool with rows + accounting.
    pub migrated: usize,
    /// Migrated bytes now counted in the cache pool.
    pub bytes: u64,
    /// Stale durable duplicates deleted (cache copy authoritative).
    pub duplicates_removed: usize,
    /// Unreferenced durable shadows deleted.
    pub orphans_removed: usize,
}

/// The cache lifecycle service. Cheap to share behind an `Arc`.
pub struct CacheMaintenance {
    store: std::sync::Arc<RedbStore>,
    cache: FsBlobStore,
    durable: Option<FsBlobStore>,
    config: RemoteCacheConfig,
    settings: MaintenanceSettings,
    ledger: &'static Mutex<CacheLedger>,
    chain: AtomicU64,
}

impl CacheMaintenance {
    /// Service over `store` and the cache pool, using the process-wide
    /// admission ledger. The pool is attached to the store so the
    /// `ReconStore` implementation sees the same bytes.
    pub fn new(
        store: std::sync::Arc<RedbStore>,
        cache: FsBlobStore,
        config: RemoteCacheConfig,
    ) -> Self {
        store.attach_cache_pool(cache.clone());
        Self {
            store,
            cache,
            durable: None,
            config,
            settings: MaintenanceSettings::default(),
            ledger: remote_cache::process_ledger(),
            chain: AtomicU64::new(0),
        }
    }

    /// Replace the cadence settings.
    pub fn with_settings(mut self, settings: MaintenanceSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Also sweep abandoned staging residue in the durable pool. Only the
    /// residue is touched (age-gated `.part` files), never blobs.
    pub fn with_durable_residue_sweep(mut self, durable: FsBlobStore) -> Self {
        self.durable = Some(durable);
        self
    }

    /// Use a specific admission ledger instead of the process-wide one
    /// (tests; a leaked `Mutex` gives the required `'static`).
    pub fn with_ledger(mut self, ledger: &'static Mutex<CacheLedger>) -> Self {
        self.ledger = ledger;
        self
    }

    /// The store this service maintains.
    pub fn store(&self) -> &RedbStore {
        &self.store
    }

    fn batch(&self) -> usize {
        self.settings.batch.clamp(1, MAINTENANCE_BATCH)
    }

    fn lock_ledger(&self) -> Result<std::sync::MutexGuard<'static, CacheLedger>> {
        self.ledger
            .lock()
            .map_err(|_| Error::Internal("cache ledger poisoned".into()))
    }

    // -----------------------------------------------------------------
    // Startup restore.
    // -----------------------------------------------------------------

    /// Restore the cache state before traffic is accepted. Idempotent and
    /// safe to repeat; every step works in pages of at most
    /// [`MAINTENANCE_BATCH`] entries and only needs the previous page's last
    /// key to continue.
    pub fn restore_at_startup(&self, now_ms: u64) -> Result<RestoreReport> {
        let mut report = RestoreReport::default();

        // No writer exists yet, so any staging residue is abandoned.
        report.residue_removed = self.sweep_residue(Duration::ZERO)?;

        // 1. Adopt in-use bytes the ledger lost track of.
        let mut after: Option<String> = None;
        loop {
            let keys = self
                .cache
                .list_keys(after.as_deref(), self.batch(), Duration::ZERO)?;
            let Some(last) = keys.last().cloned() else {
                break;
            };
            report.adopted += self.adopt_referenced(&keys, now_ms)?;
            after = Some(last);
        }

        // 2. Move pre-split durable `remote/` shadows into the cache pool
        // (copy, verify, account, delete the durable copy): after this pass
        // no `remote/` byte lives outside the evictable pool, so eviction
        // and deletes can never be shadowed by a stale durable copy.
        let shadows = self.migrate_durable_shadows(now_ms)?;
        report.shadows_migrated = shadows.migrated;
        report.shadow_bytes = shadows.bytes;
        report.shadow_duplicates_removed = shadows.duplicates_removed;
        report.shadow_orphans_removed = shadows.orphans_removed;

        // 3. Check every byte-holding row against the file system.
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = self
                .store
                .cache_rows_page(cursor.as_deref(), self.batch())?;
            let Some((last_key, _)) = page.last().cloned() else {
                break;
            };
            for (_, row) in &page {
                report.rows += 1;
                if !row.has_bytes {
                    continue;
                }
                match self.cache.size_of(&row.blob_key)? {
                    None => {
                        reconciler::ReconStore::flag_missing(&*self.store, &row.blob_key, now_ms)?;
                        report.missing += 1;
                    }
                    Some(actual) if actual != row.size => {
                        self.store.cache_mark_stored(
                            &row.source_url,
                            &row.variant,
                            &row.blob_key,
                            actual,
                            row.etag.clone(),
                            now_ms,
                            &self.config,
                        )?;
                        report.resized += 1;
                    }
                    Some(_) => {}
                }
            }
            cursor = Some(last_key);
        }

        // 3. Counters follow the rows; the admission ledger follows both.
        let repair = self.store.cache_accounting_rebuild()?;
        report.stored_bytes = repair.after_total;
        self.restore_ledger()?;
        Ok(report)
    }

    /// Rebuild the in-memory admission ledger from the durable rows,
    /// keeping in-flight reservations.
    pub fn restore_ledger(&self) -> Result<()> {
        let mut rows: Vec<RemoteCacheEntry> = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = self
                .store
                .cache_rows_page(cursor.as_deref(), self.batch())?;
            let Some((last_key, _)) = page.last().cloned() else {
                break;
            };
            rows.extend(page.into_iter().map(|(_, row)| row));
            cursor = Some(last_key);
        }
        let mut ledger = self.lock_ledger()?;
        ledger.set_caps(self.config.clone());
        ledger.clear_entries();
        for row in rows {
            if let Err(err) = ledger.restore_entry(row) {
                tracing::warn!(error = %err, "ledger row skipped during restore");
            }
        }
        Ok(())
    }

    /// Register in-use blobs (a committed attachment row names them) that
    /// the ledger has no byte-holding row for. An existing row of any kind
    /// for the blob key is an explicit decision (a shell after eviction):
    /// the surviving file is residue and is deleted from the cache pool
    /// instead of being adopted. A duplicate file for an already-counted
    /// (source, variant) pair is likewise residue. Only a blob with no row
    /// at all for its pair is adopted.
    fn adopt_referenced(&self, keys: &[String], now_ms: u64) -> Result<usize> {
        use crate::application::federation_ports::FederationContentStore;

        let mut adopted = 0;
        for key in keys {
            if !remote_cache::is_cache_key(key) {
                continue;
            }
            if self.store.cache_row_by_blob_key(key)?.is_some() {
                if self.cache.exists(key)? {
                    self.cache.delete(key)?;
                }
                continue;
            }
            let Some((media_id, variant)) = self.store.media_references_blob(key)? else {
                continue;
            };
            let Some(source) = self.store.remote_media_source(media_id)? else {
                continue;
            };
            if self.store.cache_get(&source, variant)?.is_some() {
                if self.cache.exists(key)? {
                    self.cache.delete(key)?;
                }
                continue;
            }
            let Some(size) = self.cache.size_of(key)? else {
                continue;
            };
            self.store.cache_mark_stored(
                &source,
                variant,
                key,
                size,
                None,
                now_ms,
                &self.config,
            )?;
            adopted += 1;
        }
        Ok(adopted)
    }

    /// Move pre-split durable `remote/` shadows into the cache pool.
    ///
    /// Deliberate migration (not lazy): every `remote/` key under the
    /// durable root is resolved once, at startup, in pages:
    ///
    /// - cache pool already holds the key → the durable copy is a stale
    ///   duplicate (the post-split cache copy is authoritative; cache writes
    ///   are atomic so it cannot be torn) and is deleted;
    /// - only the durable pool holds it and a ledger row or media row names
    ///   it → bytes copy to the cache pool, the copy is verified by size,
    ///   the row/accounting updates via `cache_mark_stored`, then the
    ///   durable copy is deleted (a crash between leaves the durable copy
    ///   for the next pass, never an uncounted cache blob);
    /// - nothing names it → it is crash-window residue (remote avatars are
    ///   never blob-cached, so `remote/` keys are attachments only) and the
    ///   durable copy is deleted, exactly as the reconciler deletes the
    ///   identical cache-side state.
    ///
    /// No-op without a durable handle (unit tests). After this pass no
    /// `remote/` byte lives outside the evictable pool.
    pub fn migrate_durable_shadows(&self, now_ms: u64) -> Result<ShadowMigrateReport> {
        use crate::application::federation_ports::FederationContentStore;

        let mut report = ShadowMigrateReport::default();
        let Some(durable) = &self.durable else {
            return Ok(report);
        };
        let mut after: Option<String> = None;
        loop {
            let keys = durable.list_keys(after.as_deref(), self.batch(), Duration::ZERO)?;
            let Some(last) = keys.last().cloned() else {
                break;
            };
            for key in &keys {
                if !remote_cache::is_cache_key(key) {
                    continue;
                }
                if self.cache.exists(key)? {
                    durable.delete(key)?;
                    report.duplicates_removed += 1;
                    continue;
                }
                let Some(bytes) = durable.get(key)? else {
                    continue;
                };
                let size: u64 = bytes.len() as u64;
                // The row that owns these bytes, if any: a byte-holding
                // ledger row whose mapping names the key, else the media row
                // that references it (its remote source feeds the ledger
                // row). An explicit shell (evicted) is not an owner: the
                // durable copy is residue of deleted bytes and is removed.
                let mut owner: Option<(String, String)> = None;
                if let Some((_, row)) = self.store.cache_row_by_blob_key(key)? {
                    if row.has_bytes {
                        owner = Some((row.source_url.clone(), row.variant.clone()));
                    } else {
                        durable.delete(key)?;
                        report.orphans_removed += 1;
                        continue;
                    }
                }
                if owner.is_none()
                    && let Some((media_id, variant)) = self.store.media_references_blob(key)?
                    && let Some(source) = self.store.remote_media_source(media_id)?
                {
                    owner = Some((source, variant.to_string()));
                }
                let Some((source, variant)) = owner else {
                    durable.delete(key)?;
                    report.orphans_removed += 1;
                    continue;
                };
                self.cache.put(key, &bytes)?;
                if self.cache.size_of(key)? != Some(size) {
                    let _ = self.cache.delete(key);
                    return Err(Error::Storage(format!(
                        "shadow copy of {key} failed verification"
                    )));
                }
                self.store.cache_mark_stored(
                    &source,
                    &variant,
                    key,
                    size,
                    None,
                    now_ms,
                    &self.config,
                )?;
                durable.delete(key)?;
                report.migrated += 1;
                report.bytes = report.bytes.saturating_add(size);
            }
            after = Some(last);
        }
        Ok(report)
    }

    fn sweep_residue(&self, min_age: Duration) -> Result<usize> {
        let mut removed =
            fetch_stream::sweep_stale_residue(self.cache.root(), min_age, MAINTENANCE_BATCH)?;
        if let Some(durable) = &self.durable {
            removed +=
                fetch_stream::sweep_stale_residue(durable.root(), min_age, MAINTENANCE_BATCH)?;
        }
        Ok(removed)
    }

    // -----------------------------------------------------------------
    // CacheEvict.
    // -----------------------------------------------------------------

    /// Shared eviction primitive for fetch-time admission and scheduled
    /// maintenance: select victims in durable storage first
    /// ([`CacheStore::cache_evict_batch`]: `REMOTE_CACHE` rows plus counters
    /// in ONE transaction), then delete exactly those blob keys, then catch
    /// the in-memory admission ledger up.
    ///
    /// Never deletes first: a crash between the transaction and the deletes
    /// leaves at most uncounted orphan blobs for the reconciler, never rows
    /// that claim bytes that are gone. A durable failure deletes nothing and
    /// the caller must not proceed (admission treats it as transient and
    /// retries without fetching).
    pub fn evict_pressure(
        &self,
        need: MediaClass,
        bytes_to_free: u64,
        now_ms: u64,
    ) -> Result<EvictReport> {
        let batch: usize = self.batch();
        let outcome = evict_pressure_durable_first(
            &*self.store,
            &self.cache,
            self.durable
                .as_ref()
                .map(|durable| durable as &dyn BlobStore),
            self.ledger,
            need,
            bytes_to_free,
            now_ms,
            batch,
        )?;
        Ok(EvictReport {
            expired: 0,
            pressure: outcome.victims.len(),
            blobs_deleted: outcome.blobs_deleted,
            more_work: outcome.victims.len() >= batch,
        })
    }

    /// One bounded eviction pass: TTL expiry first, then pressure relief for
    /// each class cap and the global cap. Only cache-pool keys returned by
    /// the ledger transaction are deleted.
    pub fn run_cache_evict(&self, now_ms: u64) -> Result<EvictReport> {
        let mut report = EvictReport::default();
        let batch: usize = self.batch();

        let expired: Vec<String> = self.store.cache_expire_batch(now_ms, batch)?;
        report.expired = expired.len();
        report.more_work |= expired.len() >= batch;
        report.blobs_deleted += self.delete_cache_blobs(&expired)?;

        let usage: CacheUsage = self.store.cache_accounting()?;
        let mut pressure: Vec<(MediaClass, u64)> = Vec::new();
        for (class, stored) in [
            (MediaClass::AvatarEmoji, usage.stored_avatar_emoji),
            (MediaClass::Preview, usage.stored_preview),
            (MediaClass::Full, usage.stored_full),
        ] {
            let over: u64 = stored.saturating_sub(class.cap_bytes(&self.config));
            if over > 0 {
                pressure.push((class, over));
            }
        }
        let global_over: u64 = usage
            .stored_total
            .saturating_sub(self.config.global_cap_bytes);
        if global_over > 0 && pressure.is_empty() {
            // Over the global cap with every class inside its own cap:
            // image pressure (the avatar reserve is never offered).
            pressure.push((MediaClass::Full, global_over));
        }
        for (class, bytes) in pressure {
            let pass = self.evict_pressure(class, bytes, now_ms)?;
            report.pressure += pass.pressure;
            report.more_work |= pass.more_work;
            report.blobs_deleted += pass.blobs_deleted;
        }
        Ok(report)
    }

    /// Delete the given cache-pool keys and drop them from the admission
    /// ledger. Keys outside the cache namespace are refused. Any durable
    /// shadow of the key is deleted too: the public route checks the durable
    /// root first, so a surviving shadow would keep serving evicted bytes.
    /// A file delete failure still syncs the ledger for the committed row;
    /// the error is returned after the whole batch is synced so redb and
    /// the ledger agree and the reconciler retries the orphan file.
    fn delete_cache_blobs(&self, keys: &[String]) -> Result<usize> {
        let mut deleted = 0;
        let mut first_err: Option<Error> = None;
        for key in keys {
            if !remote_cache::is_cache_key(key) {
                return Err(Error::InvalidInput(format!(
                    "refusing to evict a non-cache key: {key}"
                )));
            }
            if self.cache.exists(key)? {
                deleted += 1;
            }
            if let Err(err) = self.cache.delete(key) {
                tracing::warn!(blob_key = key.as_str(), error = %err, "expired blob delete failed");
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
            if let Some(durable) = &self.durable
                && durable.exists(key)?
                && let Err(err) = durable.delete(key)
            {
                tracing::warn!(blob_key = key.as_str(), error = %err, "expired shadow delete failed");
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
            if let Ok(mut guard) = self.lock_ledger() {
                guard.evict_by_blob_key(key);
                guard.evict_by_blob_key(key);
            }
        }
        if let Some(err) = first_err {
            return Err(err);
        }
        Ok(deleted)
    }

    /// Enforce the configured caps after [`CacheMaintenance::restore_at_startup`],
    /// before traffic is accepted: repeat the bounded eviction pass until
    /// every class cap and the global cap hold. A restart (or a shadow
    /// migration that adopted more bytes than the caps allow) must never
    /// serve while over cap; the same durable-first primitive backs every
    /// pass, so redb and disk agree throughout.
    ///
    /// Bounded (100 passes of at most [`MAINTENANCE_BATCH`] rows each): gives
    /// up with an error — startup refuses — rather than looping forever.
    pub fn enforce_caps_at_startup(&self, now_ms: u64) -> Result<EvictReport> {
        let mut total = EvictReport::default();
        for _ in 0..100 {
            if self.caps_hold()? {
                total.more_work = false;
                return Ok(total);
            }
            let pass = self.run_cache_evict(now_ms)?;
            total.expired += pass.expired;
            total.pressure += pass.pressure;
            total.blobs_deleted += pass.blobs_deleted;
            total.more_work = pass.more_work;
            if !pass.more_work && self.caps_hold()? {
                total.more_work = false;
                return Ok(total);
            }
        }
        if self.caps_hold()? {
            total.more_work = false;
            return Ok(total);
        }
        Err(Error::Storage(
            "cache still over its caps after bounded startup eviction".into(),
        ))
    }

    /// Whether every class cap and the global cap currently hold.
    fn caps_hold(&self) -> Result<bool> {
        let usage: CacheUsage = self.store.cache_accounting()?;
        for (class, stored) in [
            (MediaClass::AvatarEmoji, usage.stored_avatar_emoji),
            (MediaClass::Preview, usage.stored_preview),
            (MediaClass::Full, usage.stored_full),
        ] {
            if stored > class.cap_bytes(&self.config) {
                return Ok(false);
            }
        }
        Ok(usage.stored_total <= self.config.global_cap_bytes)
    }

    // -----------------------------------------------------------------
    // ReconcileStorage.
    // -----------------------------------------------------------------

    /// One resumable reconcile page starting at `cursor`.
    pub fn run_reconcile(&self, now_ms: u64, cursor: &ReconCursor) -> Result<ReconcileOutcome> {
        let residue_removed: usize = self.sweep_residue(self.settings.residue_min_age)?;

        let candidates = reconciler::ReconStore::list_blob_keys(
            &*self.store,
            cursor.blob_after.as_deref().filter(|_| !cursor.blobs_done),
            self.batch(),
        )?;
        let adopted: usize = if cursor.blobs_done {
            0
        } else {
            self.adopt_referenced(&candidates, now_ms)?
        };

        let page = reconciler::reconcile_page(&*self.store, now_ms, self.batch(), cursor)?;
        let accounting = self.store.cache_accounting_rebuild()?;
        self.restore_ledger()?;
        Ok(ReconcileOutcome {
            report: page.report,
            adopted,
            residue_removed,
            accounting,
            next: page.next,
        })
    }

    // -----------------------------------------------------------------
    // Job scheduling.
    // -----------------------------------------------------------------

    /// Start the maintenance chains for this process: one `CacheEvict` and
    /// one `ReconcileStorage` job. Chains left in the queue by a previous
    /// process carry a different chain id; they run once more and end, so
    /// restarts never multiply chains.
    pub fn seed_jobs(&self, queue: &dyn JobQueue, now_ms: u64) -> Result<()> {
        let chain: u64 = rand::random::<u64>() | 1;
        self.chain.store(chain, Ordering::Relaxed);
        queue.enqueue_at(
            JobKind::CacheEvict,
            serde_json::json!({ "chain": chain }),
            now_ms.saturating_add(60 * 1000),
            now_ms,
        )?;
        queue.enqueue_at(
            JobKind::ReconcileStorage,
            serde_json::json!({ "chain": chain, "cursor": ReconCursor::default() }),
            now_ms.saturating_add(5 * 60 * 1000),
            now_ms,
        )?;
        Ok(())
    }

    /// Run a claimed `CacheEvict` or `ReconcileStorage` job and enqueue the
    /// next run of its chain. Other kinds are an error: a maintenance
    /// dispatcher must never silently accept work it cannot do.
    ///
    /// A failed run is logged and the chain continues (the queue's retry
    /// would otherwise fork the chain); only a failure to schedule the
    /// follow-up is returned for the queue to retry.
    pub fn handle_job(&self, job: &Job, queue: &dyn JobQueue, now_ms: u64) -> Result<()> {
        let chain: u64 = job
            .payload
            .get("chain")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let live_chain: bool = chain != 0 && chain == self.chain.load(Ordering::Relaxed);
        match job.kind {
            JobKind::CacheEvict => {
                let (more_work, ok) = match self.run_cache_evict(now_ms) {
                    Ok(report) => {
                        tracing::info!(
                            expired = report.expired,
                            pressure = report.pressure,
                            blobs_deleted = report.blobs_deleted,
                            "cache evict"
                        );
                        (report.more_work, true)
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "cache evict run failed");
                        (false, false)
                    }
                };
                if live_chain {
                    let delay: u64 = if more_work {
                        1_000
                    } else if ok {
                        self.settings.evict_interval_ms
                    } else {
                        self.settings.evict_interval_ms.min(60_000)
                    };
                    queue.enqueue_at(
                        JobKind::CacheEvict,
                        serde_json::json!({ "chain": chain }),
                        now_ms.saturating_add(delay),
                        now_ms,
                    )?;
                }
                Ok(())
            }
            JobKind::ReconcileStorage => {
                let cursor: ReconCursor = job
                    .payload
                    .get("cursor")
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .unwrap_or_default();
                let mut next: Option<ReconCursor> = None;
                match self.run_reconcile(now_ms, &cursor) {
                    Ok(outcome) => {
                        tracing::info!(
                            orphans = outcome.report.orphans_removed,
                            stale = outcome.report.stale_removed,
                            missing = outcome.report.missing_keys.len(),
                            adopted = outcome.adopted,
                            "reconcile storage page"
                        );
                        next = outcome.next;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "reconcile storage run failed");
                    }
                }
                if live_chain {
                    let (cursor_value, delay) = match next {
                        Some(cursor) => (cursor, self.settings.reconcile_page_gap_ms),
                        None => (ReconCursor::default(), self.settings.reconcile_interval_ms),
                    };
                    queue.enqueue_at(
                        JobKind::ReconcileStorage,
                        serde_json::json!({ "chain": chain, "cursor": cursor_value }),
                        now_ms.saturating_add(delay),
                        now_ms,
                    )?;
                }
                Ok(())
            }
            other => Err(Error::Internal(format!(
                "cache maintenance cannot run {}",
                other.as_str()
            ))),
        }
    }
}

/// Outcome of one durable-first pressure eviction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PressureEvictOutcome {
    /// Blob keys unmapped (rows + counters) and deleted.
    pub victims: Vec<String>,
    /// Victim blobs actually removed from disk (a victim whose bytes were
    /// already gone still counts in `victims`, never here).
    pub blobs_deleted: usize,
    /// Victims whose mapping committed but whose file delete failed. The
    /// in-memory ledger is already synced for them; the reconciler retries
    /// the file delete.
    pub failed: Vec<String>,
}

/// Shared eviction primitive for fetch-time admission and scheduled
/// maintenance: select victims in durable storage first
/// ([`CacheStore::cache_evict_batch`]: `REMOTE_CACHE` rows plus counters in
/// ONE transaction), then delete exactly those blob keys, then catch the
/// in-memory admission ledger up.
///
/// `blobs` is the pool the victims live in (the pooled runtime handle on
/// the fetch path, the cache pool on the maintenance path);
/// `durable_shadow` additionally clears a pre-split durable copy when the
/// caller fronts the pools separately (maintenance only; the pooled handle
/// already clears both). `ledger` is the admission ledger to catch up
/// (best-effort: it is rebuilt from durable rows at startup).
///
/// Never deletes first: a crash between the transaction and the deletes
/// leaves at most uncounted orphan blobs for the reconciler, never rows
/// that claim bytes that are gone. A blob delete failure does not roll back
/// the committed mapping: the ledger is still synced and the key is
/// reported in [`PressureEvictOutcome::failed`] so the reconciler retries
/// the file delete.
pub fn evict_pressure_durable_first(
    store: &(impl CacheStore + ?Sized),
    blobs: &dyn BlobStore,
    durable_shadow: Option<&dyn BlobStore>,
    ledger: &Mutex<CacheLedger>,
    need: MediaClass,
    bytes_to_free: u64,
    now_ms: u64,
    batch: usize,
) -> Result<PressureEvictOutcome> {
    // 1. Durable victim selection first: rows + counters in ONE transaction.
    let victims: Vec<String> = store.cache_evict_batch(need, bytes_to_free, now_ms, batch)?;
    // 2. Then delete exactly the returned keys (own keys only). A delete
    // failure leaves an orphan blob: the mapping already committed, so the
    // ledger still syncs and the key is reported for the reconciler.
    let mut blobs_deleted = 0;
    let mut failed: Vec<String> = Vec::new();
    for key in &victims {
        if !remote_cache::is_cache_key(key) {
            return Err(Error::InvalidInput(format!(
                "refusing to evict a non-cache key: {key}"
            )));
        }
        let existed = blobs.exists(key).unwrap_or(false);
        if let Err(err) = blobs.delete(key) {
            tracing::warn!(blob_key = key.as_str(), error = %err, "evicted blob delete failed");
            failed.push(key.clone());
            continue;
        }
        if existed {
            blobs_deleted += 1;
        }
        if let Some(durable) = durable_shadow
            && durable.exists(key).unwrap_or(false)
            && let Err(err) = durable.delete(key)
        {
            tracing::warn!(blob_key = key.as_str(), error = %err, "evicted shadow delete failed");
            failed.push(key.clone());
        }
    }
    // 3. In-memory admission ledger follows, even for failed deletes: it
    // must match the committed redb rows. Shared rows hold one counted
    // size for N refs; one `evict_by_blob_key` drops a single ref, so two
    // calls clear the dedupe pair (2->1, then 1->0). Higher counts
    // converge over later passes via `restore_ledger` in the reconcile
    // sweep.
    if let Ok(mut guard) = ledger.lock() {
        for key in &victims {
            guard.evict_by_blob_key(key);
            guard.evict_by_blob_key(key);
        }
    }
    Ok(PressureEvictOutcome {
        victims,
        blobs_deleted,
        failed,
    })
}

// ---------------------------------------------------------------------
// Fetch policy.
// ---------------------------------------------------------------------
/// Whether ingest should enqueue `FetchRemoteMedia` eagerly for the
/// attachments of a freshly received status.
///
/// The documented policy is **on demand**: bytes are fetched on the first
/// request that misses the cache (see [`enqueue_fetch_on_miss`]), so ingest
/// enqueues nothing. Scoped warming is a distinct, capped opt-in per
/// [`remote_cache::should_warm`] (`mode = warm` or `warming = true`, relay
/// always excluded, [`WARM_FETCH_CAP_PER_STATUS`] per status); `Disabled`
/// mode never warms.
pub fn fetch_at_ingest(config: &RemoteCacheConfig, is_relay: bool) -> bool {
    if config.mode == RemoteCacheMode::Disabled {
        return false;
    }
    remote_cache::should_warm(config, is_relay)
}

/// Enqueue a `FetchRemoteMedia` job for a cached attachment whose bytes are
/// gone (evicted, reconciled away or never fetched). Single-flight: at most
/// one job per media row per suppression window in this process, and only
/// for rows that still know their source URL. Returns whether a job was
/// enqueued. Never enqueues while persistence is disabled (there would be
/// nowhere to keep the fetched bytes).
pub fn enqueue_fetch_on_miss(store: &dyn Store, media_id: Id, now_ms: u64) -> Result<bool> {
    // The process ledger carries the operator budgets (wired at startup):
    // while persistence is disabled there is nowhere to keep fetched bytes,
    // so the miss stays a 404 instead of enqueueing doomed work.
    if let Ok(ledger) = remote_cache::process_ledger().lock()
        && !ledger.persistence_allowed()
    {
        return Ok(false);
    }
    if !public_media::refetch_gate().should_enqueue(media_id.value()) {
        return Ok(false);
    }
    if store.remote_media_source(media_id)?.is_none() {
        return Ok(false);
    }
    store.enqueue(
        JobKind::FetchRemoteMedia,
        serde_json::json!({ "media_id": media_id.value() }),
        now_ms,
    )?;
    Ok(true)
}

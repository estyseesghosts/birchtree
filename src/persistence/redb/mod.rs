//! `redb` storage adapter.
//!
//! One process-wide `redb::Database` behind the narrow ports in
//! `application::ports`. Values are JSON-encoded; keys are ordered primitives
//! so index scans stay bounded. No redb types escape this module.

pub mod schema;
pub mod migrations;
pub mod accounts;
pub mod oauth;
pub mod jobs;

pub mod admin_infra;
pub mod statuses;
pub mod relationships;
pub mod conversations_push;
pub mod web;
pub mod quotes;
pub mod polls;
pub mod media;
pub mod notifications;
pub mod places;

// Phase 4 storage modules: timelines, lists, markers, filters, scheduled
// statuses and custom emoji.
pub mod timelines;
pub mod trends;
pub mod lists;
pub mod markers;
pub mod filters;
pub mod scheduled;
pub mod emoji;
pub mod suggestions;
pub mod announcements;
pub mod search;

// Phase 6: federation keys, remote accounts, inbound dedup and follow commands.
pub mod federation;
pub mod federation_content;

// Storage overhaul (`docs/database.md`): durable remote-cache metadata +
// accounting behind `CacheStore` (parent merge of Agent B's snippet).
pub mod remote_cache;

// Gaps batch (`docs/gaps.md` Step 0): collections rows (G-C owns this module).
pub mod collections;

// Gaps batch G-E (`docs/gaps.md`, parent merge of S4): second-factor rows
// over `TOTP_SECRETS` / `WEBAUTHN_CREDS`.
pub mod second_factor;
// WP1: authenticated encryption for TOTP seeds, actor keys, mail outbox.
pub mod secrets;

// D39 remainder: unified interaction-event journal backing the admin
// `interactions` measure. Table + migration are schema v12 (parent); the
// `InteractionLog` implementation lands with the metrics work.
pub mod interactions;

// Phase 7.3: admin account moderation, the moderator report queue and the
// audit trail (subagent B). Shares the `ADMIN_AUDIT` table with subagent C's
// `AdminInfraStore` audit writes (same record shape, `target_type`
// distinguishes rows).
pub mod admin;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use redb::Database;
use redb::ReadableDatabase;
use redb::WriteTransaction;

use crate::domain::{Clock, Id, IdGenerator};
use crate::media::blob_store::FsBlobStore;

/// Default age before an unreferenced cache blob counts as an orphan.
pub const DEFAULT_ORPHAN_GRACE_MS: u64 = 10 * 60 * 1000;

/// Errors raised by the storage adapter.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("open database at {path}: {source}")]
    Open {
        path: PathBuf,
        source: redb::DatabaseError,
    },
    #[error("redb transaction: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("redb table: {0}")]
    Table(#[from] redb::TableError),
    #[error("redb storage: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("redb commit: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("encode/decode: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("migration: {0}")]
    Migration(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage permissions: {0}")]
    Permissions(String),
}

impl From<StoreError> for crate::domain::Error {
    fn from(err: StoreError) -> Self {
        crate::domain::Error::Storage(err.to_string())
    }
}

// Allow the `?` operator to lift individual redb errors straight into the
// domain error type inside the port implementations.
macro_rules! domain_from_redb {
    ($($ty:ty),* $(,)?) => {
        $(
            impl From<$ty> for crate::domain::Error {
                fn from(err: $ty) -> Self {
                    crate::domain::Error::Storage(err.to_string())
                }
            }
        )*
    };
}

domain_from_redb!(
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
    redb::DatabaseError,
);

/// Convenience alias for storage results.
pub type StoreResult<T> = Result<T, StoreError>;

/// JSON-encode a record for storage.
pub(super) fn encode<T: serde::Serialize>(value: &T) -> StoreResult<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

/// JSON-decode a stored record.
pub(super) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> StoreResult<T> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Instrumentation counters (Phase 4: "measure DB transaction/query counts").
///
/// Every port method runs inside exactly one read or write transaction, so
/// transaction counts are the query count at the port boundary: a page loader
/// that issues one transaction per batched lookup instead of one per status
/// shows up directly here. Range scans additionally report the number of index
/// entries they walked through [`MetricsSnapshot::items_scanned`], which the
/// bounded-scan tests assert against.
#[derive(Debug, Default)]
struct Metrics {
    read_txns: std::sync::atomic::AtomicU64,
    write_txns: std::sync::atomic::AtomicU64,
    items_scanned: std::sync::atomic::AtomicU64,
}

/// A point-in-time copy of [`Metrics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MetricsSnapshot {
    /// Read transactions begun since the store opened.
    pub read_txns: u64,
    /// Write transactions begun since the store opened.
    pub write_txns: u64,
    /// Index entries walked by bounded range scans.
    pub items_scanned: u64,
}

/// The process-wide storage handle.
pub struct RedbStore {
    db: Arc<Database>,
    path: PathBuf,
    /// Kept for record writes that stamp their own times in later phases.
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    metrics: Metrics,
    /// The remote-cache byte pool, attached once at startup. The cache
    /// reconciler (`ReconStore`) and eviction lifecycle read and delete
    /// bytes only through this handle, so they can never reach durable
    /// media.
    cache_pool: OnceLock<FsBlobStore>,
    /// Minimum age of a cache blob before the reconciler may treat it as an
    /// orphan (a blob that is still being published has no row yet).
    orphan_grace_ms: AtomicU64,
}

impl RedbStore {
    /// Open (creating if necessary) the database at `path` and run migrations.
    pub fn open(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            prepare_data_dir(parent)?;
        }
        create_private_database_file(&path)?;
        let db = Database::create(&path).map_err(|source| StoreError::Open {
            path: path.clone(),
            source,
        })?;
        enforce_private_file(&path)?;
        migrations::apply(&db)?;
        Ok(Self {
            db: Arc::new(db),
            path,
            clock,
            ids,
            metrics: Metrics::default(),
            cache_pool: OnceLock::new(),
            orphan_grace_ms: AtomicU64::new(DEFAULT_ORPHAN_GRACE_MS),
        })
    }

    /// Attach the cache byte pool (once, at startup). A second attach is
    /// ignored: the pool a store was opened against never changes.
    pub fn attach_cache_pool(&self, pool: FsBlobStore) {
        let _ = self.cache_pool.set(pool);
    }

    /// The attached cache pool, if any.
    pub fn cache_pool(&self) -> Option<&FsBlobStore> {
        self.cache_pool.get()
    }

    /// Change the orphan grace period (tests use zero).
    pub fn set_orphan_grace_ms(&self, grace_ms: u64) {
        self.orphan_grace_ms.store(grace_ms, Ordering::Relaxed);
    }

    pub(super) fn orphan_grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.orphan_grace_ms.load(Ordering::Relaxed))
    }

    /// Counted read transaction: increments [`MetricsSnapshot::read_txns`].
    pub(super) fn begin_read(&self) -> Result<redb::ReadTransaction, redb::TransactionError> {
        self.metrics
            .read_txns
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.db.begin_read()
    }

    /// Counted write transaction: increments [`MetricsSnapshot::write_txns`].
    pub(super) fn begin_write(&self) -> Result<WriteTransaction, redb::TransactionError> {
        self.metrics
            .write_txns
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.db.begin_write()
    }

    /// Record `n` index entries walked by a range scan.
    pub(super) fn note_scanned(&self, n: usize) {
        self.metrics
            .items_scanned
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    }

    /// Current counter values (tests measure deltas around a workload).
    pub fn metrics(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            read_txns: self.metrics.read_txns.load(std::sync::atomic::Ordering::Relaxed),
            write_txns: self
                .metrics
                .write_txns
                .load(std::sync::atomic::Ordering::Relaxed),
            items_scanned: self
                .metrics
                .items_scanned
                .load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Filesystem path of the underlying database, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Readiness probe: a write transaction can begin (it is dropped
    /// uncommitted, so the check mutates nothing). False when the database
    /// is unavailable read-write.
    pub fn check_write(&self) -> bool {
        self.db.begin_write().is_ok()
    }

    /// Milliseconds since the Unix epoch from the assembled clock.
    pub(super) fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    pub(super) fn next_id(&self) -> Id {
        self.ids.generate()
    }
}

impl crate::application::ports::ReadyCheck for RedbStore {
    fn is_ready(&self) -> bool {
        self.db.begin_read().is_ok()
    }
}

/// Create the data directory if needed and make sure it is private.
///
/// The directory holds the database (credentials, key material) and the
/// default blob roots, so on Unix it must be accessible by the owner only:
/// new directories are created `0700`, and an existing directory with any
/// group/other permission bit is tightened to `0700`. Group access is not
/// supported. Startup fails when the mode cannot be established (for
/// example a directory owned by someone else).
pub fn prepare_data_dir(dir: &Path) -> StoreResult<()> {
    crate::media::blob_store::create_private_dirs(dir)
        .map_err(|err| StoreError::Permissions(err.to_string()))?;
    crate::media::blob_store::enforce_private_dir(dir)
        .map_err(|err| StoreError::Permissions(err.to_string()))?;
    Ok(())
}

/// Create the database file `0600` before `redb` opens it, so its mode never
/// depends on the process umask. An existing file is left for
/// [`enforce_private_file`].
fn create_private_database_file(path: &Path) -> StoreResult<()> {
    match crate::media::blob_store::open_new_private(path) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(StoreError::Io(err)),
    }
}

/// Ensure the database file is owner-only on Unix (tightening a looser
/// mode); a no-op elsewhere.
fn enforce_private_file(path: &Path) -> StoreResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode: u32 = std::fs::metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |err| {
                    StoreError::Permissions(format!(
                        "database file {} has mode {mode:o} and could not be restricted: {err}",
                        path.display()
                    ))
                },
            )?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SystemClock, TimestampIdGenerator};

    #[test]
    fn opens_and_migrates_fresh_database() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(
            dir.path().join("birchtree.redb"),
            Arc::new(SystemClock),
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
        )
        .unwrap();
        assert!(store.path().exists());
    }

    #[test]
    fn existing_group_or_world_accessible_data_dir_is_tightened() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            for loose in [0o707, 0o755, 0o750, 0o770] {
                let nested = dir.path().join(format!("data-{loose:o}"));
                std::fs::create_dir(&nested).unwrap();
                std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(loose))
                    .unwrap();
                prepare_data_dir(&nested).expect("tightened to 0700");
                let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700, "from {loose:o}");
            }
        }
    }

    #[test]
    fn new_database_file_is_private_regardless_of_umask() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let store = RedbStore::open(
                dir.path().join("birchtree.redb"),
                Arc::new(SystemClock),
                Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
            )
            .unwrap();
            let mode = std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode & 0o077, 0, "mode {mode:o}");
        }
    }
}

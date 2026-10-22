//! The two physical byte pools behind one runtime handle.
//!
//! Local uploads, profile images and custom emoji live in the **durable**
//! pool and are never evicted. Remote attachment bytes (`remote/<id>` keys)
//! live in the **cache** pool and may be evicted at any time. The runtime
//! hands this type out as its `BlobStore`, so every existing caller keeps
//! working while the keys decide which tree is touched:
//!
//! - `remote/…` reads/writes/deletes go to the cache pool (reads fall back to
//!   the durable tree so blobs written before the split stay readable);
//! - everything else goes to the durable pool.
//!
//! `BlobStores::pool` exposes each pool directly for the code that must be
//! pool-exact (eviction and reconciliation only ever receive the cache pool).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::application::ports::{BlobPool, BlobStore, BlobStores};
use crate::domain::{Error, Result};
use crate::media::blob_store::FsBlobStore;
use crate::media::remote_cache;

/// Durable + cache handles with key-prefix routing.
#[derive(Debug, Clone)]
pub struct PooledBlobStores {
    durable: FsBlobStore,
    cache: FsBlobStore,
}

/// Which physical pool served a pool-aware read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolOrigin {
    /// The evictable cache pool (steady state for `remote/…` keys).
    Cache,
    /// A pre-split copy under the durable root. Readable so deploys that
    /// predate the split keep serving, but a shadow: the maintenance pass
    /// moves it into the cache pool and deletes this copy.
    DurableShadow,
}

/// A pool-aware open: the file streams from disk (the serve path wraps it in
/// `tokio::fs::File` and seeks for `Range`), so a 40 MiB remote never costs
/// a 40 MiB in-memory copy per request.
#[derive(Debug)]
pub struct PoolFile {
    /// The open blob bytes.
    pub file: File,
    /// Total blob length (for `Content-Length` / `Content-Range`).
    pub len: u64,
    /// Which pool the bytes came from (shadows trigger migration).
    pub origin: PoolOrigin,
}

/// Outcome of [`PooledBlobStores::migrate_legacy_shadow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowMigration {
    /// Both pools hold the key; the durable copy caught up callers should
    /// already have preferred the cache one. Only the cache copy is kept.
    AlreadyCached,
    /// The durable copy moved into the cache pool (verified, then the
    /// durable copy deleted) with this byte size.
    Migrated {
        /// Byte size of the moved blob.
        size: u64,
    },
    /// No durable shadow exists (nothing to move).
    NoShadow,
}

impl PooledBlobStores {
    /// Pair two already-opened pools. Use [`FsBlobStore::open_pair`] to open
    /// them; it refuses identical or overlapping roots.
    pub fn new(durable: FsBlobStore, cache: FsBlobStore) -> Self {
        Self { durable, cache }
    }

    /// The durable pool handle.
    pub fn durable(&self) -> &FsBlobStore {
        &self.durable
    }

    /// The cache pool handle.
    pub fn cache(&self) -> &FsBlobStore {
        &self.cache
    }

    /// The cache pool root. The public file route streams `remote/…` keys
    /// from here (the parent wires this through the proposed
    /// `BlobStore::cache_root_path` port addition; until then the route
    /// reaches it through [`PooledBlobStores::open_in_pool`]).
    pub fn cache_root(&self) -> &Path {
        self.cache.root()
    }

    fn pool_for(&self, key: &str) -> &FsBlobStore {
        if remote_cache::is_cache_key(key) {
            &self.cache
        } else {
            &self.durable
        }
    }

    /// Open `key` in its pool for streaming, without buffering the blob.
    ///
    /// Cache keys (`remote/…`) open in the cache pool and fall back to the
    /// durable pool only for pre-split shadows; every other key opens in
    /// the durable pool. Compare with [`BlobStore::get`], which buffers the
    /// whole blob: serve paths must prefer this and
    /// [`PooledBlobStores::read_range_in_pool`].
    pub fn open_in_pool(&self, key: &str) -> Result<Option<PoolFile>> {
        let primary: &FsBlobStore = self.pool_for(key);
        if let Some((file, len)) = primary.open_at(key)? {
            let origin = if std::ptr::eq(primary, &self.cache) {
                PoolOrigin::Cache
            } else {
                PoolOrigin::DurableShadow
            };
            return Ok(Some(PoolFile { file, len, origin }));
        }
        if remote_cache::is_cache_key(key)
            && let Some((file, len)) = self.durable.open_at(key)?
        {
            return Ok(Some(PoolFile {
                file,
                len,
                origin: PoolOrigin::DurableShadow,
            }));
        }
        Ok(None)
    }

    /// Read at most `len` bytes of `key` from `offset`, plus the total blob
    /// length, locating the bytes with [`PooledBlobStores::open_in_pool`].
    /// Bounded by `len`: a small `Range` of a large remote allocates only
    /// the range.
    pub fn read_range_in_pool(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> Result<Option<(Vec<u8>, u64, PoolOrigin)>> {
        use std::io::{Read as _, Seek as _};

        let Some(mut located) = self.open_in_pool(key)? else {
            return Ok(None);
        };
        let start: u64 = offset.min(located.len);
        let count: u64 = located.len.saturating_sub(start).min(len);
        located
            .file
            .seek(std::io::SeekFrom::Start(start))
            .map_err(|err| Error::Storage(format!("seek pooled blob {key}: {err}")))?;
        let mut bytes: Vec<u8> = Vec::new();
        located
            .file
            .take(count)
            .read_to_end(&mut bytes)
            .map_err(|err| Error::Storage(format!("read pooled blob range {key}: {err}")))?;
        Ok(Some((bytes, located.len, located.origin)))
    }

    /// Move a pre-split durable `remote/…` shadow into the cache pool.
    ///
    /// Read-through-move primitive for the deliberate migration: when the
    /// cache pool already holds the key the durable copy is just a stale
    /// duplicate and is deleted; otherwise the durable bytes are copied into
    /// the cache pool, the copy is verified byte-for-byte in size, and only
    /// then is the durable copy deleted. The caller updates the ledger
    /// metadata/accounting (see
    /// [`CacheMaintenance::migrate_durable_shadows`](crate::media::cache_maintenance::CacheMaintenance::migrate_durable_shadows)).
    /// Refuses non-cache keys, so durable local uploads can never move.
    pub fn migrate_legacy_shadow(&self, key: &str) -> Result<ShadowMigration> {
        if !remote_cache::is_cache_key(key) {
            return Err(Error::InvalidInput(format!(
                "refusing to migrate a non-cache key: {key}"
            )));
        }
        if self.cache.exists(key)? {
            if self.durable.exists(key)? {
                self.durable.delete(key)?;
            }
            return Ok(ShadowMigration::AlreadyCached);
        }
        let Some(bytes) = self.durable.get(key)? else {
            return Ok(ShadowMigration::NoShadow);
        };
        self.cache.put(key, &bytes)?;
        let cached: Option<u64> = self.cache.size_of(key)?;
        if cached != Some(bytes.len() as u64) {
            let _ = self.cache.delete(key);
            return Err(Error::Storage(format!(
                "shadow copy of {key} failed verification"
            )));
        }
        self.durable.delete(key)?;
        Ok(ShadowMigration::Migrated {
            size: bytes.len() as u64,
        })
    }
}

impl BlobStores for PooledBlobStores {
    fn pool(&self, pool: BlobPool) -> Arc<dyn BlobStore> {
        match pool {
            BlobPool::Durable => Arc::new(self.durable.clone()),
            BlobPool::Cache => Arc::new(self.cache.clone()),
        }
    }
}

impl BlobStore for PooledBlobStores {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        self.pool_for(key).put(key, bytes)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let primary = self.pool_for(key).get(key)?;
        if primary.is_none() && remote_cache::is_cache_key(key) {
            // Pre-split blobs sit under the durable root (lazy move).
            return self.durable.get(key);
        }
        Ok(primary)
    }

    fn delete(&self, key: &str) -> Result<()> {
        // Cache keys are deleted in both pools: eviction (or an operator)
        // must never leave a durable shadow behind that the public route —
        // which checks the durable root first — would keep serving.
        if remote_cache::is_cache_key(key) {
            self.cache.delete(key)?;
            self.durable.delete(key)?;
            return Ok(());
        }
        self.pool_for(key).delete(key)
    }

    fn exists(&self, key: &str) -> Result<bool> {
        let primary = self.pool_for(key).exists(key)?;
        if !primary && remote_cache::is_cache_key(key) {
            return self.durable.exists(key);
        }
        Ok(primary)
    }

    /// The durable root, where the public file route and local uploads
    /// stage. The remote-media fetch also stages its download under this
    /// root; `publish_tmp` moves it into the cache pool (copying when the
    /// pools sit on different filesystems).
    fn root_path(&self) -> Option<PathBuf> {
        self.durable.root_path()
    }

    /// The cache-pool root, so the public file route streams `remote/…`
    /// keys from disk instead of buffering them through [`BlobStore::get`].
    fn cache_root_path(&self) -> Option<PathBuf> {
        Some(self.cache.root().to_path_buf())
    }

    fn publish_tmp(&self, tmp: &Path, key: &str) -> Result<()> {
        self.pool_for(key).publish_tmp(tmp, key)
    }

    fn put_streamed(&self, key: &str, bytes: &[u8], max_bytes: u64) -> Result<()> {
        self.pool_for(key).put_streamed(key, bytes, max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_remote_keys_to_the_cache_pool() {
        let dir = tempfile::tempdir().unwrap();
        let (durable, cache) =
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
                .unwrap();
        let pools = PooledBlobStores::new(durable, cache);
        pools.put("remote/7", b"cached").unwrap();
        pools
            .put("ab/cd/0123456789abcdef/original.png", b"local")
            .unwrap();
        assert!(dir.path().join("media_cache/remote/7").exists());
        assert!(!dir.path().join("media/remote/7").exists());
        assert!(
            dir.path()
                .join("media/ab/cd/0123456789abcdef/original.png")
                .exists()
        );
        assert_eq!(pools.get("remote/7").unwrap(), Some(b"cached".to_vec()));

        // Legacy durable `remote/` bytes stay readable.
        pools.durable().put("remote/9", b"legacy").unwrap();
        assert_eq!(pools.get("remote/9").unwrap(), Some(b"legacy".to_vec()));
    }

    #[test]
    fn open_in_pool_locates_the_writing_pool() {
        let dir = tempfile::tempdir().unwrap();
        let (durable, cache) =
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
                .unwrap();
        let pools = PooledBlobStores::new(durable, cache);
        pools.put("remote/7", b"cached").unwrap();
        pools
            .put("ab/cd/0123456789abcdef/original.png", b"local")
            .unwrap();

        let located = pools.open_in_pool("remote/7").unwrap().unwrap();
        assert_eq!(located.origin, PoolOrigin::Cache);
        assert_eq!(located.len, 6);

        let located = pools
            .open_in_pool("ab/cd/0123456789abcdef/original.png")
            .unwrap()
            .unwrap();
        assert_eq!(located.origin, PoolOrigin::DurableShadow);
        assert_eq!(located.len, 5);

        assert!(pools.open_in_pool("remote/missing").unwrap().is_none());
        assert!(pools.open_in_pool("../escape").is_err());
    }

    #[test]
    fn open_in_pool_reports_legacy_shadows() {
        let dir = tempfile::tempdir().unwrap();
        let (durable, cache) =
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
                .unwrap();
        let pools = PooledBlobStores::new(durable, cache);
        pools.durable().put("remote/9", b"legacy").unwrap();

        let located = pools.open_in_pool("remote/9").unwrap().unwrap();
        assert_eq!(located.origin, PoolOrigin::DurableShadow);
        use std::io::Read as _;
        let mut bytes = Vec::new();
        located
            .file
            .take(64)
            .read_to_end(&mut bytes)
            .expect("read shadow");
        assert_eq!(bytes, b"legacy");
    }

    #[test]
    fn range_reads_are_bounded_by_the_range() {
        let dir = tempfile::tempdir().unwrap();
        let (durable, cache) =
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
                .unwrap();
        let pools = PooledBlobStores::new(durable, cache);
        let bytes: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        pools.put("remote/11", &bytes).unwrap();

        let (head, total, origin) = pools
            .read_range_in_pool("remote/11", 0, 64)
            .unwrap()
            .unwrap();
        assert_eq!(origin, PoolOrigin::Cache);
        assert_eq!(total, 4096);
        assert_eq!(head, bytes[..64]);
        let (mid, _, _) = pools
            .read_range_in_pool("remote/11", 4000, 512)
            .unwrap()
            .unwrap();
        assert_eq!(mid, bytes[4000..]);
        assert!(
            pools
                .read_range_in_pool("remote/gone", 0, 8)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cache_deletes_clear_both_pools() {
        let dir = tempfile::tempdir().unwrap();
        let (durable, cache) =
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
                .unwrap();
        let pools = PooledBlobStores::new(durable, cache);
        pools.put("remote/7", b"cached").unwrap();
        pools.durable().put("remote/7", b"shadow").unwrap();
        pools.delete("remote/7").unwrap();
        assert!(!pools.exists("remote/7").unwrap());
        assert!(!dir.path().join("media_cache/remote/7").exists());
        assert!(!dir.path().join("media/remote/7").exists());

        // Local keys still delete from the durable pool only.
        pools
            .put("ab/cd/0123456789abcdef/original.png", b"local")
            .unwrap();
        pools.delete("ab/cd/0123456789abcdef/original.png").unwrap();
        assert!(
            !dir.path()
                .join("media/ab/cd/0123456789abcdef/original.png")
                .exists()
        );
    }

    #[test]
    fn shadow_migration_moves_verifies_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let (durable, cache) =
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
                .unwrap();
        let pools = PooledBlobStores::new(durable, cache);

        assert_eq!(
            pools.migrate_legacy_shadow("remote/9").unwrap(),
            ShadowMigration::NoShadow
        );
        assert!(
            pools
                .migrate_legacy_shadow("ab/cd/ef/original.png")
                .is_err()
        );

        pools.durable().put("remote/9", b"legacy").unwrap();
        assert_eq!(
            pools.migrate_legacy_shadow("remote/9").unwrap(),
            ShadowMigration::Migrated { size: 6 }
        );
        assert_eq!(pools.get("remote/9").unwrap(), Some(b"legacy".to_vec()));
        assert!(!dir.path().join("media/remote/9").exists());
        assert!(dir.path().join("media_cache/remote/9").exists());

        // A stale duplicate loses to the cache copy.
        pools.durable().put("remote/9", b"stale").unwrap();
        assert_eq!(
            pools.migrate_legacy_shadow("remote/9").unwrap(),
            ShadowMigration::AlreadyCached
        );
        assert!(!dir.path().join("media/remote/9").exists());
        assert_eq!(pools.get("remote/9").unwrap(), Some(b"legacy".to_vec()));
    }
}

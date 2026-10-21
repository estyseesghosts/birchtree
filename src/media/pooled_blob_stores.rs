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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::application::ports::{BlobPool, BlobStore, BlobStores};
use crate::domain::Result;
use crate::media::blob_store::FsBlobStore;
use crate::media::remote_cache;

/// Durable + cache handles with key-prefix routing.
#[derive(Debug, Clone)]
pub struct PooledBlobStores {
    durable: FsBlobStore,
    cache: FsBlobStore,
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

    fn pool_for(&self, key: &str) -> &FsBlobStore {
        if remote_cache::is_cache_key(key) {
            &self.cache
        } else {
            &self.durable
        }
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
        pools.put("ab/cd/0123456789abcdef/original.png", b"local").unwrap();
        assert!(dir.path().join("media_cache/remote/7").exists());
        assert!(!dir.path().join("media/remote/7").exists());
        assert!(dir.path().join("media/ab/cd/0123456789abcdef/original.png").exists());
        assert_eq!(pools.get("remote/7").unwrap(), Some(b"cached".to_vec()));

        // Legacy durable `remote/` bytes stay readable.
        pools.durable().put("remote/9", b"legacy").unwrap();
        assert_eq!(pools.get("remote/9").unwrap(), Some(b"legacy".to_vec()));
    }
}

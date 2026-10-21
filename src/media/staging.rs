//! Staging transaction for blob writes that precede a metadata commit.
//!
//! File storage cannot join a `redb` transaction, so use cases stage bytes
//! first and commit the row that references them afterwards. A
//! [`StagingGuard`] records every key staged on the way and removes the
//! uncommitted ones when it is dropped without [`StagingGuard::commit`], so
//! ordinary error paths (a failed second `put`, a failed metadata commit, an
//! early return) never leave orphans behind. Process crashes, which
//! destructors cannot cover, converge through the reconciler instead.
//!
//! A key that already held bytes before the guard overwrote it is *not*
//! deleted on rollback: that blob may still be referenced by a committed
//! row, and deleting it would turn a failed update into data loss.

use std::path::Path;

use crate::application::ports::BlobStore;
use crate::domain::Result;

/// One key staged by the guard.
#[derive(Debug)]
struct StagedKey {
    key: String,
    /// Whether the key already held bytes before this guard wrote it.
    replaced_existing: bool,
}

/// Rolls back staged-but-uncommitted blobs on drop.
#[must_use = "dropping an uncommitted guard deletes everything it staged"]
pub struct StagingGuard<'store> {
    blobs: &'store dyn BlobStore,
    staged: Vec<StagedKey>,
    committed: bool,
}

impl<'store> StagingGuard<'store> {
    /// A guard over `blobs` with nothing staged yet.
    pub fn new(blobs: &'store dyn BlobStore) -> Self {
        Self {
            blobs,
            staged: Vec::new(),
            committed: false,
        }
    }

    /// Stage `bytes` under `key`. The key is recorded *before* the write, so
    /// a write that fails half-way is still cleaned up.
    pub fn put(&mut self, key: &str, bytes: &[u8]) -> Result<()> {
        self.record(key);
        self.blobs.put(key, bytes)
    }

    /// Publish an already-staged temporary file onto `key` (one rename, no
    /// copy). The temporary file is removed by the blob store on failure.
    pub fn publish_file(&mut self, tmp: &Path, key: &str) -> Result<()> {
        self.record(key);
        self.blobs.publish_tmp(tmp, key)
    }

    fn record(&mut self, key: &str) {
        let replaced_existing: bool = self.blobs.exists(key).unwrap_or(false);
        self.staged.push(StagedKey {
            key: key.to_string(),
            replaced_existing,
        });
    }

    /// Keys staged so far (for tests and diagnostics).
    pub fn staged_keys(&self) -> impl Iterator<Item = &str> {
        self.staged.iter().map(|staged| staged.key.as_str())
    }

    /// The metadata commit succeeded: keep everything that was staged.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for StagingGuard<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for staged in self.staged.iter().rev() {
            if staged.replaced_existing {
                continue;
            }
            if let Err(err) = self.blobs.delete(&staged.key) {
                tracing::warn!(key = %staged.key, error = %err, "staged blob left for the reconciler");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::blob_store::FsBlobStore;

    #[test]
    fn uncommitted_guard_removes_new_keys_but_not_replaced_ones() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
        blobs.put("a/old", b"old").unwrap();
        {
            let mut guard = StagingGuard::new(&blobs);
            guard.put("a/new", b"new").unwrap();
            guard.put("a/old", b"replacement").unwrap();
        }
        assert_eq!(blobs.get("a/new").unwrap(), None);
        assert!(blobs.get("a/old").unwrap().is_some());
    }

    #[test]
    fn committed_guard_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
        let mut guard = StagingGuard::new(&blobs);
        guard.put("a/new", b"new").unwrap();
        guard.commit();
        assert_eq!(blobs.get("a/new").unwrap(), Some(b"new".to_vec()));
    }
}

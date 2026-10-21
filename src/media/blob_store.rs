//! Filesystem blob store.
//!
//! Keys are relative paths (`000/000/042/original/ab12cd.jpg`, emoji images,
//! ...). The root is created `0700`; keys are validated against traversal so
//! a crafted filename can never escape the media root.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::application::ports::BlobStore;
use crate::domain::{Error, Result};

/// A `BlobStore` rooted at a directory (default: `<data_dir>/media`).
///
/// `Clone` so callers can hold the durable and cache handles side by side;
/// each handle owns its root and never touches the other's tree.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
}

/// Process-wide counter so tmp siblings never collide across threads.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl FsBlobStore {
    /// Open (creating) the blob root under `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|err| {
            Error::Storage(format!("create blob root {}: {err}", root.display()))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self { root })
    }

    /// Open the durable + cache pair in one call (`docs/database.md` Agent A).
    ///
    /// The durable root keeps the pre-overhaul path (`<data_dir>/media`) so
    /// pre-migration blobs stay readable in place (lazy move); the cache root
    /// is a fresh tree (`<data_dir>/media_cache`). The two roots must differ —
    /// `Config::validate` enforces this too, this is the defense in depth.
    pub fn open_pair(
        durable: impl Into<PathBuf>,
        cache: impl Into<PathBuf>,
    ) -> Result<(Self, Self)> {
        let durable = durable.into();
        let cache = cache.into();
        if durable == cache {
            return Err(Error::InvalidInput(
                "durable and cache blob roots must differ".into(),
            ));
        }
        Ok((Self::open(durable)?, Self::open(cache)?))
    }

    /// The absolute root this handle writes under (observability/tests).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Unique tmp sibling for `dest`, in the same directory so the final
    /// `rename` is an atomic same-filesystem move. Dot-prefixed so a crashed
    /// writer's residue never looks like a readable blob.
    fn tmp_sibling(&self, dest: &Path) -> PathBuf {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "blob".to_string());
        dest.with_file_name(format!(
            ".{name}.part-{}-{seq}",
            std::process::id()
        ))
    }

    /// Atomic write: stage to a tmp sibling in-root, `sync_all`, then rename
    /// over the destination. A failed write (or a crash before rename) leaves
    /// the old bytes — or nothing — at the key, never a torn prefix; residue
    /// is a dot-prefixed tmp file for the reconciler to sweep, never a blob
    /// a reader can see.
    fn put_atomic(&self, dest: &Path, bytes: &[u8]) -> Result<()> {
        use std::io::Write as _;

        let tmp = self.tmp_sibling(dest);
        let mut file = std::fs::File::create(&tmp)
            .map_err(|err| Error::Storage(format!("create tmp blob {}: {err}", tmp.display())))?;
        if let Err(err) = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                drop(file);
                Ok(())
            })
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Storage(format!(
                "write tmp blob {}: {err}",
                tmp.display()
            )));
        }
        // `rename` overwrites atomically on POSIX; on Windows the target must
        // be removed first.
        if std::fs::rename(&tmp, dest).is_err() {
            let _ = std::fs::remove_file(dest);
            if let Err(err) = std::fs::rename(&tmp, dest) {
                let _ = std::fs::remove_file(&tmp);
                return Err(Error::Storage(format!(
                    "rename tmp blob {}: {err}",
                    dest.display()
                )));
            }
        }
        Ok(())
    }

    /// Resolve a key to an absolute path, rejecting traversal.
    fn resolve(&self, key: &str) -> Result<PathBuf> {
        if key.is_empty() {
            return Err(Error::InvalidInput("empty blob key".into()));
        }
        let path = Path::new(key);
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir))
        {
            return Err(Error::InvalidInput(format!("invalid blob key: {key}")));
        }
        Ok(self.root.join(path))
    }
}

impl BlobStore for FsBlobStore {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::Storage(format!("create blob dir {}: {err}", parent.display()))
            })?;
        }
        self.put_atomic(&path, bytes)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.resolve(key)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::Storage(format!(
                "read blob {}: {err}",
                path.display()
            ))),
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        if key.is_empty() {
            return Ok(());
        }
        let path = self.resolve(key)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::Storage(format!(
                "delete blob {}: {err}",
                path.display()
            ))),
        }
    }

    fn exists(&self, key: &str) -> Result<bool> {
        if key.is_empty() {
            return Ok(false);
        }
        Ok(self.resolve(key)?.exists())
    }

    fn root_path(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn publish_tmp(&self, tmp: &Path, key: &str) -> Result<()> {
        let dest = self.resolve(key)?;
        crate::media::fetch_stream::publish_tmp(tmp, &dest)
    }

    fn put_streamed(&self, key: &str, bytes: &[u8], max_bytes: u64) -> Result<()> {
        if bytes.len() as u64 > max_bytes {
            return Err(Error::InvalidInput(format!("stream body exceeds {max_bytes} bytes")));
        }
        // Bytes are already resident here (the HTTP layer buffered them);
        // the atomic tmp+rename `put` below is what keeps torn writes
        // invisible (true request-body streaming awaits endpoint work).
        self.put(key, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsBlobStore::open(dir.path().join("media")).unwrap();
        store.put("000/000/001/original/a.jpg", b"bytes").unwrap();
        assert_eq!(store.get("000/000/001/original/a.jpg").unwrap(), Some(b"bytes".to_vec()));
        assert!(store.exists("000/000/001/original/a.jpg").unwrap());
        store.delete("000/000/001/original/a.jpg").unwrap();
        assert_eq!(store.get("000/000/001/original/a.jpg").unwrap(), None);
        // Traversal and absolute keys are rejected.
        assert!(store.get("../escape").is_err());
        assert!(store.put("/etc/passwd", b"x").is_err());
        assert!(store.get("").is_err());
    }

    #[test]
    fn pair_rejects_shared_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media")).is_err()
        );
    }
}

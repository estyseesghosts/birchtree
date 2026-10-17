//! Filesystem blob store.
//!
//! Keys are relative paths (`000/000/042/original/ab12cd.jpg`, emoji images,
//! ...). The root is created `0700`; keys are validated against traversal so
//! a crafted filename can never escape the media root.

use std::path::{Path, PathBuf};

use crate::application::ports::BlobStore;
use crate::domain::{Error, Result};

/// A `BlobStore` rooted at a directory (default: `<data_dir>/media`).
pub struct FsBlobStore {
    root: PathBuf,
}

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
        std::fs::write(&path, bytes)
            .map_err(|err| Error::Storage(format!("write blob {}: {err}", path.display())))?;
        Ok(())
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
}

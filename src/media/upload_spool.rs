//! Spooling of one upload body to a temporary file.
//!
//! The multipart handler writes the file part chunk by chunk into a private
//! temporary file under the byte cap, so an oversized or malformed upload is
//! rejected before it is ever resident in memory, and the use case later
//! moves the file onto its blob key with one rename instead of copying.
//!
//! The spool owns its file: dropping it (client disconnect, malformed body,
//! failed validation, any early return) removes the file, and
//! [`UploadSpool::disarm`] hands ownership of the path to the caller once the
//! bytes are committed to move.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;

use crate::domain::Error;

/// Longest leading slice kept in memory for content sniffing.
pub const SNIFF_PREFIX_BYTES: usize = 64 * 1024;

/// Why a chunk could not be spooled.
#[derive(Debug)]
pub enum SpoolError {
    /// The body crossed the byte cap.
    TooLarge { limit: u64 },
    /// The temporary file could not be created or written.
    Storage(Error),
}

/// A capped temporary upload file that deletes itself unless disarmed.
#[derive(Debug)]
pub struct UploadSpool {
    path: PathBuf,
    file: Option<tokio::fs::File>,
    written: u64,
    limit: u64,
    prefix: Vec<u8>,
    armed: bool,
}

impl UploadSpool {
    /// Start a spool of at most `limit` bytes. The file lives under
    /// `<blob root>/tmp` when `blob_root` is known (so publishing it is a
    /// same-filesystem rename), otherwise in the system temp directory.
    pub async fn create(blob_root: Option<&Path>, limit: u64) -> Result<Self, SpoolError> {
        let path: PathBuf = match blob_root {
            Some(root) => crate::media::fetch_stream::tmp_path_for(root).map_err(SpoolError::Storage)?,
            None => std::env::temp_dir().join(format!(
                "birchtree-upload-{}-{:032x}.part",
                std::process::id(),
                rand::random::<u128>()
            )),
        };
        let std_file = crate::media::blob_store::open_new_private(&path).map_err(|err| {
            SpoolError::Storage(Error::Storage(format!(
                "create upload spool {}: {err}",
                path.display()
            )))
        })?;
        Ok(Self {
            path,
            file: Some(tokio::fs::File::from_std(std_file)),
            written: 0,
            limit,
            prefix: Vec::new(),
            armed: true,
        })
    }

    /// Append one chunk, failing once the running total would pass the cap.
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), SpoolError> {
        let total: u64 = self.written.saturating_add(chunk.len() as u64);
        if total > self.limit {
            return Err(SpoolError::TooLarge { limit: self.limit });
        }
        let Some(file) = self.file.as_mut() else {
            return Err(SpoolError::Storage(Error::Internal(
                "upload spool already finished".into(),
            )));
        };
        file.write_all(chunk).await.map_err(|err| {
            SpoolError::Storage(Error::Storage(format!(
                "write upload spool {}: {err}",
                self.path.display()
            )))
        })?;
        if self.prefix.len() < SNIFF_PREFIX_BYTES {
            let room: usize = SNIFF_PREFIX_BYTES - self.prefix.len();
            self.prefix.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
        self.written = total;
        Ok(())
    }

    /// Flush and close the file; the spool stays valid for reading.
    pub async fn finish(&mut self) -> Result<(), SpoolError> {
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        file.flush().await.map_err(|err| {
            SpoolError::Storage(Error::Storage(format!(
                "flush upload spool {}: {err}",
                self.path.display()
            )))
        })?;
        file.sync_all().await.map_err(|err| {
            SpoolError::Storage(Error::Storage(format!(
                "sync upload spool {}: {err}",
                self.path.display()
            )))
        })?;
        Ok(())
    }

    /// Bytes written so far.
    pub fn len(&self) -> u64 {
        self.written
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.written == 0
    }

    /// The leading bytes (up to [`SNIFF_PREFIX_BYTES`]) for sniffing.
    pub fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    /// The temporary file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the whole spooled file into memory. Bounded by the cap the spool
    /// was created with.
    pub fn read_all(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(&self.path)
    }

    /// Give up ownership of the file: it is no longer deleted on drop. Call
    /// when the file has been (or is being) moved by `publish_tmp`.
    pub fn disarm(&mut self) -> PathBuf {
        self.armed = false;
        self.path.clone()
    }
}

impl Drop for UploadSpool {
    fn drop(&mut self) {
        if self.armed {
            self.file.take();
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn caps_cleans_up_and_keeps_a_sniff_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = UploadSpool::create(Some(dir.path()), 10).await.expect("create");
        let path = spool.path().to_path_buf();
        spool.write_chunk(b"01234").await.expect("first");
        assert_eq!(spool.prefix(), b"01234");
        assert!(matches!(
            spool.write_chunk(b"567890").await,
            Err(SpoolError::TooLarge { limit: 10 })
        ));
        drop(spool);
        assert!(!path.exists(), "an aborted spool leaves no file");
    }

    #[tokio::test]
    async fn disarmed_spool_keeps_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = UploadSpool::create(Some(dir.path()), 100).await.expect("create");
        spool.write_chunk(b"abc").await.expect("write");
        spool.finish().await.expect("finish");
        let path = spool.disarm();
        drop(spool);
        assert_eq!(std::fs::read(path).unwrap(), b"abc");
    }
}

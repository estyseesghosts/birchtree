//! Stream-to-tmp download helper (Agent C, storage overhaul `docs/database.md`).
//!
//! Used by `fetch_and_attach` (`src/federation/media.rs`, which today
//! buffers the whole body in a `Vec` behind `MAX_REMOTE_MEDIA_BYTES`) and
//! `upload_media` (`src/application/media.rs`, which today `put`s the whole
//! body at once). Both callers move to this shape:
//!
//! 1. [`check_content_length`] — `Content-Length` is advisory only: reject
//!    early when the declared length already exceeds the cap, otherwise
//!    proceed (a dishonest-small or absent header is caught mid-body).
//! 2. [`copy_stream_capped`] / [`stage_slices_capped`] — stream the body to
//!    `tmp/{rand}.part` under the same root, counting bytes as they land and
//!    aborting mid-body when the cap is exceeded. The partial tmp file is
//!    removed on abort, so a failed download leaves no residue that reads
//!    as a blob.
//! 3. Atomic rename onto the final key on success. Readers therefore see
//!    either the complete bytes or nothing — a torn write reads as `None`
//!    and heals when the fetch is retried ([`read_final`]).
//!
//! No blobs move inside a `redb` transaction: callers stage bytes here,
//! commit metadata afterwards, and the [`sweep_tmp`] residue scan (also used
//! by the reconciler) bounds crash leftovers. No shell is used anywhere.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::domain::{Error, Result};

/// Directory under a blob root that holds in-flight downloads.
pub const TMP_DIR_NAME: &str = "tmp";

/// Suffix marking an in-flight download (never a readable blob).
pub const TMP_SUFFIX: &str = ".part";

/// Process-wide counter so tmp names never collide across threads.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Canonical oversize error: the body exceeded `limit` bytes mid-stream.
///
/// Callers map this into their own envelope (the federation worker reports
/// `"attachment is larger than the limit"`; uploads report the configured
/// file-too-large body).
pub fn oversized(limit: u64) -> Error {
    Error::InvalidInput(format!("stream body exceeds {limit} bytes"))
}

/// Advisory `Content-Length` gate.
///
/// A declared length above `limit` aborts before any body byte is read. A
/// missing header — or a dishonestly small one — passes here and is caught
/// mid-body by the streaming byte counter instead.
pub fn check_content_length(declared: Option<u64>, limit: u64) -> Result<()> {
    if declared.is_some_and(|len| len > limit) {
        return Err(oversized(limit));
    }
    Ok(())
}

/// Resolve `key` under `root` with the blob store's key validation (empty,
/// absolute, parent-dir, dot-prefixed and reserved components are refused).
fn resolve_key(root: &Path, key: &str) -> Result<PathBuf> {
    crate::media::blob_store::validate_key(key)?;
    Ok(root.join(key))
}

/// Allocate a fresh tmp path for a download staged under `root`.
///
/// The file lives in `<root>/tmp/{pid}-{seq}-{rand}.part`: same filesystem
/// as the final key (so the publish step is an atomic rename), and
/// dot-free but `.part`-suffixed so neither blob readers nor the final-name
/// listing ever mistake it for a committed blob.
pub fn tmp_path_for(root: &Path) -> Result<PathBuf> {
    let dir = root.join(TMP_DIR_NAME);
    crate::media::blob_store::create_private_dirs(&dir)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let rand: u128 = rand::random();
    Ok(dir.join(format!(
        "{}-{seq}-{rand:032x}{TMP_SUFFIX}",
        std::process::id()
    )))
}

/// Atomically publish a staged tmp file onto `dest`.
///
/// One `rename` replaces the destination; the destination is never removed
/// first, so a failed publish leaves the previous bytes in place. A failed
/// publish removes the tmp file.
pub fn publish_tmp(tmp: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        crate::media::blob_store::create_private_dirs(parent)?;
    }
    let published: std::io::Result<()> = crate::media::blob_store::replace_file(tmp, dest)
        .and_then(|()| crate::media::blob_store::sync_parent_dir(dest));
    if let Err(err) = published {
        let _ = std::fs::remove_file(tmp);
        return Err(Error::Storage(format!(
            "rename tmp blob {}: {err}",
            dest.display()
        )));
    }
    Ok(())
}

/// Stream `chunks` to `tmp/{rand}.part` under `root`, aborting mid-body when
/// more than `limit` bytes arrive, then atomically rename onto `key`.
///
/// Returns the staged byte count. On cap breach the tmp file is removed and
/// [`oversized`] is returned; nothing is left behind that reads as a blob.
/// This is the `upload_media` file-body path and the synchronous test hook
/// for the streaming counter.
pub fn stage_slices_capped(
    root: &Path,
    key: &str,
    chunks: &[&[u8]],
    limit: u64,
) -> Result<u64> {
    use std::io::Write as _;

    let dest = resolve_key(root, key)?;
    let tmp = tmp_path_for(root)?;
    let mut written: u64 = 0;
    let abort = |tmp: &Path| {
        let _ = std::fs::remove_file(tmp);
    };
    let mut file = crate::media::blob_store::open_new_private(&tmp)
        .map_err(|err| Error::Storage(format!("create tmp blob {}: {err}", tmp.display())))?;
    for chunk in chunks {
        if written.saturating_add(chunk.len() as u64) > limit {
            drop(file);
            abort(&tmp);
            return Err(oversized(limit));
        }
        if let Err(err) = file.write_all(chunk) {
            drop(file);
            abort(&tmp);
            return Err(Error::Storage(format!(
                "write tmp blob {}: {err}",
                tmp.display()
            )));
        }
        written += chunk.len() as u64;
    }
    if let Err(err) = file.sync_all() {
        drop(file);
        abort(&tmp);
        return Err(Error::Storage(format!(
            "sync tmp blob {}: {err}",
            tmp.display()
        )));
    }
    drop(file);
    publish_tmp(&tmp, &dest)?;
    Ok(written)
}

/// Convenience wrapper for a single contiguous body (the `upload_media`
/// file-body path): advisory length check, then stage + atomic rename.
pub fn stage_bytes_capped(root: &Path, key: &str, bytes: &[u8], limit: u64) -> Result<u64> {
    check_content_length(Some(bytes.len() as u64), limit)?;
    stage_slices_capped(root, key, &[bytes], limit)
}

/// Healing read of a staged key: `None` when the blob is missing.
///
/// A crash between the tmp write and the rename leaves only a `.part`
/// file, which never resolves to a key — so a torn write reads as `None`
/// here and heals when the fetch is retried and renames onto the key.
pub fn read_final(root: &Path, key: &str) -> Result<Option<Vec<u8>>> {
    let path = resolve_key(root, key)?;
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::Storage(format!(
            "read blob {}: {err}",
            path.display()
        ))),
    }
}

/// Remove stale `*.part` residue under `<root>/tmp` (crashed downloads that
/// never renamed). Returns the number of files removed. Idempotent.
pub fn sweep_tmp(root: &Path) -> Result<usize> {
    let dir = root.join(TMP_DIR_NAME);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(Error::Storage(format!(
                "list tmp dir {}: {err}",
                dir.display()
            )));
        }
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "part") && path.is_file() {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Maximum directory entries one [`sweep_stale_residue`] pass inspects.
pub const RESIDUE_SWEEP_VISIT_CAP: usize = 200_000;

/// Remove staging residue older than `min_age` under `root`: `tmp/*.part`
/// downloads and dot-prefixed `.name.part-*` atomic-write siblings anywhere
/// in the tree. The age gate keeps a live writer's file safe, unlike
/// [`sweep_tmp`] (which is for startup, before any writer exists). At most
/// `cap` files are removed and [`RESIDUE_SWEEP_VISIT_CAP`] entries visited
/// per call; returns the number removed.
pub fn sweep_stale_residue(
    root: &Path,
    min_age: std::time::Duration,
    cap: usize,
) -> Result<usize> {
    let now = std::time::SystemTime::now();
    let mut removed = 0usize;
    let mut visited = 0usize;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(Error::Storage(format!(
                    "list residue dir {}: {err}",
                    dir.display()
                )));
            }
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > RESIDUE_SWEEP_VISIT_CAP || removed >= cap {
                return Ok(removed);
            }
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let in_tmp = dir.file_name().is_some_and(|n| n == TMP_DIR_NAME)
                && dir.parent() == Some(root);
            let is_residue = (in_tmp && name.ends_with(TMP_SUFFIX))
                || (name.starts_with('.') && name.contains(".part-"));
            if !is_residue || !meta.is_file() {
                continue;
            }
            let old_enough = meta
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= min_age);
            if old_enough && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Copy an async byte stream (e.g. `reqwest::Response::bytes_stream`) to an
/// already-allocated `tmp` path, counting bytes and aborting mid-body when
/// `limit` is exceeded.
///
/// The stream item type is generic over any byte-chunk/error pair
/// (`reqwest`'s `Bytes`/`reqwest::Error` satisfy the bounds, as do the
/// `Vec<u8>`/`std::io::Error` test doubles). On cap breach the tmp file is
/// removed and [`oversized`] is returned; on a stream error the tmp file is
/// removed and the failure is reported as `Storage` (the caller decides
/// retry vs quarantine — a transport failure is retryable, an oversize
/// body is not).
pub async fn copy_stream_capped<S, B, E>(stream: S, tmp: &Path, limit: u64) -> Result<u64>
where
    S: futures_util::Stream<Item = std::result::Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let mut stream = stream;
    let mut file = crate::media::blob_store::open_new_private(tmp)
        .map(tokio::fs::File::from_std)
        .map_err(|err| Error::Storage(format!("create tmp blob {}: {err}", tmp.display())))?;
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                drop(file);
                let _ = tokio::fs::remove_file(tmp).await;
                return Err(Error::Storage(format!(
                    "stream tmp blob {}: {err}",
                    tmp.display()
                )));
            }
        };
        let bytes = chunk.as_ref();
        if written.saturating_add(bytes.len() as u64) > limit {
            drop(file);
            let _ = tokio::fs::remove_file(tmp).await;
            return Err(oversized(limit));
        }
        if let Err(err) = file.write_all(bytes).await {
            drop(file);
            let _ = tokio::fs::remove_file(tmp).await;
            return Err(Error::Storage(format!(
                "write tmp blob {}: {err}",
                tmp.display()
            )));
        }
        written += bytes.len() as u64;
    }
    if let Err(err) = file.sync_all().await {
        drop(file);
        let _ = tokio::fs::remove_file(tmp).await;
        return Err(Error::Storage(format!(
            "sync tmp blob {}: {err}",
            tmp.display()
        )));
    }
    drop(file);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_gate_rejects_declared_oversize() {
        assert!(check_content_length(Some(41 * 1024 * 1024), 40 * 1024 * 1024).is_err());
        // Dishonest-small and absent headers pass here; the byte counter
        // catches them mid-body.
        assert!(check_content_length(Some(8), 1024).is_ok());
        assert!(check_content_length(None, 1024).is_ok());
    }

    #[test]
    fn slices_abort_mid_body_and_leave_no_blob() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let chunks: Vec<&[u8]> = vec![b"0123456789", b"0123456789", b"0123456789"];
        let err = stage_slices_capped(root, "remote/1", &chunks, 25).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
        assert_eq!(read_final(root, "remote/1").unwrap(), None);
        assert_eq!(sweep_tmp(root).unwrap(), 0);
    }

    #[test]
    fn exact_cap_fits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let staged = stage_slices_capped(root, "a/b", &[b"0123456789"], 10).unwrap();
        assert_eq!(staged, 10);
        assert_eq!(read_final(root, "a/b").unwrap(), Some(b"0123456789".to_vec()));
    }

    #[test]
    fn rejects_bad_keys() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(stage_bytes_capped(root, "../escape", b"x", 1024).is_err());
        assert!(stage_bytes_capped(root, "", b"x", 1024).is_err());
    }
}

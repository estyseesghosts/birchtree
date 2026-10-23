//! Temp files and blob staging (S12 split of `super`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::application::ports::{BlobStore, Store};
use crate::domain::{Id, MediaState, Result};

// ---------------------------------------------------------------------------
// Temp files and blob staging.
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Filename prefix of transcode temp files (input/probe/output). The
/// orphan sweeper ([`sweep_tmp_files`]) keys on this prefix.
pub const TEMP_PREFIX: &str = "birchtree-transcode-";

/// A fresh temp path that never collides within or across processes.
pub(super) fn temp_path(prefix: &str, ext: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "{TEMP_PREFIX}{prefix}-{}-{n}.{ext}",
        std::process::id()
    ))
}

/// Deletes its paths on drop (best-effort): a crash-safe temp-file guard.
pub(super) struct TempGuard {
    paths: Vec<PathBuf>,
}

impl TempGuard {
    pub(super) fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Deterministic staging key for an attachment's in-flight derived blob:
/// the attachment shard with the `derived` style (`{dir}/derived.{ext}`).
/// The key is stable per attachment, so a crashed attempt's orphan is
/// overwritten by the retry, never accumulated; it is deleted after commit.
pub fn staging_key_for(file_key: &str, ext: &str) -> Option<String> {
    crate::media::public_media::sibling_key(file_key, "derived", ext)
}

/// Maximum `/tmp` orphans removed by one [`sweep_tmp_files`] pass: the sweep
/// is bounded so a crowded temp dir cannot stall startup.
pub const TMP_SWEEP_CAP: usize = 1_000;

/// Minimum age of a `/tmp` file the sweeper removes: a running transcode
/// holds its temp files for seconds at most (wall-time cap 120 s default),
/// so one hour of grace never touches a live job while still converging
/// `SIGKILL` orphans within an operator-tolerable window.
pub const TMP_SWEEP_GRACE_MS: u64 = 3_600_000;

/// Remove stale transcode temp files (`{TEMP_PREFIX}*`) from `dir`.
///
/// A `SIGKILL` between ffmpeg spawn and `TempGuard` drop orphans the input /
/// probe / output files permanently (no guard runs on `SIGKILL`), so the
/// server calls this once at startup and the operator can re-run it via the
/// `sweep-transcode-orphans` CLI. Only files whose mtime is at least
/// `older_than_ms` old are removed, so a concurrently running transcode's
/// fresh files are never touched; entries whose age cannot be read are kept
/// for the next pass, never fatal. At most `cap` files (clamped to
/// [`TMP_SWEEP_CAP`]) are removed per call, so a crowded temp dir cannot
/// stall startup. Returns the number of files removed. Staging blobs inside
/// the blob store are covered by [`sweep_staging_for_media`], not here.
pub fn sweep_tmp_files(dir: &Path, older_than_ms: u64, cap: usize) -> usize {
    let cap = cap.min(TMP_SWEEP_CAP);
    let entries = std::fs::read_dir(dir)
        .map(|read| read.collect::<Vec<_>>())
        .unwrap_or_default();
    let mut removed = 0usize;
    for entry in entries {
        if removed >= cap {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            continue;
        }
        let age_ms: Option<u64> = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .and_then(|age| u64::try_from(age.as_millis()).ok());
        match age_ms {
            Some(age) if age >= older_than_ms => {
                if std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
            _ => continue,
        }
    }
    removed
}

/// Remove one attachment's orphaned staging blobs, if any.
///
/// The derived blob is staged under a deterministic key
/// ([`staging_key_for`]) and deleted best-effort after the `Ready` commit, so
/// a crash between commit and cleanup — or a retry's overwrite that itself
/// crashed — leaves a staging orphan behind a terminal row, where no future
/// job will ever revisit it (retries are no-ops on `Ready`/`Failed` rows).
/// This function deletes both candidate staging keys (`derived.webm`,
/// `derived.webp`; the plan is not stored on the row, so both shapes are
/// probed) when the row is missing, `Ready` or `Failed`, and returns whether
/// anything was removed. `Pending`/`Processing` rows are left alone: the
/// in-flight job (or its retry) owns their staging keys, and a quarantined
/// job's row stays `Pending` until the operator re-drives it.
///
/// Deletion is best-effort per key (a store error aborts with `Err` so the
/// caller can retry; a missing key is simply not counted).
pub fn sweep_staging_for_media(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    media_id: Id,
) -> Result<bool> {
    let media = store.media_by_id(media_id)?;
    let terminal = match &media {
        None => true,
        Some(media) => matches!(media.state, MediaState::Ready | MediaState::Failed),
    };
    if !terminal {
        return Ok(false);
    }
    let file_key = media.map(|media| media.file_key).unwrap_or_default();
    if file_key.is_empty() {
        return Ok(false);
    }
    let mut removed = false;
    for ext in ["webm", "webp"] {
        let Some(key) = staging_key_for(&file_key, ext) else {
            continue;
        };
        match blobs.get(&key)? {
            None => {}
            Some(_) => {
                blobs.delete(&key)?;
                removed = true;
            }
        }
    }
    Ok(removed)
}

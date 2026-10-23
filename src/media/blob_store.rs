//! Filesystem blob store.
//!
//! Keys are relative paths (`ab/cd/0123456789abcdef/original.jpg`, emoji
//! images, `remote/<id>` cache bytes, ...). The root is private (`0700` on
//! Unix; group and other access are *not* supported and are refused or
//! tightened at open time), keys are validated component by component, and
//! no path component below the root may be a symlink, so a crafted key or a
//! planted link can never address bytes outside the media tree.
//!
//! Writes are atomic: bytes go to an exclusively created dot-prefixed
//! sibling, are flushed, and replace the destination with one `rename`. The
//! destination is never removed in response to a failure, so a failed
//! replacement always leaves the previous bytes in place.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::application::ports::BlobStore;
use crate::domain::{Error, Result};

/// Longest accepted blob key (bytes).
const MAX_KEY_LEN: usize = 1024;

/// Longest accepted key component (the common filesystem limit).
const MAX_COMPONENT_LEN: usize = 255;

/// Longest stem of a staging name derived from the destination file name, so
/// `.{stem}.part-{pid}-{seq}` always fits a filesystem name limit.
const MAX_STAGING_STEM_LEN: usize = 120;

/// Top-level directory reserved for in-flight downloads (`fetch_stream`).
/// Never a valid blob key namespace.
const RESERVED_TOP_LEVEL: &str = "tmp";

/// Process-wide counter so staging siblings never collide across threads.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A point in the atomic-write sequence where a failure can be injected.
///
/// Test hook for the acceptance tests: one armed fault fires once, at the
/// first matching point, and then disarms itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageFault {
    /// Before the staging file is created.
    BeforeCreate,
    /// Half-way through writing the bytes (a torn staging file is left to
    /// clean up).
    DuringWrite,
    /// At the flush (`sync_all`) of the staging file.
    AtSync,
    /// At the rename that replaces the destination.
    AtReplace,
}

/// A `BlobStore` rooted at a directory (default: `<data_dir>/media`).
///
/// `Clone` so callers can hold the durable and cache handles side by side;
/// each handle owns its root and never touches the other's tree.
#[derive(Debug, Clone)]
pub struct FsBlobStore {
    root: PathBuf,
    canonical_root: PathBuf,
    fault: Arc<Mutex<Option<StageFault>>>,
}

impl FsBlobStore {
    /// Open (creating) the blob root under `root`, enforcing private
    /// permissions on Unix. Fails when the permissions cannot be
    /// established.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root: PathBuf = root.into();
        create_private_dirs(&root)?;
        enforce_private_dir(&root)?;
        let canonical_root: PathBuf = std::fs::canonicalize(&root).map_err(|err| {
            Error::Storage(format!("resolve blob root {}: {err}", root.display()))
        })?;
        Ok(Self {
            root,
            canonical_root,
            fault: Arc::new(Mutex::new(None)),
        })
    }

    /// Open the durable + cache pair in one call (`docs/database.md` Agent A).
    ///
    /// The durable root keeps the pre-overhaul path (`<data_dir>/media`) so
    /// pre-migration blobs stay readable in place (lazy move); the cache root
    /// is a fresh tree (`<data_dir>/media_cache`). The roots are compared
    /// after resolving symlinks: identical or nested roots (including
    /// aliases through a link) are refused, because cache eviction must
    /// never be able to reach durable bytes.
    pub fn open_pair(
        durable: impl Into<PathBuf>,
        cache: impl Into<PathBuf>,
    ) -> Result<(Self, Self)> {
        let durable: PathBuf = durable.into();
        let cache: PathBuf = cache.into();
        if durable == cache {
            return Err(Error::InvalidInput(
                "durable and cache blob roots must differ".into(),
            ));
        }
        let durable_store: Self = Self::open(durable)?;
        let cache_store: Self = Self::open(cache)?;
        crate::media::storage_roots::ensure_disjoint(
            &durable_store.canonical_root,
            &cache_store.canonical_root,
        )?;
        Ok((durable_store, cache_store))
    }

    /// The root this handle writes under (observability/tests).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root with every symlink resolved.
    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    /// Arm a one-shot failure at `fault` (acceptance-test hook).
    pub fn fail_next(&self, fault: StageFault) {
        if let Ok(mut slot) = self.fault.lock() {
            *slot = Some(fault);
        }
    }

    /// Fire the armed fault if it matches `at`.
    fn check_fault(&self, at: StageFault) -> Result<()> {
        let Ok(mut slot) = self.fault.lock() else {
            return Ok(());
        };
        if *slot == Some(at) {
            *slot = None;
            return Err(Error::Storage(format!("injected blob store fault: {at:?}")));
        }
        Ok(())
    }

    /// Unique staging sibling for `dest`, in the same directory so the final
    /// `rename` is an atomic same-filesystem move. Dot-prefixed so residue
    /// never looks like (or resolves as) a blob.
    fn tmp_sibling(dest: &Path) -> PathBuf {
        let seq: u64 = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let name: String = dest
            .file_name()
            .map(|n| {
                n.to_string_lossy()
                    .chars()
                    .take(MAX_STAGING_STEM_LEN)
                    .collect()
            })
            .unwrap_or_else(|| "blob".to_string());
        dest.with_file_name(format!(".{name}.part-{}-{seq}", std::process::id()))
    }

    /// Exclusively create a fresh staging file next to `dest`.
    fn create_staging_file(dest: &Path) -> Result<(File, PathBuf)> {
        for _ in 0..8 {
            let tmp: PathBuf = Self::tmp_sibling(dest);
            match open_new_private(&tmp) {
                Ok(file) => return Ok((file, tmp)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(Error::Storage(format!(
                        "create tmp blob {}: {err}",
                        tmp.display()
                    )));
                }
            }
        }
        Err(Error::Storage(format!(
            "could not allocate a staging file for {}",
            dest.display()
        )))
    }

    /// Atomic write: stage to an exclusively created sibling, flush it, then
    /// rename over the destination. Any failure removes the staging file and
    /// leaves the previous destination bytes untouched.
    fn put_atomic(&self, dest: &Path, bytes: &[u8]) -> Result<()> {
        self.check_fault(StageFault::BeforeCreate)?;
        let (mut file, tmp) = Self::create_staging_file(dest)?;
        let mut cleanup = RemoveOnDrop::new(tmp.clone());

        let written: std::io::Result<()> = if self.check_fault(StageFault::DuringWrite).is_err() {
            // Torn write: half the bytes land, then the "device" fails.
            let half: usize = bytes.len() / 2;
            file.write_all(&bytes[..half])
                .and_then(|()| Err(std::io::Error::other("injected write failure")))
        } else {
            file.write_all(bytes)
        };
        written
            .map_err(|err| Error::Storage(format!("write tmp blob {}: {err}", tmp.display())))?;

        self.check_fault(StageFault::AtSync)?;
        file.sync_all()
            .map_err(|err| Error::Storage(format!("sync tmp blob {}: {err}", tmp.display())))?;
        drop(file);

        self.publish_staged(&tmp, dest)?;
        cleanup.disarm();
        Ok(())
    }

    /// Replace `dest` with the already-written, already-flushed `tmp`.
    fn publish_staged(&self, tmp: &Path, dest: &Path) -> Result<()> {
        self.check_fault(StageFault::AtReplace)?;
        match replace_file(tmp, dest) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {
                // The staging file lives on another filesystem (the two
                // pools may be separate disks): copy next to the destination
                // and rename from there.
                let (mut staged, sibling) = Self::create_staging_file(dest)?;
                let mut cleanup = RemoveOnDrop::new(sibling.clone());
                let mut source = File::open(tmp).map_err(|err| {
                    Error::Storage(format!("open staged {}: {err}", tmp.display()))
                })?;
                std::io::copy(&mut source, &mut staged)
                    .and_then(|_| staged.sync_all())
                    .map_err(|err| {
                        Error::Storage(format!("copy staged {}: {err}", tmp.display()))
                    })?;
                drop(staged);
                replace_file(&sibling, dest).map_err(|err| {
                    Error::Storage(format!("replace blob {}: {err}", dest.display()))
                })?;
                cleanup.disarm();
                let _ = std::fs::remove_file(tmp);
            }
            Err(err) => {
                return Err(Error::Storage(format!(
                    "replace blob {}: {err}",
                    dest.display()
                )));
            }
        }
        sync_parent_dir(dest).map_err(|err| {
            Error::Storage(format!("sync blob directory of {}: {err}", dest.display()))
        })
    }

    /// Resolve a key to an absolute path: the key shape is validated and no
    /// existing path component below the root may be a symlink.
    fn resolve(&self, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        let path: PathBuf = self.root.join(key);
        self.refuse_links(&path)?;
        Ok(path)
    }

    /// Fail when an existing component between the root and `path` is a link.
    fn refuse_links(&self, path: &Path) -> Result<()> {
        refuse_links_under(&self.root, path)
    }

    /// Open a blob for reading without following links. `None` when missing
    /// or not a regular file.
    pub fn open_read(&self, key: &str) -> Result<Option<File>> {
        open_read_in_root(&self.root, key)
    }

    /// Open a blob for reading plus its length (`None` when missing or not
    /// a regular file). The streaming serve path uses this so a `Range`
    /// seeks instead of buffering the whole blob.
    pub fn open_at(&self, key: &str) -> Result<Option<(File, u64)>> {
        let Some(file) = self.open_read(key)? else {
            return Ok(None);
        };
        let len: u64 = file
            .metadata()
            .map_err(|err| Error::Storage(format!("stat blob {key}: {err}")))?
            .len();
        Ok(Some((file, len)))
    }

    /// Read at most `len` bytes starting at `offset`, plus the total blob
    /// length for `Content-Range` (`None` when missing). Seeks instead of
    /// buffering: serving a 64 KiB range of a 40 MiB remote allocates ~64
    /// KiB, never 40 MiB. Offsets past the end read zero bytes.
    pub fn read_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<(Vec<u8>, u64)>> {
        use std::io::{Read as _, Seek as _};

        let Some((mut file, total)) = self.open_at(key)? else {
            return Ok(None);
        };
        let start: u64 = offset.min(total);
        let count: u64 = total.saturating_sub(start).min(len);
        file.seek(std::io::SeekFrom::Start(start))
            .map_err(|err| Error::Storage(format!("seek blob {key}: {err}")))?;
        let mut bytes: Vec<u8> = Vec::new();
        file.take(count)
            .read_to_end(&mut bytes)
            .map_err(|err| Error::Storage(format!("read blob range {key}: {err}")))?;
        Ok(Some((bytes, total)))
    }

    /// Size in bytes of a stored blob (`None` when missing).
    pub fn size_of(&self, key: &str) -> Result<Option<u64>> {
        let path: PathBuf = self.resolve(key)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => Ok(Some(meta.len())),
            Ok(_) => Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(Error::Storage(format!(
                "stat blob {}: {err}",
                path.display()
            ))),
        }
    }

    /// At most `max_bytes` from the start of a blob (`None` when missing).
    /// Used to sniff a content type without reading the whole file.
    pub fn read_prefix(&self, key: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        use std::io::Read as _;

        let Some(file) = self.open_read(key)? else {
            return Ok(None);
        };
        let mut prefix: Vec<u8> = Vec::new();
        file.take(max_bytes)
            .read_to_end(&mut prefix)
            .map_err(|err| Error::Storage(format!("read blob prefix {key}: {err}")))?;
        Ok(Some(prefix))
    }

    /// One page of stored blob keys in ascending string order, strictly
    /// after `after`. Staging residue (dot-prefixed files, the reserved
    /// `tmp/` directory) is never listed, and files modified within
    /// `min_age` are skipped so a blob that is still being published is not
    /// mistaken for an orphan.
    pub fn list_keys(
        &self,
        after: Option<&str>,
        limit: usize,
        min_age: Duration,
    ) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        let now: SystemTime = SystemTime::now();
        self.walk_sorted(&self.root, "", after, limit, min_age, now, &mut out)?;
        Ok(out)
    }

    /// Depth-first walk in key order (directories sort as `name/`, so the
    /// traversal order equals the plain string order of the full keys).
    fn walk_sorted(
        &self,
        dir: &Path,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        min_age: Duration,
        now: SystemTime,
        out: &mut Vec<String>,
    ) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(Error::Storage(format!(
                    "list blob dir {}: {err}",
                    dir.display()
                )));
            }
        };
        let mut sorted: Vec<(String, bool, std::fs::Metadata)> = Vec::new();
        for entry in entries.flatten() {
            let name: String = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || (prefix.is_empty() && name == RESERVED_TOP_LEVEL) {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if is_link(&meta) {
                continue;
            }
            let is_dir: bool = meta.is_dir();
            let sort_key: String = if is_dir { format!("{name}/") } else { name };
            sorted.push((sort_key, is_dir, meta));
        }
        sorted.sort_by(|a, b| a.0.cmp(&b.0));

        for (sort_key, is_dir, meta) in sorted {
            if out.len() >= limit {
                return Ok(());
            }
            let full: String = format!("{prefix}{sort_key}");
            if is_dir {
                // Every key below `full` starts with `full`; skip the subtree
                // when the cursor is already past all of them.
                if let Some(cursor) = after
                    && !cursor.starts_with(&full)
                    && full.as_str() <= cursor
                {
                    continue;
                }
                let child: PathBuf = dir.join(sort_key.trim_end_matches('/'));
                self.walk_sorted(&child, &full, after, limit, min_age, now, out)?;
                continue;
            }
            if after.is_some_and(|cursor| full.as_str() <= cursor) {
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let age: Duration = meta
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .unwrap_or(Duration::ZERO);
            if age < min_age {
                continue;
            }
            out.push(full);
        }
        Ok(())
    }
}

/// Fail when an existing component between `root` and `path` is a link.
/// The root itself may be a link (operators may point it at another disk).
fn refuse_links_under(root: &Path, path: &Path) -> Result<()> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Err(Error::InvalidInput("blob path escapes the root".into()));
    };
    let mut current: PathBuf = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if is_link(&meta) => {
                return Err(Error::InvalidInput(format!(
                    "blob path crosses a link: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(Error::Storage(format!(
                    "inspect blob path {}: {err}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

/// Open the regular file for `key` under an existing `root` for reading,
/// refusing invalid keys and links. `None` when missing. Used by the public
/// file route, which only holds a root path.
pub fn open_read_in_root(root: &Path, key: &str) -> Result<Option<File>> {
    validate_key(key)?;
    let path: PathBuf = root.join(key);
    refuse_links_under(root, &path)?;
    match File::open(&path) {
        Ok(file) => {
            let meta = file
                .metadata()
                .map_err(|err| Error::Storage(format!("stat blob {}: {err}", path.display())))?;
            Ok(meta.is_file().then_some(file))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::Storage(format!(
            "open blob {}: {err}",
            path.display()
        ))),
    }
}

/// Deletes a path on drop unless disarmed (staging-file cleanup).
struct RemoveOnDrop {
    path: PathBuf,
    armed: bool,
}

impl RemoveOnDrop {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Validate a blob key: relative, `/`-separated, every component a plain
/// name. Dot-prefixed components are reserved for staging residue, `tmp` is
/// reserved for in-flight downloads, and separators or stream syntax of
/// other platforms (`\`, and `:` on Windows) are refused.
pub fn validate_key(key: &str) -> Result<()> {
    let invalid = || Error::InvalidInput(format!("invalid blob key: {key}"));
    if key.is_empty() {
        return Err(Error::InvalidInput("empty blob key".into()));
    }
    if key.len() > MAX_KEY_LEN || key.contains(['\0', '\\']) {
        return Err(invalid());
    }
    if cfg!(windows) && key.contains(':') {
        return Err(invalid());
    }
    for (index, component) in key.split('/').enumerate() {
        if component.is_empty()
            || component.len() > MAX_COMPONENT_LEN
            || component.starts_with('.')
            || component.ends_with(' ')
            || (index == 0 && component == RESERVED_TOP_LEVEL)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Replace `dest` with `tmp` in one rename. The destination is never removed
/// first: on failure the previous bytes are still there. (`std::fs::rename`
/// replaces an existing file on every supported platform, using
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` on Windows.)
pub(crate) fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, dest)
}

/// Flush the directory entry of `path` so a completed rename survives power
/// loss. A no-op where directories cannot be opened for flushing.
pub(crate) fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        if let Some(parent) = path.parent() {
            return File::open(parent)?.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Exclusively create a new file readable only by the owner.
pub(crate) fn open_new_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Create `dir` and missing parents, owner-only on Unix.
pub(crate) fn create_private_dirs(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|err| Error::Storage(format!("create blob dir {}: {err}", dir.display())))
}

/// Ensure `dir` is private to the owner (Unix: no group/other bits). A
/// looser existing mode is tightened to `0700`; a directory that cannot be
/// made private is an error. Other platforms inherit ACLs from the parent.
pub fn enforce_private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let read_mode = |path: &Path| -> Result<u32> {
            std::fs::metadata(path)
                .map(|meta| meta.permissions().mode() & 0o777)
                .map_err(|err| Error::Storage(format!("stat {}: {err}", path.display())))
        };
        if read_mode(dir)? & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(
                |err| {
                    Error::Storage(format!(
                        "{} is not private and could not be restricted to 0700: {err}",
                        dir.display()
                    ))
                },
            )?;
            let mode: u32 = read_mode(dir)?;
            if mode & 0o077 != 0 {
                return Err(Error::Storage(format!(
                    "{} still has group/other access (mode {mode:o})",
                    dir.display()
                )));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Whether `meta` describes a symlink or (on Windows) any reparse point
/// such as a junction.
fn is_link(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

impl BlobStore for FsBlobStore {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path: PathBuf = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            create_private_dirs(parent)?;
            // Re-check after creation: the parents now exist.
            self.refuse_links(&path)?;
        }
        self.put_atomic(&path, bytes)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        use std::io::Read as _;

        let Some(mut file) = self.open_read(key)? else {
            return Ok(None);
        };
        let mut bytes: Vec<u8> = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|err| Error::Storage(format!("read blob {key}: {err}")))?;
        Ok(Some(bytes))
    }

    fn delete(&self, key: &str) -> Result<()> {
        if key.is_empty() {
            return Ok(());
        }
        let path: PathBuf = self.resolve(key)?;
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
        Ok(self.size_of(key)?.is_some())
    }

    fn root_path(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    /// Single-pool staging uses the same root, so `publish_tmp` is still a
    /// same-volume rename (adversarial-2 F11).
    fn staging_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn publish_tmp(&self, tmp: &Path, key: &str) -> Result<()> {
        let dest: PathBuf = self.resolve(key)?;
        if let Some(parent) = dest.parent() {
            create_private_dirs(parent)?;
            self.refuse_links(&dest)?;
        }
        let result: Result<()> = self.publish_staged(tmp, &dest);
        if result.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }

    fn put_streamed(&self, key: &str, bytes: &[u8], max_bytes: u64) -> Result<()> {
        if bytes.len() as u64 > max_bytes {
            return Err(Error::InvalidInput(format!(
                "stream body exceeds {max_bytes} bytes"
            )));
        }
        self.put(key, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> (tempfile::TempDir, FsBlobStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsBlobStore::open(dir.path().join("media")).expect("open");
        (dir, store)
    }

    #[test]
    fn round_trips_and_rejects_traversal() {
        let (_dir, store) = open_store();
        store.put("000/000/001/original/a.jpg", b"bytes").unwrap();
        assert_eq!(
            store.get("000/000/001/original/a.jpg").unwrap(),
            Some(b"bytes".to_vec())
        );
        assert!(store.exists("000/000/001/original/a.jpg").unwrap());
        store.delete("000/000/001/original/a.jpg").unwrap();
        assert_eq!(store.get("000/000/001/original/a.jpg").unwrap(), None);
        assert!(store.get("../escape").is_err());
        assert!(store.put("/etc/passwd", b"x").is_err());
        assert!(store.get("").is_err());
    }

    #[test]
    fn key_validation_rejects_hostile_shapes() {
        for key in [
            "a//b",
            "a/./b",
            "a/../b",
            "a\\b",
            "a/b\0c",
            ".hidden",
            "a/.b.part-1-2",
            "tmp/1.part",
            "a/b ",
            "a/",
        ] {
            assert!(validate_key(key).is_err(), "should reject {key:?}");
        }
        assert!(validate_key("ab/cd/0123456789abcdef/original.jpg").is_ok());
        assert!(validate_key("remote/42").is_ok());
        assert!(validate_key("custom_emojis/images/6b/65/key@host.example/a.png").is_ok());
    }

    #[test]
    fn range_reads_seek_without_buffering_the_blob() {
        let (_dir, store) = open_store();
        let bytes: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        store.put("remote/42", &bytes).unwrap();
        let (head, total) = store.read_range("remote/42", 0, 64).unwrap().unwrap();
        assert_eq!(total, 1024);
        assert_eq!(head, bytes[..64]);
        let (mid, total) = store.read_range("remote/42", 100, 10).unwrap().unwrap();
        assert_eq!(total, 1024);
        assert_eq!(mid, bytes[100..110]);
        // Clamped at the end, and past-the-end reads zero bytes.
        let (tail, _) = store.read_range("remote/42", 1000, 4096).unwrap().unwrap();
        assert_eq!(tail, bytes[1000..]);
        let (empty, total) = store.read_range("remote/42", 2048, 64).unwrap().unwrap();
        assert!(empty.is_empty());
        assert_eq!(total, 1024);
        assert!(store.read_range("remote/missing", 0, 64).unwrap().is_none());
        assert!(store.read_range("../escape", 0, 64).is_err());
    }

    #[test]
    fn pair_rejects_shared_and_nested_roots() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        assert!(FsBlobStore::open_pair(&media, &media).is_err());
        assert!(FsBlobStore::open_pair(&media, media.join("cache")).is_err());
        assert!(FsBlobStore::open_pair(media.join("cache"), &media).is_err());
        assert!(FsBlobStore::open_pair(dir.path().join("a"), dir.path().join("b")).is_ok());
    }

    #[test]
    fn listing_is_ordered_resumable_and_skips_residue() {
        let (_dir, store) = open_store();
        for key in ["b/2", "a.b", "a/x", "a/y", "c"] {
            store.put(key, b"v").unwrap();
        }
        std::fs::create_dir_all(store.root().join("tmp")).unwrap();
        std::fs::write(store.root().join("tmp").join("1.part"), b"x").unwrap();
        std::fs::write(store.root().join("a").join(".y.part-1-1"), b"x").unwrap();

        let all = store.list_keys(None, 100, Duration::ZERO).unwrap();
        assert_eq!(all, vec!["a.b", "a/x", "a/y", "b/2", "c"]);

        let mut collected: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store
                .list_keys(cursor.as_deref(), 2, Duration::ZERO)
                .unwrap();
            let Some(last) = page.last().cloned() else {
                break;
            };
            collected.extend(page);
            cursor = Some(last);
        }
        assert_eq!(collected, all);
        assert!(
            store
                .list_keys(None, 100, Duration::from_secs(3600))
                .unwrap()
                .is_empty(),
            "fresh files are younger than the minimum age"
        );
    }
}

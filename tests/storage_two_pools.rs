//! Agent A — two blob pools (`docs/database.md`).
//!
//! Proves the storage-overhaul pool split at the `FsBlobStore` layer:
//! (a) a durable write never lands under the cache root and vice versa,
//! (b) a pre-migration blob already sitting under the old shared `media/`
//! path still `get()`s once the durable handle opens that same path
//! (lazy-move readability — no flag day), and
//! (c) tmp+rename staging leaves no partial blob visible when a write fails.

use std::path::{Path, PathBuf};

use birchtree::application::ports::BlobStore;
use birchtree::media::blob_store::FsBlobStore;

fn open_pair_under(base: &Path) -> (FsBlobStore, FsBlobStore) {
    FsBlobStore::open_pair(base.join("media"), base.join("media_cache"))
        .expect("open durable + cache pair")
}

/// Every regular file under `root`, recursively.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// No tmp-staging residue: tmp siblings are dot-prefixed `.name.part-*` and
/// must never be visible as blobs after success or failure.
fn tmp_debris(root: &Path) -> Vec<PathBuf> {
    walk_files(root)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(".part-"))
        })
        .collect()
}

#[test]
fn durable_and_cache_roots_are_isolated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (durable, cache) = open_pair_under(dir.path());

    durable
        .put("000/000/042/original/local.jpg", b"durable-bytes")
        .expect("durable put");
    cache
        .put("remote/9/preview.webp", b"cache-bytes")
        .expect("cache put");

    // Same-layout keys coexist with independent contents.
    durable.put("shared/key.bin", b"D").expect("durable shared");
    cache.put("shared/key.bin", b"C").expect("cache shared");

    // Cross-visibility: neither handle sees the other's tree.
    assert_eq!(durable.get("remote/9/preview.webp").expect("get"), None);
    assert_eq!(
        cache.get("000/000/042/original/local.jpg").expect("get"),
        None
    );
    assert_eq!(
        durable.get("shared/key.bin").expect("get"),
        Some(b"D".to_vec())
    );
    assert_eq!(
        cache.get("shared/key.bin").expect("get"),
        Some(b"C".to_vec())
    );

    // On-disk: bytes live under exactly one root each.
    assert!(dir
        .path()
        .join("media/000/000/042/original/local.jpg")
        .exists());
    assert!(!dir
        .path()
        .join("media_cache/000/000/042/original/local.jpg")
        .exists());
    assert!(dir.path().join("media_cache/remote/9/preview.webp").exists());
    assert!(!dir.path().join("media/remote/9/preview.webp").exists());

    // Both roots were created 0700 on unix (traversal rejection relies on the
    // root layout; perms are defense in depth).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for root in ["media", "media_cache"] {
            let mode = std::fs::metadata(dir.path().join(root))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "root {root} perms");
        }
    }

    // No staging residue after successful writes.
    assert!(tmp_debris(&dir.path().join("media")).is_empty());
    assert!(tmp_debris(&dir.path().join("media_cache")).is_empty());
}

#[test]
fn pre_migration_blob_stays_readable() {
    // Simulate the pre-overhaul layout: bytes were staged directly under the
    // old shared `<data_dir>/media` root by the previous single-store wiring.
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy_key = "remote/000/000/007/original/legacy.jpg";
    let legacy_path = dir.path().join("media").join(legacy_key);
    std::fs::create_dir_all(legacy_path.parent().expect("parent")).expect("mkdirs");
    std::fs::write(&legacy_path, b"legacy-bytes").expect("seed legacy blob");

    // The durable handle opens that same path (the default
    // `durable_dir_resolved()`), so the row's bytes stay readable with no
    // migration step; the reconciler copies + repoints later.
    let (durable, cache) = open_pair_under(dir.path());
    assert_eq!(
        durable.get(legacy_key).expect("durable get"),
        Some(b"legacy-bytes".to_vec())
    );
    // ...while the fresh cache tree knows nothing about it.
    assert_eq!(cache.get(legacy_key).expect("cache get"), None);
}

#[test]
fn failed_write_leaves_no_partial_blob() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (durable, _cache) = open_pair_under(dir.path());
    let root = dir.path().join("media");

    // (i) Rejected keys write nothing and stage nothing.
    assert!(durable.put("../escape.bin", b"x").is_err());
    assert!(durable.put("/etc/passwd", b"x").is_err());
    assert!(durable.put("", b"x").is_err());
    assert!(durable.get("../escape.bin").is_err());
    assert!(tmp_debris(&root).is_empty());
    assert!(!root.join("escape.bin").exists());

    // (ii) A mid-write failure: block the parent directory by squatting a
    // regular file where the key's parent dir must go, so staging fails.
    std::fs::write(root.join("blocked"), b"squat").expect("squat");
    assert!(durable.put("blocked/child.bin", b"data").is_err());
    // No partial blob at the key, no tmp residue anywhere under the root.
    assert!(!root.join("blocked/child.bin").exists());
    assert!(tmp_debris(&root).is_empty());

    // (iii) The store is healthy afterwards: later writes land atomically and
    // overwrites replace whole content (never a torn prefix).
    durable.put("ok/blob.bin", b"v1").expect("put v1");
    durable.put("ok/blob.bin", b"v2-longer-payload").expect("put v2");
    assert_eq!(
        durable.get("ok/blob.bin").expect("get"),
        Some(b"v2-longer-payload".to_vec())
    );
    assert!(tmp_debris(&root).is_empty());
}

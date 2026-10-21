//! Work package 2: blob-store correctness on the real file system.
//!
//! - fault injection before create, during write, at sync and at the
//!   replace rename: the previous bytes always survive and no staging file
//!   remains (2.1);
//! - private permissions are established or startup fails (2.2, Unix);
//! - keys cannot reach staging files, other roots or links (2.2/2.3);
//! - overlapping or aliased roots are refused (WP6.7);
//! - an uncommitted staging guard leaves nothing behind (2.4).

use std::path::{Path, PathBuf};

use birchtree::application::ports::BlobStore;
use birchtree::media::blob_store::{FsBlobStore, StageFault};
use birchtree::media::staging::StagingGuard;

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
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

fn open(base: &Path) -> FsBlobStore {
    FsBlobStore::open(base.join("media")).expect("open blob store")
}

#[test]
fn failed_replacement_always_keeps_the_previous_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(dir.path());
    let key = "ab/cd/0123456789abcdef/original.png";
    store.put(key, b"original-bytes").expect("seed");

    for fault in [
        StageFault::BeforeCreate,
        StageFault::DuringWrite,
        StageFault::AtSync,
        StageFault::AtReplace,
    ] {
        store.fail_next(fault);
        let result = store.put(key, b"replacement-bytes-that-are-longer");
        assert!(result.is_err(), "{fault:?} must fail the put");
        assert_eq!(
            store.get(key).expect("read after failure"),
            Some(b"original-bytes".to_vec()),
            "{fault:?}: previous bytes must survive"
        );
        let residue: Vec<_> = walk_files(store.root())
            .into_iter()
            .filter(|path| path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
            .collect();
        assert!(residue.is_empty(), "{fault:?}: staging residue {residue:?}");
    }

    // The next write succeeds and replaces whole content, never a prefix.
    store.put(key, b"replacement-bytes-that-are-longer").expect("healthy put");
    assert_eq!(
        store.get(key).unwrap(),
        Some(b"replacement-bytes-that-are-longer".to_vec())
    );
}

#[test]
fn failed_first_write_leaves_no_blob_at_all() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(dir.path());
    for fault in [
        StageFault::BeforeCreate,
        StageFault::DuringWrite,
        StageFault::AtSync,
        StageFault::AtReplace,
    ] {
        store.fail_next(fault);
        assert!(store.put("fresh/key.bin", b"data").is_err());
        assert_eq!(store.get("fresh/key.bin").unwrap(), None, "{fault:?}");
        assert!(!store.exists("fresh/key.bin").unwrap());
        assert!(
            walk_files(store.root()).is_empty(),
            "{fault:?}: nothing may remain on disk"
        );
    }
}

#[test]
fn replace_never_exposes_a_partial_file_to_concurrent_readers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(dir.path());
    let key = "ab/cd/0123456789abcdef/original.bin";
    let old = vec![b'o'; 256 * 1024];
    let new = vec![b'n'; 512 * 1024];
    store.put(key, &old).unwrap();

    let reader = {
        let store = store.clone();
        let (old, new) = (old.clone(), new.clone());
        std::thread::spawn(move || {
            for _ in 0..200 {
                let seen = store.get(key).unwrap().expect("blob always present");
                assert!(seen == old || seen == new, "torn read of {} bytes", seen.len());
            }
        })
    };
    for round in 0..20 {
        let bytes = if round % 2 == 0 { &new } else { &old };
        store.put(key, bytes).unwrap();
    }
    reader.join().expect("reader saw only whole files");
}

#[test]
fn keys_cannot_name_staging_files_or_escape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(dir.path());
    store.put("ab/cd/0123456789abcdef/original.png", b"x").unwrap();
    // A crashed writer's residue and a fetch spool, as they appear on disk.
    std::fs::write(
        store.root().join("ab/cd/0123456789abcdef/.original.png.part-1-2"),
        b"partial",
    )
    .unwrap();
    std::fs::create_dir_all(store.root().join("tmp")).unwrap();
    std::fs::write(store.root().join("tmp/1-2-3.part"), b"partial").unwrap();

    for key in [
        "ab/cd/0123456789abcdef/.original.png.part-1-2",
        "tmp/1-2-3.part",
        "../outside",
        "ab/../../outside",
        "/absolute",
        "ab\\cd\\x",
        "ab//cd",
        ".hidden",
        "",
    ] {
        assert!(store.get(key).is_err(), "get {key:?}");
        assert!(store.put(key, b"x").is_err(), "put {key:?}");
        assert!(store.exists(key).map(|found| !found).unwrap_or(true), "exists {key:?}");
    }
    assert!(!dir.path().join("outside").exists());
}

#[cfg(unix)]
#[test]
fn symlinks_below_the_root_are_not_followed() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(dir.path());
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();

    // A directory link inside the tree and a file link inside the tree.
    symlink(&outside, store.root().join("linkdir")).unwrap();
    std::fs::create_dir_all(store.root().join("real")).unwrap();
    symlink(outside.join("secret.txt"), store.root().join("real/file.txt")).unwrap();

    assert!(store.get("linkdir/secret.txt").is_err());
    assert!(store.put("linkdir/new.txt", b"x").is_err());
    assert!(!outside.join("new.txt").exists());
    assert!(store.get("real/file.txt").is_err());
    assert!(
        birchtree::media::blob_store::open_read_in_root(store.root(), "real/file.txt").is_err()
    );
}

#[cfg(unix)]
#[test]
fn roots_and_files_are_private_and_tightened() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let loose = dir.path().join("media");
    std::fs::create_dir(&loose).unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).unwrap();

    let store = FsBlobStore::open(&loose).expect("open tightens an owned root");
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(store.root()), 0o700);

    store.put("ab/cd/0123456789abcdef/original.png", b"x").unwrap();
    assert_eq!(mode(&store.root().join("ab")), 0o700, "created directories are private");
    assert_eq!(
        mode(&store.root().join("ab/cd/0123456789abcdef/original.png")) & 0o077,
        0,
        "blob files are owner-only"
    );
}

#[test]
fn identical_nested_and_aliased_roots_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let media = dir.path().join("media");
    assert!(FsBlobStore::open_pair(&media, &media).is_err(), "identical");
    assert!(FsBlobStore::open_pair(&media, media.join("cache")).is_err(), "cache inside durable");
    assert!(FsBlobStore::open_pair(media.join("durable"), &media).is_err(), "durable inside cache");
    // Spelling differences that resolve to the same directory.
    let dotted = dir.path().join("media").join(".").join("..").join("media");
    assert!(FsBlobStore::open_pair(&media, dotted).is_err(), "dot-dot alias");
    assert!(
        FsBlobStore::open_pair(dir.path().join("a"), dir.path().join("b")).is_ok(),
        "separate roots are fine"
    );
}

#[cfg(unix)]
#[test]
fn symlink_alias_between_roots_is_refused() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let durable = dir.path().join("media");
    std::fs::create_dir_all(&durable).unwrap();
    let alias = dir.path().join("cache-alias");
    symlink(&durable, &alias).unwrap();
    assert!(FsBlobStore::open_pair(&durable, &alias).is_err());

    // A cache that is a link into a *subdirectory* of the durable tree.
    let inner = durable.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let alias_inner = dir.path().join("cache-inner");
    symlink(&inner, &alias_inner).unwrap();
    assert!(FsBlobStore::open_pair(&durable, &alias_inner).is_err());
}

#[test]
fn uncommitted_staging_guard_removes_everything_it_staged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(dir.path());
    {
        let mut guard = StagingGuard::new(&store);
        guard.put("ab/cd/0123456789abcdef/original.png", b"original").unwrap();
        guard.put("ab/cd/0123456789abcdef/thumbnail.png", b"thumb").unwrap();
        // The metadata commit "fails": the guard is dropped uncommitted.
    }
    assert!(walk_files(store.root()).is_empty(), "no orphan after a failed commit");

    // A failure part-way through staging cleans the earlier writes too.
    {
        let mut guard = StagingGuard::new(&store);
        guard.put("ab/cd/0123456789abcdef/original.png", b"original").unwrap();
        store.fail_next(StageFault::AtReplace);
        assert!(guard.put("ab/cd/0123456789abcdef/thumbnail.png", b"thumb").is_err());
    }
    assert!(walk_files(store.root()).is_empty(), "no orphan after a failed second put");
}

#[test]
fn listing_is_resumable_and_skips_staging_residue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(dir.path());
    let mut expected: Vec<String> = Vec::new();
    for shard in 0..7 {
        for file in 0..5 {
            let key = format!("{shard:02x}/aa/{file}.bin");
            store.put(&key, b"v").unwrap();
            expected.push(key);
        }
    }
    std::fs::write(store.root().join("00/aa/.0.bin.part-1-1"), b"x").unwrap();
    std::fs::create_dir_all(store.root().join("tmp")).unwrap();
    std::fs::write(store.root().join("tmp/9.part"), b"x").unwrap();
    expected.sort();

    let mut collected: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = store
            .list_keys(cursor.as_deref(), 4, std::time::Duration::ZERO)
            .unwrap();
        let Some(last) = page.last().cloned() else { break };
        collected.extend(page);
        cursor = Some(last);
    }
    assert_eq!(collected, expected);
}

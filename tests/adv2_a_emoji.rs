//! Workstream A (admission + fetch path): remote-emoji admission tests.
//!
//! - A2: `store_remote_emoji` is admission-controlled. Over the avatar/emoji
//!   cap, or with persistence disabled, it writes nothing. A failed second
//!   write leaves no blob, no row and no reservation.
//! - Guard: the honest path still produces both keys, both durable rows,
//!   accounting of exactly `2 * size`, and the render rows.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use birchtree::application::ports::{BlobStore, CacheStore, EmojiStore};
use birchtree::config::{RemoteCacheConfig, RemoteCacheMode};
use birchtree::domain::{Error, Result};
use birchtree::federation::media::store_remote_emoji;
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::remote_cache::{CacheLedger, MediaClass};
use birchtree::persistence::redb::RedbStore;

use common::temp_store;

mod common;

const T0: u64 = 1_700_000_000_000;

fn test_caps(global: u64, avatar: u64, preview: u64, full: u64) -> RemoteCacheConfig {
    RemoteCacheConfig {
        mode: RemoteCacheMode::OnDemand,
        global_cap_bytes: global,
        avatar_emoji_cap_bytes: avatar,
        preview_cap_bytes: preview,
        full_cap_bytes: full,
        avatar_retention_days: 30,
        warming: false,
    }
}

/// A valid 2x1 PNG in one flat colour.
fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        1,
        image::Rgba([10, 20, 30, 255]),
    ));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("png");
    cursor.into_inner()
}

/// A blob handle that fails the second `put` (the failed static copy).
struct FailSecondPut {
    inner: FsBlobStore,
    puts: AtomicUsize,
}

impl FailSecondPut {
    fn new(inner: FsBlobStore) -> Self {
        Self {
            inner,
            puts: AtomicUsize::new(0),
        }
    }
}

impl BlobStore for FailSecondPut {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        if self.puts.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(Error::Storage("injected second-write failure".into()));
        }
        self.inner.put(key, bytes)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.inner.get(key)
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.inner.delete(key)
    }

    fn exists(&self, key: &str) -> Result<bool> {
        self.inner.exists(key)
    }

    fn root_path(&self) -> Option<std::path::PathBuf> {
        self.inner.root_path()
    }
}

fn image_and_static(domain: &str, shortcode: &str) -> (String, String) {
    birchtree::media::remote_cache::remote_emoji_keys(domain, shortcode).expect("keys")
}

/// A2: an emoji over the avatar/emoji cap is refused with no blob left.
#[test]
fn emoji_over_avatar_cap_is_refused_with_no_blob_left() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
    let png = png_bytes();
    // The avatar cap cannot even hold one copy, let alone two.
    let cfg = test_caps(1024 * 1024, 16, 512 * 1024, 256 * 1024);
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));

    let err = store_remote_emoji(
        &*store,
        &blobs,
        &*store,
        &ledger,
        "party@remote.example",
        "remote.example",
        "https://remote.example/e.png",
        &png,
        T0,
        &cfg,
    )
    .expect_err("over-cap emoji is refused");

    assert!(!err.is_empty());
    let (image, static_key) = image_and_static("remote.example", "party");
    assert_eq!(blobs.get(&image).unwrap(), None);
    assert_eq!(blobs.get(&static_key).unwrap(), None);
}

/// A2: disabled mode writes nothing.
#[test]
fn emoji_in_disabled_mode_writes_nothing() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
    let png = png_bytes();
    let cfg = RemoteCacheConfig {
        mode: RemoteCacheMode::Disabled,
        ..RemoteCacheConfig::default()
    };
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));

    let err = store_remote_emoji(
        &*store,
        &blobs,
        &*store,
        &ledger,
        "party@remote.example",
        "remote.example",
        "https://remote.example/e.png",
        &png,
        T0,
        &cfg,
    )
    .expect_err("disabled mode refuses");

    assert!(!err.is_empty());
    let (image, static_key) = image_and_static("remote.example", "party");
    assert_eq!(blobs.get(&image).unwrap(), None);
    assert_eq!(blobs.get(&static_key).unwrap(), None);
    assert_eq!(
        store
            .cache_get("https://remote.example/e.png", "emoji")
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .cache_get("https://remote.example/e.png", "emoji_static")
            .unwrap(),
        None
    );
}

/// A2: failure on the second write leaves no blob, no row, no reservation.
#[test]
fn emoji_second_write_failure_leaves_nothing_behind() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().unwrap();
    let blobs = FailSecondPut::new(FsBlobStore::open(dir.path().join("media")).expect("open"));
    let png = png_bytes();
    let cfg = RemoteCacheConfig::default();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));

    store_remote_emoji(
        &*store,
        &blobs,
        &*store,
        &ledger,
        "party@remote.example",
        "remote.example",
        "https://remote.example/e.png",
        &png,
        T0,
        &cfg,
    )
    .expect_err("second write fails");

    // The first copy is rolled back: no blob under either key.
    let (image, static_key) = image_and_static("remote.example", "party");
    assert_eq!(blobs.get(&image).unwrap(), None, "first copy removed");
    assert_eq!(blobs.get(&static_key).unwrap(), None);
    // No durable row claims bytes ...
    assert_eq!(
        store
            .cache_get("https://remote.example/e.png", "emoji")
            .unwrap()
            .map(|row| row.has_bytes),
        None
    );
    assert_eq!(
        store
            .cache_get("https://remote.example/e.png", "emoji_static")
            .unwrap()
            .map(|row| row.has_bytes),
        None
    );
    // ... and the reservation is released.
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_for(MediaClass::AvatarEmoji), 0);
}

/// A `CacheStore` facet that fails `cache_mark_stored` after a budget of
/// successful calls, delegating everything else to the real store.
struct FaultCache {
    inner: Arc<RedbStore>,
    marks: AtomicUsize,
    succeed_first: usize,
}

impl FaultCache {
    fn new(inner: &Arc<RedbStore>, succeed_first: usize) -> Self {
        Self {
            inner: inner.clone(),
            marks: AtomicUsize::new(0),
            succeed_first,
        }
    }
}

impl CacheStore for FaultCache {
    fn cache_mark_stored(
        &self,
        source_url: &str,
        variant: &str,
        blob_key: &str,
        size: u64,
        etag: Option<String>,
        now_ms: u64,
        cfg: &RemoteCacheConfig,
    ) -> birchtree::domain::Result<()> {
        let n = self.marks.fetch_add(1, Ordering::SeqCst);
        if n >= self.succeed_first {
            return Err(Error::Storage("injected cache_mark_stored failure".into()));
        }
        self.inner
            .cache_mark_stored(source_url, variant, blob_key, size, etag, now_ms, cfg)
    }

    fn cache_get(
        &self,
        source_url: &str,
        variant: &str,
    ) -> birchtree::domain::Result<Option<birchtree::media::remote_cache::RemoteCacheEntry>> {
        self.inner.cache_get(source_url, variant)
    }

    fn cache_put_metadata(
        &self,
        entry: birchtree::media::remote_cache::RemoteCacheEntry,
    ) -> birchtree::domain::Result<()> {
        self.inner.cache_put_metadata(entry)
    }

    fn cache_mark_evicted(
        &self,
        source_url: &str,
        variant: &str,
    ) -> birchtree::domain::Result<Option<String>> {
        self.inner.cache_mark_evicted(source_url, variant)
    }

    fn cache_touch(
        &self,
        source_url: &str,
        variant: &str,
        now_ms: u64,
    ) -> birchtree::domain::Result<()> {
        self.inner.cache_touch(source_url, variant, now_ms)
    }

    fn cache_accounting(
        &self,
    ) -> birchtree::domain::Result<birchtree::media::remote_cache::CacheUsage> {
        self.inner.cache_accounting()
    }

    fn cache_evict_batch(
        &self,
        need: MediaClass,
        bytes_to_free: u64,
        now_ms: u64,
        cap: usize,
    ) -> birchtree::domain::Result<Vec<String>> {
        self.inner
            .cache_evict_batch(need, bytes_to_free, now_ms, cap)
    }
}

/// A blob handle that refuses every delete (records the attempt).
struct RefuseDelete {
    inner: FsBlobStore,
    deletes: AtomicUsize,
}

impl RefuseDelete {
    fn new(inner: FsBlobStore) -> Self {
        Self {
            inner,
            deletes: AtomicUsize::new(0),
        }
    }
}

impl BlobStore for RefuseDelete {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        self.inner.put(key, bytes)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.inner.get(key)
    }

    fn delete(&self, _key: &str) -> Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        Err(Error::Storage("injected blob deletion failure".into()))
    }

    fn exists(&self, key: &str) -> Result<bool> {
        self.inner.exists(key)
    }

    fn root_path(&self) -> Option<std::path::PathBuf> {
        self.inner.root_path()
    }
}

/// A2: failure of the second durable write rolls back blobs, the first row
/// and the reservation.
#[test]
fn emoji_second_durable_write_failure_rolls_back() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
    let png = png_bytes();
    let cfg = RemoteCacheConfig::default();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    // One durable write succeeds, the second fails.
    let cache = FaultCache::new(&store, 1);

    store_remote_emoji(
        &*store,
        &blobs,
        &cache,
        &ledger,
        "party@remote.example",
        "remote.example",
        "https://remote.example/e.png",
        &png,
        T0,
        &cfg,
    )
    .expect_err("second durable write fails");

    let (image, static_key) = image_and_static("remote.example", "party");
    assert_eq!(blobs.get(&image).unwrap(), None);
    assert_eq!(blobs.get(&static_key).unwrap(), None);
    // The first row is unwound back to a shell; the second was never written.
    assert_eq!(
        store
            .cache_get("https://remote.example/e.png", "emoji")
            .unwrap()
            .map(|row| row.has_bytes),
        Some(false),
        "first row evicted back to a shell"
    );
    assert_eq!(
        store
            .cache_get("https://remote.example/e.png", "emoji_static")
            .unwrap()
            .map(|row| row.has_bytes),
        None,
        "second row never written"
    );
    assert_eq!(store.cache_accounting().unwrap().stored_avatar_emoji, 0);
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_for(MediaClass::AvatarEmoji), 0);
}

/// A2: when blob deletion itself fails, the rollback still frees the
/// reservation and unclaims every durable row. The surviving blob is
/// reconciler residue (documented crash-window contract), never counted
/// bytes.
#[test]
fn emoji_rollback_survives_blob_deletion_failure() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().unwrap();
    let png = png_bytes();
    let cfg = RemoteCacheConfig::default();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let source = "https://remote.example/e2.png";

    // The static copy never lands; unwinding the image copy hits the
    // delete refusal.
    struct FailStatic {
        inner: RefuseDelete,
        puts: AtomicUsize,
    }
    impl BlobStore for FailStatic {
        fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
            if self.puts.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(Error::Storage("injected second-write failure".into()));
            }
            self.inner.put(key, bytes)
        }

        fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
            self.inner.get(key)
        }

        fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key)
        }

        fn exists(&self, key: &str) -> Result<bool> {
            self.inner.exists(key)
        }

        fn root_path(&self) -> Option<std::path::PathBuf> {
            self.inner.root_path()
        }
    }
    let failing = FailStatic {
        inner: RefuseDelete::new(FsBlobStore::open(dir.path().join("media")).expect("open")),
        puts: AtomicUsize::new(0),
    };

    store_remote_emoji(
        &*store,
        &failing,
        &*store,
        &ledger,
        "party@remote.example",
        "remote.example",
        source,
        &png,
        T0,
        &cfg,
    )
    .expect_err("second write fails");

    // The rollback attempted the delete (and it refused): no row claims
    // bytes and the reservation is free.
    assert!(
        failing.inner.deletes.load(Ordering::SeqCst) > 0,
        "rollback attempted the delete"
    );
    for variant in ["emoji", "emoji_static"] {
        assert_eq!(
            store
                .cache_get(source, variant)
                .unwrap()
                .map(|row| row.has_bytes),
            None,
            "no row claims bytes for {variant}"
        );
    }
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_for(MediaClass::AvatarEmoji), 0);
    drop(held);
}

/// Guard: the honest path still produces both keys, both rows, accounting of
/// exactly `2 * size`, and the render rows.
#[test]
fn honest_emoji_fetch_produces_both_copies_and_rows() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
    let png = png_bytes();
    let cfg = RemoteCacheConfig::default();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));

    let row = store_remote_emoji(
        &*store,
        &blobs,
        &*store,
        &ledger,
        "party@remote.example",
        "remote.example",
        "https://remote.example/e.png",
        &png,
        T0,
        &cfg,
    )
    .expect("honest emoji stages");

    let (image, static_key) = image_and_static("remote.example", "party");
    assert_eq!(row.image_key, image);
    assert_eq!(row.static_image_key, static_key);
    assert_eq!(blobs.get(&image).unwrap().as_deref(), Some(png.as_slice()));
    assert_eq!(
        blobs.get(&static_key).unwrap().as_deref(),
        Some(png.as_slice())
    );
    for (variant, key) in [("emoji", &image), ("emoji_static", &static_key)] {
        let entry = store
            .cache_get("https://remote.example/e.png", variant)
            .unwrap()
            .unwrap_or_else(|| panic!("durable row for {variant}"));
        assert!(entry.has_bytes);
        assert_eq!(entry.blob_key, *key);
        assert_eq!(entry.size, png.len() as u64);
    }
    let held = ledger.lock().unwrap();
    assert_eq!(
        held.stored_for(MediaClass::AvatarEmoji),
        2 * png.len() as u64
    );
    assert_eq!(held.inflight_total(), 0);
    drop(held);
    // The interim namespaced row and the render row both exist.
    let namespaced = store
        .emoji_by_shortcode("party@remote.example")
        .unwrap()
        .expect("namespaced emoji row");
    assert_eq!(namespaced.image_key, image);
    let rendered = store
        .remote_emojis_by_shortcodes(&["party".to_string()])
        .unwrap();
    assert!(
        rendered.iter().any(|entry| entry.domain == "remote.example"
            && entry.source_url == "https://remote.example/e.png"),
        "render row present: {rendered:?}"
    );
}

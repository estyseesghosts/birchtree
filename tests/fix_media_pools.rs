//! Fix verification: pool-aware media storage and delivery.
//!
//! Covers the media-pool work at the storage layer against temporary pools:
//!
//! - pool-aware opens and bounded range reads never buffer the whole blob
//!   (a 40 MiB remote serves concurrent ranges with range-sized
//!   allocations), and report which pool served the bytes;
//! - cache deletes clear both pools, and the startup migration moves
//!   pre-split durable `remote/` shadows into the cache pool (verified,
//!   accounted, durable copy deleted);
//! - `Disabled` cache mode refuses to persist loudly;
//! - remote emoji resolve to the evictable `remote/emojis/` namespace;
//! - emoji ingest refuses bounded header/dimension bombs.

mod common;

use std::sync::Mutex;

use birchtree::application::emoji::{add_emoji, validate_remote_emoji_bytes};
use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{BlobStore, CacheStore, EmojiStore, MediaStore, NewMedia};
use birchtree::config::{RemoteCacheConfig, RemoteCacheMode};
use birchtree::domain::Id;
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::cache_maintenance::{CacheMaintenance, enqueue_fetch_on_miss};
use birchtree::media::pooled_blob_stores::{PoolOrigin, PooledBlobStores, ShadowMigration};
use birchtree::media::remote_cache::{self, CacheLedger, MediaClass};
use common::{T0, mk_account, temp_store};

/// Deterministic blob bytes: position mixed with a prime so slices are
/// content-checked, not just length-checked.
fn pattern_bytes(len: usize) -> Vec<u8> {
    (0..len as u32).map(|i| (i % 251) as u8).collect()
}

fn open_pools(dir: &std::path::Path) -> PooledBlobStores {
    let (durable, cache) =
        FsBlobStore::open_pair(dir.join("media"), dir.join("media_cache")).expect("open pools");
    PooledBlobStores::new(durable, cache)
}

fn range_of(bytes: &[u8], offset: u64, len: u64) -> Vec<u8> {
    let start: usize = (offset as usize).min(bytes.len());
    let end: usize = (start as u64 + len).min(bytes.len() as u64) as usize;
    bytes[start..end].to_vec()
}

#[test]
fn pool_streams_concurrent_ranges_without_full_copies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pools = open_pools(dir.path());
    // 40 MiB remote, the per-file ceiling: the old serve path buffered all
    // of this per request.
    let bytes: Vec<u8> = pattern_bytes(40 * 1024 * 1024);
    pools.put("remote/7", &bytes).expect("stage remote");

    std::thread::scope(|scope| {
        for worker in 0..8u64 {
            let pools = &pools;
            let bytes = &bytes;
            scope.spawn(move || {
                for round in 0..4u64 {
                    let offset: u64 =
                        (worker * 5_000_003 + round * 1_000_033) % (bytes.len() as u64 - 65_536);
                    let (got, total, origin) = pools
                        .read_range_in_pool("remote/7", offset, 4096)
                        .expect("range read")
                        .expect("hit");
                    assert_eq!(origin, PoolOrigin::Cache);
                    assert_eq!(total, bytes.len() as u64);
                    // Bounded: the allocation is the range, never the blob.
                    assert_eq!(got.len(), 4096);
                    assert_eq!(got, range_of(bytes, offset, 4096));
                }
            });
        }
    });

    // Tail clamps, origin reports, misses stay missing.
    let (tail, total, _) = pools
        .read_range_in_pool("remote/7", total_len(&pools) - 100, 4096)
        .expect("tail")
        .expect("hit");
    assert_eq!(total, 40 * 1024 * 1024);
    assert_eq!(tail.len(), 100);
    assert!(
        pools
            .read_range_in_pool("remote/gone", 0, 8)
            .expect("miss")
            .is_none()
    );
    assert!(pools.open_in_pool("../escape").is_err());
}

fn total_len(pools: &PooledBlobStores) -> u64 {
    pools
        .open_in_pool("remote/7")
        .expect("open")
        .expect("hit")
        .len
}

#[test]
fn shadow_migration_moves_verifies_and_clears() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pools = open_pools(dir.path());

    assert_eq!(
        pools.migrate_legacy_shadow("remote/9").expect("no shadow"),
        ShadowMigration::NoShadow
    );
    assert!(pools.migrate_legacy_shadow("local/key.png").is_err());

    pools
        .durable()
        .put("remote/9", b"legacy")
        .expect("seed shadow");
    let located = pools.open_in_pool("remote/9").expect("open").expect("hit");
    assert_eq!(located.origin, PoolOrigin::DurableShadow);
    assert_eq!(located.len, 6);

    assert_eq!(
        pools.migrate_legacy_shadow("remote/9").expect("migrate"),
        ShadowMigration::Migrated { size: 6 }
    );
    assert_eq!(
        pools.get("remote/9").expect("get"),
        Some(b"legacy".to_vec())
    );
    assert!(!dir.path().join("media/remote/9").exists());
    assert!(dir.path().join("media_cache/remote/9").exists());

    // Deletes clear both pools, so eviction can never be shadowed.
    pools
        .durable()
        .put("remote/9", b"stale")
        .expect("re-shadow");
    let store: &dyn BlobStore = &pools;
    store.delete("remote/9").expect("delete");
    assert!(!dir.path().join("media/remote/9").exists());
    assert!(!dir.path().join("media_cache/remote/9").exists());
}

#[test]
fn maintenance_migrates_durable_shadows_at_startup() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().expect("tempdir");
    let (durable, cache) =
        FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
            .expect("open pools");

    // Referenced shadow: a media row names the key and knows its source.
    let account = mk_account(&store, "shadow");
    let media = store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "unknown".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media row");
    let shadow_key: String = format!("remote/{}", media.id.value());
    let shadow_bytes: Vec<u8> = pattern_bytes(2048);
    durable
        .put(&shadow_key, &shadow_bytes)
        .expect("seed shadow");
    let source = "https://cdn.example/shadow.png";
    store
        .set_remote_media_source(media.id, source)
        .expect("source");
    store
        .stage_remote_media(
            media.id,
            &shadow_key,
            "image/png",
            shadow_bytes.len() as u64,
            T0,
        )
        .expect("stage")
        .expect("row staged");

    // Stale duplicate: both pools hold the key; the cache copy wins.
    durable.put("remote/556", b"stale").expect("seed duplicate");
    cache.put("remote/556", b"fresh").expect("seed cache copy");

    // True orphan: nothing names it; the durable copy is deleted.
    durable.put("remote/557", b"orphan").expect("seed orphan");

    // Non-cache keys are never migration candidates.
    durable.put("ab/cd/keep.png", b"local").expect("seed local");

    let ledger: &'static Mutex<CacheLedger> = Box::leak(Box::new(Mutex::new(CacheLedger::new(
        RemoteCacheConfig::default(),
    ))));
    let maintenance =
        CacheMaintenance::new(store.clone(), cache.clone(), RemoteCacheConfig::default())
            .with_durable_residue_sweep(durable.clone())
            .with_ledger(ledger);
    let report = maintenance.restore_at_startup(T0).expect("restore");

    assert_eq!(report.shadows_migrated, 1);
    assert_eq!(report.shadow_bytes, shadow_bytes.len() as u64);
    assert_eq!(report.shadow_duplicates_removed, 1);
    assert_eq!(report.shadow_orphans_removed, 1);

    // The referenced shadow moved with its accounting; the durable copy is gone.
    assert_eq!(
        cache.get(&shadow_key).expect("cache read"),
        Some(shadow_bytes)
    );
    assert_eq!(durable.get(&shadow_key).expect("durable read"), None);
    let row = store
        .cache_get(source, "original")
        .expect("row")
        .expect("row present");
    assert!(row.has_bytes);
    assert_eq!(row.size, 2048);
    assert_eq!(row.blob_key, shadow_key);
    let usage = store.cache_accounting().expect("accounting");
    assert_eq!(usage.stored_total, 2048);

    // Duplicate resolved to the cache copy; orphan and local key handled.
    assert_eq!(
        cache.get("remote/556").expect("dup").as_deref(),
        Some(b"fresh".as_slice())
    );
    assert_eq!(durable.get("remote/556").expect("dup gone"), None);
    assert_eq!(durable.get("remote/557").expect("orphan gone"), None);
    assert_eq!(
        durable.get("ab/cd/keep.png").expect("local kept"),
        Some(b"local".to_vec())
    );

    // Steady state: no shadow left anywhere under the durable root.
    assert!(
        durable
            .list_keys(None, 100, std::time::Duration::ZERO)
            .expect("list")
            .iter()
            .all(|key| !key.starts_with("remote/")),
        "no remote/ key may remain outside the cache pool"
    );
}

#[test]
fn disabled_mode_never_persists_and_never_fetches() {
    let (_tmp, store) = temp_store();
    remote_cache::configure_process_ledger(RemoteCacheConfig {
        mode: RemoteCacheMode::Disabled,
        ..RemoteCacheConfig::default()
    });

    let mut ledger = CacheLedger::new(RemoteCacheConfig {
        mode: RemoteCacheMode::Disabled,
        ..RemoteCacheConfig::default()
    });
    assert!(!ledger.persistence_allowed());
    assert!(matches!(
        ledger.reserve(MediaClass::Full, 64),
        Err(remote_cache::CacheError::Unsupported(_))
    ));
    let entry = remote_cache::RemoteCacheEntry::stored(
        &remote_cache::CacheEntryCtx {
            source_url: "https://cdn.example/a.png",
            variant: "original",
            class: MediaClass::Full,
            blob_key: "remote/1",
            now_ms: T0,
            cfg: &RemoteCacheConfig::default(),
        },
        64,
    );
    assert!(ledger.commit(MediaClass::Full, 0, entry).is_err());

    // A miss stays a miss: no refetch job is enqueued while disabled.
    store
        .set_remote_media_source(Id::new(9001), "https://cdn.example/missing.png")
        .expect("source");
    assert!(!enqueue_fetch_on_miss(&*store, Id::new(9001), T0).expect("no enqueue"));

    remote_cache::configure_process_ledger(RemoteCacheConfig::default());
}

#[test]
fn remote_emoji_namespace_is_evictable_and_keyed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pools = open_pools(dir.path());

    let (image_key, static_key) =
        remote_cache::remote_emoji_keys("remote.example", "party").expect("keys");
    assert!(remote_cache::is_cache_key(&image_key));
    assert_eq!(image_key, "remote/emojis/remote.example/party/original.png");
    assert_eq!(static_key, "remote/emojis/remote.example/party/static.png");

    // Routing puts remote emoji bytes in the cache pool, never the durable one.
    let bytes: Vec<u8> = pattern_bytes(1024);
    pools.put(&image_key, &bytes).expect("put image");
    pools.put(&static_key, &bytes).expect("put static");
    assert!(dir.path().join(format!("media_cache/{image_key}")).exists());
    assert!(!dir.path().join(format!("media/{image_key}")).exists());

    // The ledger accounts them under the reserved avatar/emoji class.
    let mut ledger = CacheLedger::new(RemoteCacheConfig::default());
    ledger
        .reserve(MediaClass::AvatarEmoji, 2048)
        .expect("reserve");
    for (source, key) in [
        ("https://remote.example/a.png", &image_key),
        ("https://remote.example/b.png", &static_key),
    ] {
        ledger
            .commit(
                MediaClass::AvatarEmoji,
                0,
                remote_cache::RemoteCacheEntry::stored(
                    &remote_cache::CacheEntryCtx {
                        source_url: source,
                        variant: remote_cache::EMOJI_VARIANT,
                        class: MediaClass::AvatarEmoji,
                        blob_key: key,
                        now_ms: T0,
                        cfg: &RemoteCacheConfig::default(),
                    },
                    1024,
                ),
            )
            .expect("commit");
    }
    assert_eq!(ledger.stored_for(MediaClass::AvatarEmoji), 2048);
    assert_eq!(
        remote_cache::class_for_fetch("image/gif", remote_cache::EMOJI_VARIANT),
        MediaClass::AvatarEmoji
    );
}

#[test]
fn emoji_decode_limits_reject_bombs_before_staging() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().expect("tempdir");
    let blobs = FsBlobStore::open(dir.path().join("media")).expect("blobs");

    // A 54-byte BMP declaring 30000x30000: the header check refuses it
    // without decoding or allocating the gigapixel surface.
    let mut bomb: Vec<u8> = Vec::new();
    bomb.extend_from_slice(b"BM");
    bomb.extend_from_slice(&54u32.to_le_bytes());
    bomb.extend_from_slice(&[0u8; 4]);
    bomb.extend_from_slice(&54u32.to_le_bytes());
    bomb.extend_from_slice(&40u32.to_le_bytes());
    bomb.extend_from_slice(&30000i32.to_le_bytes());
    bomb.extend_from_slice(&30000i32.to_le_bytes());
    bomb.extend_from_slice(&1u16.to_le_bytes());
    bomb.extend_from_slice(&24u16.to_le_bytes());
    bomb.extend_from_slice(&[0u8; 24]);
    assert_eq!(bomb.len(), 54);
    let err = add_emoji(&*store, &blobs, "bomb", &bomb, None, true).expect_err("bomb refused");
    assert!(matches!(err, birchtree::domain::Error::InvalidInput(_)));

    // Oversize and non-image remote bytes are refused with the same shape.
    assert!(validate_remote_emoji_bytes(&vec![0u8; 300 * 1024]).is_err());
    assert!(validate_remote_emoji_bytes(b"plain text, not an image").is_err());

    // A real PNG still passes and stages both variants.
    let png = png_bytes();
    assert_eq!(
        validate_remote_emoji_bytes(&png).expect("valid"),
        "image/png"
    );
    add_emoji(&*store, &blobs, "party", &png, None, true).expect("add");
    let row = store
        .emoji_by_shortcode("party")
        .expect("lookup")
        .expect("row");
    assert_eq!(blobs.get(&row.image_key).expect("image"), Some(png.clone()));
    assert_eq!(blobs.get(&row.static_image_key).expect("static"), Some(png));
}

/// A valid 2x1 PNG (same encoder stack the server decodes with).
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

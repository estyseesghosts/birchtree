//! Emoji cache discipline (prod-2 item 5).
//!
//! - Local replace is transactional: a replace stages under fresh
//!   unpublished keys, commits the row to those keys, then deletes the old
//!   keys. A failed second variant or a failed metadata commit leaves the
//!   previous shortcode bytes (and row) unchanged.
//! - Remote emoji land in the evictable `remote/emojis/` namespace (never
//!   `custom_emojis/images/`), register durable byte rows so
//!   `cache_evict_batch` can reclaim them, and serve through the
//!   `/system/remote/emojis/` route with refetch-on-miss (never 500).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;

use birchtree::api::mastodon::dto;
use birchtree::application::emoji::add_emoji;
use birchtree::application::ports::{BlobStore, CacheStore, EmojiStore, JobQueue};
use birchtree::domain::{Error, JobKind, Result};
use birchtree::federation::media::store_remote_emoji;
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::pooled_blob_stores::PooledBlobStores;
use birchtree::media::remote_cache::{self, EMOJI_VARIANT, MediaClass};

use common::{TestServer, get_request, send, temp_store};

const T0: u64 = 1_700_000_000_000;

/// A valid 2x1 PNG in one flat colour.
fn png_bytes(r: u8, g: u8, b: u8) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        1,
        image::Rgba([r, g, b, 255]),
    ));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("png");
    cursor.into_inner()
}

/// A blob handle that fails exactly the second `put` (simulates a failed
/// second variant mid-replace); everything else delegates.
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
            return Err(Error::Storage("injected second-variant failure".into()));
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

/// A failed second variant leaves the previous shortcode bytes AND the row
/// unchanged: nothing ever overwrites a live key, so there is nothing to
/// roll back.
#[test]
fn failed_second_variant_keeps_previous_generation() {
    let (_dir, store) = temp_store();
    let blobs_dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(blobs_dir.path().join("media")).unwrap();
    let old_png = png_bytes(10, 20, 30);
    let first = add_emoji(&*store, &blobs, "party", &old_png, None, true).expect("add");
    let old_image = first.image_key.clone();
    let old_static = first.static_image_key.clone();

    let failing =
        FailSecondPut::new(FsBlobStore::open(blobs_dir.path().join("media")).expect("reopen"));
    let err = add_emoji(
        &*store,
        &failing,
        "party",
        &png_bytes(200, 10, 10),
        None,
        true,
    )
    .expect_err("second variant fails");
    assert!(matches!(err, Error::Storage(_)), "{err:?}");

    // Previous bytes byte-identical, row untouched.
    assert_eq!(
        blobs.get(&old_image).unwrap().as_deref(),
        Some(old_png.as_slice())
    );
    assert_eq!(
        blobs.get(&old_static).unwrap().as_deref(),
        Some(old_png.as_slice())
    );
    let row = store.emoji_by_shortcode("party").unwrap().expect("row");
    assert_eq!(row.image_key, old_image);
    assert_eq!(row.static_image_key, old_static);
}

/// A failed metadata commit (staged-but-uncommitted drop) cleans only the
/// never-published keys: the previous generation stays live. This is the
/// same unwind path `add_emoji` takes when `upsert_emoji` fails — the guard
/// never touches live keys either way.
#[test]
fn uncommitted_staging_never_touches_live_keys() {
    use birchtree::media::staging::StagingGuard;

    let dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
    blobs.put("live/a.png", b"live").unwrap();
    {
        let mut guard = StagingGuard::new(&blobs);
        guard.put("staged/b.png", b"new").unwrap();
        // Metadata commit fails here: the guard drops uncommitted.
    }
    assert_eq!(
        blobs.get("live/a.png").unwrap().as_deref(),
        Some(b"live".as_slice())
    );
    assert_eq!(blobs.get("staged/b.png").unwrap(), None);
}

/// A successful replace serves the new bytes under fresh keys and deletes
/// the previous generation; the DTO follows the row (never the canonical
/// shortcode path, which would 404 the new generation).
#[test]
fn successful_replace_serves_new_bytes_and_drops_old_keys() {
    let (_dir, store) = temp_store();
    let blobs_dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(blobs_dir.path().join("media")).unwrap();
    let old_png = png_bytes(10, 20, 30);
    let new_png = png_bytes(200, 10, 10);
    let first = add_emoji(&*store, &blobs, "party", &old_png, None, true).expect("add");

    let second = add_emoji(&*store, &blobs, "party", &new_png, None, true).expect("replace");
    assert_ne!(second.image_key, first.image_key);
    assert_ne!(second.static_image_key, first.static_image_key);
    assert_eq!(
        blobs.get(&second.image_key).unwrap().as_deref(),
        Some(new_png.as_slice())
    );
    assert_eq!(
        blobs.get(&second.static_image_key).unwrap().as_deref(),
        Some(new_png.as_slice())
    );
    assert!(
        !blobs.exists(&first.image_key).unwrap(),
        "old image deleted"
    );
    assert!(
        !blobs.exists(&first.static_image_key).unwrap(),
        "old static deleted"
    );

    // The DTO serves the row's keys (the fresh generation is servable).
    let server = common::server_info();
    let body = dto::custom_emoji_json(&server, &second, false);
    let url = body["url"].as_str().expect("url");
    let file = second.image_key.rsplit('/').next().expect("file");
    assert!(url.ends_with(file), "{url} serves the committed key");
    // Fresh adds keep the canonical keys, so their URLs are unchanged.
    let fresh = add_emoji(&*store, &blobs, "fresh", &old_png, None, true).expect("add");
    assert!(fresh.image_key.ends_with("/original/fresh.png"));
    let fresh_body = dto::custom_emoji_json(&server, &fresh, false);
    assert!(
        fresh_body["url"]
            .as_str()
            .expect("url")
            .ends_with("70/61/fresh/original/fresh.png")
            || fresh_body["url"]
                .as_str()
                .expect("url")
                .contains("original/fresh.png"),
        "{}",
        fresh_body["url"]
    );
}

/// Remote emoji stage into the evictable cache pool (never the durable
/// emoji tree) and register durable byte rows, so `cache_evict_batch` can
/// reclaim both copies.
#[test]
fn remote_emoji_live_in_the_cache_pool_and_evict() {
    let (_tmp, store) = temp_store();
    let dir = tempfile::tempdir().unwrap();
    let (durable, cache) =
        FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
            .expect("pools");
    let pools = PooledBlobStores::new(durable, cache);
    let png = png_bytes(10, 20, 30);
    let source = "https://remote.example/e.png";

    let row = store_remote_emoji(
        &*store,
        &pools,
        "party@remote.example",
        "remote.example",
        source,
        &png,
        T0,
        &birchtree::config::RemoteCacheConfig::default(),
    )
    .expect("staged");
    assert_eq!(
        row.image_key,
        "remote/emojis/remote.example/party/original.png"
    );
    assert_eq!(
        row.static_image_key,
        "remote/emojis/remote.example/party/static.png"
    );
    assert!(remote_cache::is_cache_key(&row.image_key));
    // Pool routing put both copies in the cache pool only.
    assert!(
        dir.path()
            .join(format!("media_cache/{}", row.image_key))
            .exists()
    );
    assert!(!dir.path().join(format!("media/{}", row.image_key)).exists());

    // Both copies are durable byte rows under the reserved class.
    let usage = store.cache_accounting().expect("accounting");
    assert_eq!(usage.stored_avatar_emoji, 2 * png.len() as u64);
    // …and one pressure pass reclaims them (durable rows first, blobs second).
    let victims = store
        .cache_evict_batch(MediaClass::AvatarEmoji, 2 * png.len() as u64, T0 + 1, 500)
        .expect("evict");
    assert!(victims.contains(&row.image_key), "{victims:?}");
    assert!(victims.contains(&row.static_image_key), "{victims:?}");
    for victim in &victims {
        pools.delete(victim).expect("delete");
    }
    assert_eq!(
        store
            .cache_accounting()
            .expect("accounting")
            .stored_avatar_emoji,
        0
    );
}

/// The `/system/remote/emojis/` route serves staged bytes, answers 404
/// (never 500) for evicted bytes, and queues exactly one refetch.
#[tokio::test]
async fn remote_emoji_route_serves_and_refetches_on_miss() {
    let srv = TestServer::new(true);
    let png = png_bytes(10, 20, 30);
    let source = "https://remote.example/e.png";
    let row = store_remote_emoji(
        &*srv.store,
        &*srv.blobs,
        "party@remote.example",
        "remote.example",
        source,
        &png,
        T0,
        &birchtree::config::RemoteCacheConfig::default(),
    )
    .expect("staged");

    // Hit: the staged bytes with an image content type.
    let (status, headers, body) = send(
        &srv.router,
        get_request(&format!("/system/{}", row.image_key), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(body, png);

    // Wrong shapes 404 before the filesystem (no job, no 500).
    let (status, _, _) = send(
        &srv.router,
        get_request(
            "/system/remote/emojis/remote.example/party/derived.png",
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Evict the bytes (rows survive): the route 404s and queues one refetch
    // carrying the origin URL.
    srv.blobs.delete(&row.image_key).expect("evict image");
    srv.blobs
        .delete(&row.static_image_key)
        .expect("evict static");
    let (status, _, _) = send(
        &srv.router,
        get_request(&format!("/system/{}", row.image_key), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let jobs = srv
        .store
        .claim_due(u64::MAX / 2, 100)
        .expect("claim")
        .into_iter()
        .filter(|job| job.kind == JobKind::FetchRemoteEmoji)
        .collect::<Vec<_>>();
    assert_eq!(jobs.len(), 1, "one refetch queued");
    assert_eq!(jobs[0].payload["shortcode"], "party");
    assert_eq!(jobs[0].payload["domain"], "remote.example");
    assert_eq!(jobs[0].payload["source_url"], source);
}

//! Remote media class discipline (prod-2 item 6).
//!
//! The fetch path classifies AFTER the sniff with `class_for_fetch`, never
//! assuming `Full`: audio/video originals are `Stream` — never persisted by
//! design (clients play them from the origin `remote_url`). A `Stream`
//! outcome releases the reservation, drops the download and records the row
//! as non-cached with a clear reason, then completes successfully
//! (adversarial-2 D2 stage 1): no cache blob is created and `stored_full`
//! never moves. Images still persist as `Full`, with a durable byte row for
//! the eviction passes.

mod common;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::json;

use birchtree::activitypub::FedContext;
use birchtree::application::context::Context;
use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{CacheStore, MediaStore, NewMedia};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator};
use birchtree::federation::media;
use birchtree::federation::net::SafeHttpConfig;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};

use common::{TEST_PUBLIC_URL, mk_account, temp_store};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn loopback_policy() -> SafeHttpConfig {
    SafeHttpConfig {
        allow_http: true,
        allowlist: ["127.0.0.0/8", "::1/128"]
            .iter()
            .map(|c| birchtree::federation::policy::Cidr::parse(c).unwrap())
            .collect(),
        ..SafeHttpConfig::default()
    }
}

fn context(store: &Arc<RedbStore>, dir: &std::path::Path) -> Arc<Context> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blobs"),
    );
    let server = Arc::new(birchtree::api::mastodon::ServerInfo::from_config(&Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    }));
    let streams = Hub::new(store.clone(), clock.clone(), server, Limits::default());
    Arc::new(Context::new(
        clock,
        ids,
        store.clone(),
        blobs,
        Arc::new(streams),
    ))
}

fn federation(
    store: &Arc<RedbStore>,
    dir: &std::path::Path,
) -> activitypub_federation::config::FederationConfig<FedContext> {
    common::federation_config_with(
        context(store, dir),
        TEST_PUBLIC_URL,
        &loopback_policy(),
        true,
    )
}

/// ISO-BMFF bytes that sniff as `video/mp4`.
fn mp4_bytes(len: usize) -> Vec<u8> {
    let mut mp4 = vec![0u8, 0, 0, 0x18];
    mp4.extend_from_slice(b"ftypisom");
    mp4.extend_from_slice(&[0, 0, 0, 0]);
    mp4.extend_from_slice(b"isomiso2");
    while mp4.len() < len {
        mp4.push((mp4.len() % 251) as u8);
    }
    mp4
}

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

async fn serve_bytes(path: &str, bytes: Vec<u8>) -> String {
    let app = Router::new().route(
        path,
        get(move || {
            let bytes = bytes.clone();
            async move { (StatusCode::OK, bytes) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://127.0.0.1:{port}{path}")
}

fn fetch_job(media_id: Id, now: u64) -> Job {
    Job::new(
        Id::new(now),
        JobKind::FetchRemoteMedia,
        json!({"media_id": media_id.value()}),
        now,
        now,
    )
}

fn fetch_media(store: &RedbStore, source: &str, now: u64) -> Id {
    let account = mk_account(store, &format!("fan-{}", now % 100_000));
    let attachment = store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "unknown".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media row");
    store
        .set_remote_media_source(attachment.id, source)
        .expect("source");
    attachment.id
}

/// An audio/video fixture is never persisted: no cache blob, no
/// `stored_full` movement, and the row records the clear non-persist reason.
/// The job completes successfully (adversarial-2 D2 stage 1) — never a retry
/// loop, never a silent `Full` persist, never a quarantine error.
#[tokio::test]
async fn audio_fixture_is_never_persisted() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let url = serve_bytes("/clip.mp4", mp4_bytes(64 * 1024)).await;
    let media_id = fetch_media(&store, &url, now);
    let key = media::cache_key_for(media_id);

    let before = store.cache_accounting().expect("accounting").stored_full;
    let outcome = media::fetch(&config, &fetch_job(media_id, now)).await;
    assert!(
        matches!(outcome, Ok(())),
        "Stream completes as not-cached, got {outcome:?}"
    );

    // No blob under the row key, in either pool.
    let ctx: &FedContext = &config;
    assert!(
        !ctx.app.blobs.exists(&key).expect("exists"),
        "no cache blob for audio"
    );
    let after = store.cache_accounting().expect("accounting").stored_full;
    assert_eq!(before, after, "stored_full never moves for Stream");
    assert_eq!(after, 0);

    // The row fails loudly (operator-visible reason), never staged.
    let row = store.media_by_id(media_id).expect("row").expect("media");
    assert!(row.file_key.is_empty(), "nothing staged");
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("never cached")),
        "clear reason recorded: {:?}",
        row.error
    );
}

/// Control: an image fixture still persists as `Full` with a durable byte
/// row for the eviction passes.
#[tokio::test]
async fn image_fixture_still_persists_as_full() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let png = png_bytes();
    let url = serve_bytes("/pic.png", png.clone()).await;
    let media_id = fetch_media(&store, &url, now);
    let key = media::cache_key_for(media_id);

    media::fetch(&config, &fetch_job(media_id, now))
        .await
        .expect("image stages");

    let ctx: &FedContext = &config;
    assert!(
        ctx.app.blobs.exists(&key).expect("exists"),
        "image blob staged"
    );
    let usage = store.cache_accounting().expect("accounting");
    assert_eq!(usage.stored_full, png.len() as u64);
    let row = store.media_by_id(media_id).expect("row").expect("media");
    assert_eq!(row.file_key, key);
}

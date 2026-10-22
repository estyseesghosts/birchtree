//! Workstream A (admission + fetch path): fetch-behavior integration tests.
//!
//! - A1: disabled mode refuses before any eviction (a seeded cached blob and
//!   its row survive); a genuine `BudgetExhausted` still evicts and then
//!   admits (guard).
//! - A3/N2: the body stream is capped at the live reservation, so a server
//!   declaring `Content-Length: 16` but sending ~1 MiB is rejected with
//!   nothing stored.
//! - A4: a cached video blob is never counted or staged (streams from its
//!   origin instead).
//! - Guards: honest streamed and buffered fetches, absent `Content-Length`,
//!   exact-size bodies, and concurrent fetches near the cap.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::json;

use birchtree::activitypub::FedContext;
use birchtree::application::context::Context;
use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{BlobStore, CacheStore, MediaStore, NewMedia};
use birchtree::config::{Config, RemoteCacheConfig};
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator};
use birchtree::federation::delivery::DeliveryError;
use birchtree::federation::media::{self, MAX_REMOTE_MEDIA_BYTES};
use birchtree::federation::net::SafeHttpConfig;
use birchtree::media::remote_cache::{self, MediaClass};
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};

use common::{TEST_PUBLIC_URL, mk_account, temp_store};

/// Serializes every test in this file: all of them reconfigure the
/// process-wide admission ledger.
static SERIAL: Mutex<()> = Mutex::new(());

/// Restores the default ledger budgets when a test finishes.
struct RestoreLedger;

impl Drop for RestoreLedger {
    fn drop(&mut self) {
        remote_cache::configure_process_ledger(RemoteCacheConfig::default());
    }
}

/// Fresh admission state for one test: install `cfg`, drop every stored
/// entry, and require no in-flight reservation to be outstanding.
fn reset_ledger(cfg: RemoteCacheConfig) {
    let mut ledger = remote_cache::process_ledger().lock().unwrap();
    ledger.set_caps(cfg);
    ledger.clear_entries();
    assert_eq!(
        ledger.inflight_total(),
        0,
        "no reservation may outlive a test"
    );
}

fn test_caps(global: u64, avatar: u64, preview: u64, full: u64) -> RemoteCacheConfig {
    RemoteCacheConfig {
        global_cap_bytes: global,
        avatar_emoji_cap_bytes: avatar,
        preview_cap_bytes: preview,
        full_cap_bytes: full,
        ..RemoteCacheConfig::default()
    }
}

/// Admission-sized budgets: every fetch reserves the 40 MiB worst case up
/// front, so the global and full caps must clear it for the download to
/// proceed at all.
fn fetch_caps() -> RemoteCacheConfig {
    test_caps(48 * 1024 * 1024, 1024 * 1024, 1024 * 1024, 46 * 1024 * 1024)
}

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

fn context_with(store: &Arc<RedbStore>, blobs: Arc<dyn BlobStore>) -> Arc<Context> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
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

fn rooted_context(store: &Arc<RedbStore>, dir: &std::path::Path) -> Arc<Context> {
    let blobs = Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blobs"),
    );
    context_with(store, blobs)
}

fn federation(
    store: &Arc<RedbStore>,
    dir: &std::path::Path,
) -> activitypub_federation::config::FederationConfig<FedContext> {
    common::federation_config_with(
        rooted_context(store, dir),
        TEST_PUBLIC_URL,
        &loopback_policy(),
        true,
    )
}

/// In-memory blob handle: `root_path` is `None`, so fetches take the legacy
/// buffered path.
#[derive(Debug, Default)]
struct MemBlobs {
    map: Mutex<HashMap<String, Vec<u8>>>,
}

impl BlobStore for MemBlobs {
    fn put(&self, key: &str, bytes: &[u8]) -> birchtree::domain::Result<()> {
        self.map
            .lock()
            .unwrap()
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }

    fn get(&self, key: &str) -> birchtree::domain::Result<Option<Vec<u8>>> {
        Ok(self.map.lock().unwrap().get(key).cloned())
    }

    fn delete(&self, key: &str) -> birchtree::domain::Result<()> {
        self.map.lock().unwrap().remove(key);
        Ok(())
    }

    fn exists(&self, key: &str) -> birchtree::domain::Result<bool> {
        Ok(self.map.lock().unwrap().contains_key(key))
    }
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

/// ~1 MiB of valid PNG (random noise does not compress): the lying-server
/// body. Old code stages all of it; the reservation cap must stop it first.
fn big_png_bytes() -> Vec<u8> {
    use rand::RngExt as _;

    let mut rng = rand::rng();
    let pixels: Vec<u8> = (0..512 * 512 * 4).map(|_| rng.random()).collect();
    let img =
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(512, 512, pixels).expect("raw"));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("png");
    let body = cursor.into_inner();
    assert!(body.len() > 512 * 1024, "fixture must be large");
    body
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

/// A body streamed without `Content-Length` (chunked framing).
async fn serve_stream_no_cl(path: &str, bytes: Vec<u8>) -> String {
    let app = Router::new().route(
        path,
        get(move || {
            let bytes = bytes.clone();
            async move {
                (
                    StatusCode::OK,
                    Body::from_stream(futures_util::stream::iter(vec![Ok::<_, std::io::Error>(
                        bytes,
                    )])),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://127.0.0.1:{port}{path}")
}

/// A lying origin: declares `Content-Length: {declared}` yet sends the whole
/// body chunked. Hand-rolled framing: no framework would emit both headers.
async fn serve_lying(body: Vec<u8>, declared: u64) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = vec![0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {declared}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        sock.write_all(head.as_bytes()).await.expect("head");
        sock.write_all(format!("{:X}\r\n", body.len()).as_bytes())
            .await
            .expect("chunk head");
        sock.write_all(&body).await.expect("chunk");
        sock.write_all(b"\r\n0\r\n\r\n").await.expect("end");
    });
    format!("http://127.0.0.1:{port}/lying.png")
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

fn fetch_media(store: &RedbStore, source: &str, _now: u64) -> Id {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let account = mk_account(store, &format!("fan-{n}"));
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

fn ledger_usage() -> birchtree::media::remote_cache::CacheUsage {
    remote_cache::process_ledger().lock().unwrap().usage()
}

/// A1: disabled mode refuses before any admission or eviction. The unrelated
/// cached victim (blob + durable row) survives the refused fetch.
#[tokio::test]
async fn disabled_mode_evicts_nothing_and_refuses() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(RemoteCacheConfig {
        mode: birchtree::config::RemoteCacheMode::Disabled,
        ..RemoteCacheConfig::default()
    });

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let ctx: &FedContext = &config;
    let now = now_ms();
    let png = png_bytes();

    // Unrelated victim: fully cached (blob + durable row).
    let victim_url = serve_bytes("/victim.png", png.clone()).await;
    let victim_id = fetch_media(&store, &victim_url, now);
    let victim_key = media::cache_key_for(victim_id);
    ctx.app
        .blobs
        .put(&victim_key, &png)
        .expect("seed victim blob");
    store
        .cache_mark_stored(
            &victim_url,
            "original",
            &victim_key,
            png.len() as u64,
            None,
            now,
            &RemoteCacheConfig::default(),
        )
        .expect("seed victim row");

    // The fetch under test: a different, uncached attachment.
    let target_url = serve_bytes("/target.png", png.clone()).await;
    let target_id = fetch_media(&store, &target_url, now);

    let outcome = media::fetch(&config, &fetch_job(target_id, now)).await;
    assert!(
        matches!(outcome, Err(DeliveryError::Quarantine(ref msg)) if msg.contains("disabled")),
        "disabled outcome, got {outcome:?}"
    );

    // The victim is untouched: no eviction ran.
    assert!(
        ctx.app.blobs.exists(&victim_key).expect("exists"),
        "victim blob survives"
    );
    let row = store
        .cache_get(&victim_url, "original")
        .expect("row")
        .expect("victim row survives");
    assert!(row.has_bytes, "victim row still claims its bytes");
    // And the refused fetch staged nothing.
    assert_eq!(ledger_usage().inflight_total, 0);
    assert_eq!(
        store
            .cache_get(&target_url, "original")
            .expect("lookup")
            .map(|row| row.has_bytes),
        None,
        "no durable row for the refused fetch"
    );
}

/// A1 guard: a genuine `BudgetExhausted` still evicts and then admits.
#[tokio::test]
async fn budget_exhaustion_evicts_then_admits() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(fetch_caps());

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let ctx: &FedContext = &config;
    let now = now_ms();

    // One 9 MiB victim: the 40 MiB worst-case reservation no longer fits.
    let victim_source = "https://cdn.example/victim-9m.png";
    let victim_key = "remote/9001";
    let victim_bytes = vec![7u8; 9 * 1024 * 1024];
    ctx.app.blobs.put(victim_key, &victim_bytes).expect("seed");
    store
        .cache_mark_stored(
            victim_source,
            "original",
            victim_key,
            victim_bytes.len() as u64,
            None,
            now,
            &RemoteCacheConfig::default(),
        )
        .expect("seed row");
    {
        let mut ledger = remote_cache::process_ledger().lock().unwrap();
        ledger
            .reserve(MediaClass::Full, victim_bytes.len() as u64)
            .expect("seed reserve");
        ledger
            .commit(
                MediaClass::Full,
                victim_bytes.len() as u64,
                birchtree::media::remote_cache::RemoteCacheEntry::stored(
                    victim_source,
                    "original",
                    MediaClass::Full,
                    victim_key,
                    victim_bytes.len() as u64,
                    now,
                    &RemoteCacheConfig::default(),
                ),
            )
            .expect("seed commit");
    }
    drop(victim_bytes);

    let png = png_bytes();
    let target_url = serve_bytes("/admit.png", png.clone()).await;
    let target_id = fetch_media(&store, &target_url, now);
    let target_key = media::cache_key_for(target_id);

    media::fetch(&config, &fetch_job(target_id, now))
        .await
        .expect("evicts then admits");

    // Victim gone from disk and unmapped; the newcomer staged and counted.
    assert!(
        !ctx.app.blobs.exists(victim_key).expect("exists"),
        "victim blob evicted"
    );
    let victim_row = store
        .cache_get(victim_source, "original")
        .expect("lookup")
        .expect("shell survives");
    assert!(!victim_row.has_bytes, "victim shell keeps no bytes");
    assert_eq!(
        ctx.app.blobs.get(&target_key).expect("get").as_deref(),
        Some(png.as_slice())
    );
    assert_eq!(ledger_usage().stored_full, png.len() as u64);
    assert_eq!(ledger_usage().inflight_total, 0);
}

/// A3/N2: a chunked body with a bogus `Content-Length` is treated as
/// unknown length (reqwest reports no usable length when framing is
/// chunked), so the file ceiling — not the bogus declaration — caps the
/// stream. A ~1 MiB body stages fine inside the intact worst-case
/// reservation; nothing is counted beyond what is staged.
#[tokio::test]
async fn chunked_body_with_bogus_content_length_uses_file_ceiling() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(fetch_caps());

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let big = big_png_bytes();
    let url = serve_lying(big.clone(), 16).await;
    let media_id = fetch_media(&store, &url, now);
    let key = media::cache_key_for(media_id);

    let outcome = media::fetch(&config, &fetch_job(media_id, now)).await;
    outcome.expect("chunked body inside the file ceiling stages");

    let ctx: &FedContext = &config;
    assert_eq!(
        ctx.app.blobs.get(&key).expect("get").as_deref(),
        Some(big.as_slice()),
        "staged exactly what was sent"
    );
    let usage = ledger_usage();
    assert_eq!(usage.stored_total, big.len() as u64);
    assert_eq!(usage.inflight_total, 0);
}

/// Guard: without a usable length the file ceiling caps the stream. A body
/// past the ceiling aborts mid-stream with nothing staged or counted.
#[tokio::test]
async fn body_past_the_ceiling_aborts_with_nothing_stored() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(fetch_caps());

    let (_dir, store) = temp_store();
    let blobs = Arc::new(MemBlobs::default());
    let ctx = context_with(&store, blobs.clone());
    let federation =
        common::federation_config_with(ctx.clone(), TEST_PUBLIC_URL, &loopback_policy(), true);
    let now = now_ms();
    // Valid PNG magic first (so the failure is the cap, not the sniff),
    // then zeros past the 40 MiB ceiling — chunked, so no usable length.
    let head = big_png_bytes();
    let head_len = head.len() as u64;
    let tail = vec![0u8; 1024 * 1024];
    let over = (MAX_REMOTE_MEDIA_BYTES - head_len) / (1024 * 1024) + 1;
    let app = Router::new().route(
        "/huge.png",
        get(move || {
            let head = head.clone();
            let tail = tail.clone();
            async move {
                let chunks: Vec<Result<Vec<u8>, std::io::Error>> = std::iter::once(Ok(head))
                    .chain(std::iter::repeat_with(|| Ok(tail.clone())).take(over as usize))
                    .collect();
                (
                    StatusCode::OK,
                    Body::from_stream(futures_util::stream::iter(chunks)),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    let url = format!("http://127.0.0.1:{port}/huge.png");
    let media_id = fetch_media(&store, &url, now);
    let key = media::cache_key_for(media_id);

    let outcome = media::fetch(&federation, &fetch_job(media_id, now)).await;
    assert!(
        matches!(outcome, Err(DeliveryError::Quarantine(ref msg)) if msg.contains("larger than the limit")),
        "oversize refusal, got {outcome:?}"
    );
    assert_eq!(blobs.get(&key).expect("get"), None);
    assert_eq!(
        store
            .cache_get(&url, "original")
            .expect("lookup")
            .map(|row| row.has_bytes),
        None
    );
    let usage = ledger_usage();
    assert_eq!(usage.stored_total, 0);
    assert_eq!(usage.inflight_total, 0);
}

/// A4: a cached video blob is never counted or staged.
#[tokio::test]
async fn cached_video_blob_is_neither_counted_nor_staged() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(fetch_caps());

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let ctx: &FedContext = &config;
    let now = now_ms();
    let clip = mp4_bytes(64 * 1024);

    // Stale policy-dead bytes under the row key (no ledger entry, no stage).
    let url = serve_bytes("/clip.mp4", clip.clone()).await;
    let media_id = fetch_media(&store, &url, now);
    let key = media::cache_key_for(media_id);
    ctx.app.blobs.put(&key, &clip).expect("seed stale bytes");

    let outcome = media::fetch(&config, &fetch_job(media_id, now)).await;
    assert!(
        matches!(outcome, Err(DeliveryError::Quarantine(ref msg)) if msg.contains("never cached")),
        "stream refusal, got {outcome:?}"
    );

    assert_eq!(ctx.app.blobs.get(&key).expect("get"), None);
    assert_eq!(ledger_usage().stored_full, 0);
    let row = store.media_by_id(media_id).expect("row").expect("media");
    assert!(row.file_key.is_empty(), "nothing staged");
}

/// Guard: an honest streamed fetch still stages, counts, and chains.
#[tokio::test]
async fn honest_streamed_fetch_stages_and_counts() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(fetch_caps());

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let png = png_bytes();
    let url = serve_bytes("/honest.png", png.clone()).await;
    let media_id = fetch_media(&store, &url, now);

    media::fetch(&config, &fetch_job(media_id, now))
        .await
        .expect("honest fetch stages");

    let ctx: &FedContext = &config;
    let key = media::cache_key_for(media_id);
    assert_eq!(
        ctx.app.blobs.get(&key).expect("get").as_deref(),
        Some(png.as_slice())
    );
    let usage = ledger_usage();
    assert_eq!(usage.stored_full, png.len() as u64);
    assert_eq!(usage.inflight_total, 0);
    let row = store.media_by_id(media_id).expect("row").expect("media");
    assert_eq!(row.file_key, key);
}

/// Guard: the buffered path (rootless blob handle) stages honest bytes.
#[tokio::test]
async fn honest_buffered_fetch_stages_and_counts() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(fetch_caps());

    let (_dir, store) = temp_store();
    let blobs: Arc<dyn BlobStore> = Arc::new(MemBlobs::default());
    let ctx = context_with(&store, blobs.clone());
    let federation =
        common::federation_config_with(ctx.clone(), TEST_PUBLIC_URL, &loopback_policy(), true);
    let now = now_ms();
    let png = png_bytes();
    let url = serve_bytes("/buffered.png", png.clone()).await;
    let media_id = fetch_media(&store, &url, now);

    media::fetch(&federation, &fetch_job(media_id, now))
        .await
        .expect("buffered fetch stages");

    let key = media::cache_key_for(media_id);
    assert_eq!(
        blobs.get(&key).expect("get").as_deref(),
        Some(png.as_slice())
    );
    let usage = ledger_usage();
    assert_eq!(usage.stored_full, png.len() as u64);
    assert_eq!(usage.inflight_total, 0);
}

/// Guard: absent `Content-Length` still fetches under the file ceiling.
#[tokio::test]
async fn absent_content_length_still_fetches() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    reset_ledger(fetch_caps());

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let png = png_bytes();
    let url = serve_stream_no_cl("/chunked.png", png.clone()).await;
    let media_id = fetch_media(&store, &url, now);

    media::fetch(&config, &fetch_job(media_id, now))
        .await
        .expect("chunked fetch stages");

    assert_eq!(ledger_usage().stored_full, png.len() as u64);
    assert_eq!(ledger_usage().inflight_total, 0);
}

/// Guard: concurrent fetches near the cap never exceed it; losers retry.
#[tokio::test]
async fn concurrent_fetches_never_exceed_the_cap() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreLedger;
    // Two worst-case (40 MiB) reservations fit; the third does not.
    let cap = 2 * MAX_REMOTE_MEDIA_BYTES + 8 * 1024 * 1024;
    remote_cache::configure_process_ledger(test_caps(cap, 1024 * 1024, 1024 * 1024, cap));

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let png = png_bytes();
    let url = serve_bytes("/shared.png", png.clone()).await;
    // Stagger the media ids but share the origin: four racers, two winners.
    let mut jobs = Vec::new();
    for _ in 0..4 {
        let media_id = fetch_media(&store, &url, now);
        jobs.push((media_id, fetch_job(media_id, now)));
    }

    let mut handles = Vec::new();
    for (_, job) in jobs {
        let config = config.clone();
        handles.push(tokio::spawn(async move {
            media::fetch(&config, &job).await.is_ok()
        }));
    }
    let mut ok = 0;
    for handle in handles {
        if handle.await.expect("join") {
            ok += 1;
        }
    }
    assert_eq!(ok, 2, "exactly two worst-case reservations fit");
    let usage = ledger_usage();
    assert!(usage.stored_full <= cap, "global cap holds");
    assert_eq!(usage.inflight_total, 0, "every reservation settled");
}

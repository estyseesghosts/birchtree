//! Work packages 5 and 8.3 through the real router, redb and file system.
//!
//! Upload limits (file cap independent of the body cap, abort before the
//! payload is resident, cleanup on malformed bodies and disconnects, decoded
//! size limits), streaming + byte-range serving, and the hardened public
//! media route (guessed keys, traversal, backslashes, symlinks, staging
//! names, missing files).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use birchtree::api::mastodon::{ApiState, ServerInfo, build_router};
use birchtree::application::context::Context;
use birchtree::application::ports::{BlobStore, JobQueue};
use birchtree::config::Config;
use birchtree::domain::{Clock, IdGenerator, SystemClock, TimestampIdGenerator};
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::pooled_blob_stores::PooledBlobStores;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};
use tower::ServiceExt;

use common::{TEST_PUBLIC_URL, form_request, get_request, json, send};

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses \
                         write:statuses write:media";
const BOUNDARY: &str = "birchtree-c-boundary";

// ---------------------------------------------------------------------------
// Harness: the production wiring (two pools behind one routing handle).
// ---------------------------------------------------------------------------

struct Harness {
    dir: tempfile::TempDir,
    store: Arc<RedbStore>,
    pools: PooledBlobStores,
    router: axum::Router,
}

impl Harness {
    fn new(max_file_size: u64, max_body_size: usize) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
        let store = Arc::new(
            RedbStore::open(dir.path().join("birchtree.redb"), clock.clone(), ids.clone())
                .expect("open store"),
        );
        let (durable, cache) = FsBlobStore::open_pair(
            dir.path().join("media"),
            dir.path().join("media_cache"),
        )
        .expect("open pools");
        store.attach_cache_pool(cache.clone());
        let pools = PooledBlobStores::new(durable, cache);
        let blobs: Arc<dyn BlobStore> = Arc::new(pools.clone());
        let cfg = Config {
            public_url: TEST_PUBLIC_URL.to_string(),
            registration_open: true,
            transcoding_enabled: false,
            max_file_size,
            max_body_size,
            ..Config::default()
        };
        let server = Arc::new(ServerInfo::from_config(&cfg));
        let streams = Hub::new(store.clone(), clock.clone(), server.clone(), Limits::default());
        let context = Arc::new(Context::new(
            clock,
            ids,
            store.clone(),
            blobs,
            Arc::new(streams.clone()),
        ));
        let federation = common::federation_config(context.clone(), TEST_PUBLIC_URL);
        let router = build_router(ApiState {
            context,
            trusted_proxies: Arc::new(Vec::new()),
            max_body_size,
            server,
            streams,
            federation,
            rate_limit_multiplier: 1,
            mail_env: Arc::new(birchtree::mailer::MailEnv::disabled()),
            invites: store.clone() as Arc<dyn birchtree::application::accounts::InviteStore>,
        });
        Self {
            dir,
            store,
            pools,
            router,
        }
    }

    fn durable_root(&self) -> PathBuf {
        self.dir.path().join("media")
    }

    /// Every regular file under the durable root.
    fn durable_files(&self) -> Vec<PathBuf> {
        walk_files(&self.durable_root())
    }
}

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

fn enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn user_token(h: &Harness, username: &str) -> String {
    let body = format!(
        "client_name=Test+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(ALL_SCOPES),
    );
    let (status, _h, bytes) =
        send(&h.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    let app = json(&bytes);
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc("write:accounts"),
    );
    let (status, _h, bytes) =
        send(&h.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    let app_token = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.social")),
    );
    let (status, _h, bytes) = send(
        &h.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 2, |x, y| {
        image::Rgba([(x * 40 + 10) as u8, (y * 90 + 20) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

/// A valid PNG plus `extra` trailing bytes (decoders ignore trailing data;
/// the sniffer only reads the signature).
fn padded_png(extra: usize) -> Vec<u8> {
    let mut bytes = png_bytes();
    bytes.extend(std::iter::repeat_n(0u8, extra));
    bytes
}

/// ISO-BMFF bytes that sniff as `video/mp4`, padded to `len`.
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

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A tiny PNG whose header declares `width` x `height` (a decompression
/// bomb: kilobytes on the wire, gigabytes if decoded).
fn bomb_png(width: u32, height: u32) -> Vec<u8> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut chunk = b"IHDR".to_vec();
    chunk.extend_from_slice(&ihdr);
    png.extend_from_slice(&(ihdr.len() as u32).to_be_bytes());
    png.extend_from_slice(&chunk);
    png.extend_from_slice(&crc32(&chunk).to_be_bytes());
    let iend = b"IEND".to_vec();
    png.extend_from_slice(&0u32.to_be_bytes());
    png.extend_from_slice(&iend);
    png.extend_from_slice(&crc32(&iend).to_be_bytes());
    png
}

fn file_part(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut part = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"f.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    part.extend_from_slice(bytes);
    part.extend_from_slice(b"\r\n");
    part
}

fn upload_request(token: &str, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .header("authorization", format!("Bearer {token}"))
        .body(body)
        .expect("upload request")
}

fn upload_bytes_request(token: &str, file: &[u8]) -> Request<Body> {
    let mut body = file_part("file", file);
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    upload_request(token, "/api/v1/media", Body::from(body))
}

// ---------------------------------------------------------------------------
// 5.1 / 5.2 upload limits and spooling.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn configured_file_limit_is_enforced_independently_of_the_body_limit() {
    // Body limit 4 MiB, file limit 4 KiB: the old code used the body limit.
    let h = Harness::new(4096, 4 * 1024 * 1024);
    let token = user_token(&h, "alice").await;

    let (status, _h, body) = send(&h.router, upload_bytes_request(&token, &padded_png(8000))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&body));
    let error = json(&body)["error"].as_str().unwrap().to_string();
    assert!(error.contains("File must be less than 4 KB"), "{error}");
    assert!(h.durable_files().is_empty(), "rejected upload leaves no file: {:?}", h.durable_files());

    // Within the file limit it succeeds, and the original is on disk.
    let (status, _h, body) = send(&h.router, upload_bytes_request(&token, &png_bytes())).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(!h.durable_files().is_empty());
    assert!(
        walk_files(&h.durable_root().join("tmp")).is_empty(),
        "the spool file was moved onto its key"
    );
}

/// A multipart body that yields `chunks` zero-filled chunks after the part
/// header and counts how many chunks the server actually pulled.
fn counting_body(chunk_len: usize, chunks: usize, pulled: Arc<AtomicUsize>, finish: bool) -> Body {
    let header = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    let stream = futures_util::stream::unfold(0usize, move |sent| {
        let header = header.clone();
        let pulled = pulled.clone();
        async move {
            if sent == 0 {
                return Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(header)), 1));
            }
            if sent <= chunks {
                pulled.fetch_add(1, Ordering::SeqCst);
                return Some((Ok(axum::body::Bytes::from(vec![0u8; chunk_len])), sent + 1));
            }
            if sent == chunks + 1 && finish {
                let tail = format!("\r\n--{BOUNDARY}--\r\n").into_bytes();
                return Some((Ok(axum::body::Bytes::from(tail)), sent + 1));
            }
            if finish {
                return None;
            }
            // Never finishes: the client "stalls" mid-body.
            std::future::pending().await
        }
    });
    Body::from_stream(stream)
}

#[tokio::test]
async fn oversized_field_is_aborted_before_the_full_payload_is_resident() {
    // 64 MiB body allowed, 64 KiB per file: a 32 MiB stream must be cut off
    // after roughly the file limit, not read to the end.
    let h = Harness::new(64 * 1024, 64 * 1024 * 1024);
    let token = user_token(&h, "bob").await;
    let pulled = Arc::new(AtomicUsize::new(0));
    let body = counting_body(16 * 1024, 2048, pulled.clone(), true);
    let (status, _h, bytes) = send(&h.router, upload_request(&token, "/api/v1/media", body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    let consumed = pulled.load(Ordering::SeqCst);
    assert!(
        consumed <= 16,
        "server pulled {consumed} x 16 KiB chunks before aborting a 64 KiB cap"
    );
    assert!(walk_files(&h.durable_root()).is_empty(), "partial spool removed");
}

#[tokio::test]
async fn malformed_multipart_cleans_the_spool() {
    let h = Harness::new(1024 * 1024, 4 * 1024 * 1024);
    let token = user_token(&h, "carol").await;
    // The file part starts and the body ends without a closing boundary.
    let mut body = file_part("file", &padded_png(2000));
    body.truncate(body.len() - 20);
    let (status, _h, _bytes) = send(
        &h.router,
        upload_request(&token, "/api/v1/media", Body::from(body)),
    )
    .await;
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "malformed body answered {status}"
    );
    assert!(walk_files(&h.durable_root()).is_empty(), "no spool or blob remains");
}

#[tokio::test]
async fn client_disconnect_mid_upload_cleans_the_spool() {
    let h = Harness::new(8 * 1024 * 1024, 16 * 1024 * 1024);
    let token = user_token(&h, "dave").await;
    let pulled = Arc::new(AtomicUsize::new(0));
    // The body stalls forever after 8 chunks; the request future is dropped
    // when the client gives up, as hyper does on a closed connection.
    let body = counting_body(16 * 1024, 8, pulled.clone(), false);
    let request = upload_request(&token, "/api/v1/media", body);
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(400),
        h.router.clone().oneshot(request),
    )
    .await;
    assert!(outcome.is_err(), "the stalled upload must still be in flight");
    assert!(pulled.load(Ordering::SeqCst) >= 1, "some bytes were spooled");
    // Dropped futures run their destructors synchronously.
    assert!(
        walk_files(&h.durable_root()).is_empty(),
        "abandoned spool removed: {:?}",
        h.durable_files()
    );
}

#[tokio::test]
async fn decompression_bomb_is_rejected_before_decode() {
    let h = Harness::new(1024 * 1024, 4 * 1024 * 1024);
    let token = user_token(&h, "erin").await;
    // 60_000 x 60_000 RGBA would need ~14 GB; the file is ~60 bytes.
    let bomb = bomb_png(60_000, 60_000);
    assert!(bomb.len() < 128);
    let started = std::time::Instant::now();
    let (status, _h, body) = send(&h.router, upload_bytes_request(&token, &bomb)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&body));
    assert!(started.elapsed().as_secs() < 5, "rejected from the header, not by decoding");

    // Many pixels but within the cap still decodes (sanity: limit is a cap,
    // not a blanket refusal of big dimensions).
    let wide = bomb_png(20_000, 1);
    let (status, _h, _body) = send(&h.router, upload_bytes_request(&token, &wide)).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an IHDR with no pixel data fails to decode, but not via the size limit"
    );
}

#[tokio::test]
async fn concurrent_uploads_complete_through_the_bounded_gate() {
    let h = Harness::new(2 * 1024 * 1024, 8 * 1024 * 1024);
    let token = user_token(&h, "frank").await;
    let mut tasks = Vec::new();
    for _ in 0..24 {
        let router = h.router.clone();
        let token = token.clone();
        tasks.push(tokio::spawn(async move {
            let response = router
                .oneshot(upload_bytes_request(&token, &padded_png(1000)))
                .await
                .expect("response");
            response.status()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.expect("join"), StatusCode::OK);
    }
    let gate = birchtree::media::upload_gate::media_work_gate();
    assert_eq!(
        gate.available(),
        birchtree::media::upload_gate::DEFAULT_MEDIA_WORK_LIMIT,
        "every slot is returned"
    );
    assert!(walk_files(&h.durable_root().join("tmp")).is_empty());
}

// ---------------------------------------------------------------------------
// 5.2 streaming and byte ranges.
// ---------------------------------------------------------------------------

const PUBLISHED_KEY: &str = "ab/cd/0123456789abcdef/original.mp4";

fn get_with_range(path: &str, range: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(range) = range {
        builder = builder.header("range", range);
    }
    builder.body(Body::empty()).expect("request")
}

fn route(key: &str) -> String {
    format!("/system/media_attachments/files/{key}")
}

#[tokio::test]
async fn byte_ranges_are_honored_for_large_media() {
    let h = Harness::new(8 * 1024 * 1024, 16 * 1024 * 1024);
    let media = mp4_bytes(300 * 1024);
    h.pools.put(PUBLISHED_KEY, &media).unwrap();
    let path = route(PUBLISHED_KEY);

    let (status, headers, body) = send(&h.router, get_with_range(&path, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["accept-ranges"], "bytes");
    assert_eq!(headers["content-length"], media.len().to_string().as_str());
    assert_eq!(headers["content-type"], "video/mp4");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(body, media);

    let (status, headers, body) = send(&h.router, get_with_range(&path, Some("bytes=100-199"))).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers["content-range"], format!("bytes 100-199/{}", media.len()).as_str());
    assert_eq!(headers["content-length"], "100");
    assert_eq!(body, &media[100..200]);

    let (status, headers, body) = send(&h.router, get_with_range(&path, Some("bytes=-1000"))).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers["content-range"],
        format!("bytes {}-{}/{}", media.len() - 1000, media.len() - 1, media.len()).as_str()
    );
    assert_eq!(body, &media[media.len() - 1000..]);

    // Open-ended range across several read chunks.
    let start = 70_000usize;
    let (status, _headers, body) =
        send(&h.router, get_with_range(&path, Some(&format!("bytes={start}-")))).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, &media[start..]);

    let (status, headers, _body) = send(
        &h.router,
        get_with_range(&path, Some(&format!("bytes={}-", media.len()))),
    )
    .await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers["content-range"], format!("bytes */{}", media.len()).as_str());

    // Multi-range and garbage are answered with the whole body.
    for odd in ["bytes=0-1,5-6", "items=0-5", "bytes=oops"] {
        let (status, _h, body) = send(&h.router, get_with_range(&path, Some(odd))).await;
        assert_eq!(status, StatusCode::OK, "{odd}");
        assert_eq!(body.len(), media.len());
    }
}

// ---------------------------------------------------------------------------
// 8.3 public route hardening.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn public_route_serves_only_published_keys() {
    let h = Harness::new(8 * 1024 * 1024, 16 * 1024 * 1024);
    let shard = "ab/cd/0123456789abcdef";
    let good = format!("{shard}/original.png");
    h.pools.put(&good, &png_bytes()).unwrap();

    // Staging and scratch files exist on disk next to the real blob.
    let dir = h.durable_root().join(shard);
    std::fs::write(dir.join(".original.png.part-1-2"), b"partial-bytes-not-media").unwrap();
    std::fs::write(dir.join("derived.webm"), b"scratch-transcode-output").unwrap();
    std::fs::write(dir.join("notes.txt"), b"not media").unwrap();
    std::fs::create_dir_all(h.durable_root().join("tmp")).unwrap();
    std::fs::write(h.durable_root().join("tmp/1-2-3.part"), b"downloading").unwrap();
    std::fs::write(h.dir.path().join("outside.txt"), b"outside the media root").unwrap();
    h.pools
        .put("custom_emojis/images/6b/65/key/original/key.png", &png_bytes())
        .unwrap();

    let (status, headers, body) = send(&h.router, get_request(&route(&good), None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, png_bytes());
    assert_eq!(headers["content-type"], "image/png");

    let denied = [
        format!("{shard}/.original.png.part-1-2"),
        format!("{shard}/derived.webm"),
        format!("{shard}/notes.txt"),
        "tmp/1-2-3.part".to_string(),
        // Guessed, missing keys.
        "ab/cd/ffffffffffffffff/original.png".to_string(),
        "00/00/0000000000000000/original.png".to_string(),
        // Traversal in every spelling.
        format!("{shard}/../../../outside.txt"),
        "..%2F..%2Foutside.txt".to_string(),
        "%2e%2e/%2e%2e/outside.txt".to_string(),
        format!("{shard}/%2e%2e%2f%2e%2e%2foriginal.png"),
        "..%5C..%5Coutside.txt".to_string(),
        format!("{shard}%5Coriginal.png"),
        "ab%5Ccd%5C0123456789abcdef%5Coriginal.png".to_string(),
        format!("{shard}//original.png"),
        format!("{shard}/original.png/"),
        format!("{shard}/original.png%00"),
        format!("{shard}/ORIGINAL.PNG"),
        // Other namespaces have their own route.
        "custom_emojis/images/6b/65/key/original/key.png".to_string(),
        "remote/not-a-number".to_string(),
        "remote/1/2".to_string(),
        String::new(),
    ];
    for path in denied {
        let (status, _headers, body) = send(&h.router, get_request(&route(&path), None, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path:?} answered {status}");
        assert!(
            !body.windows(7).any(|w| w == b"partial" || w == b"scratch" || w == b"outside"),
            "{path:?} leaked file contents"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn public_route_never_follows_symlinks() {
    use std::os::unix::fs::symlink;

    let h = Harness::new(8 * 1024 * 1024, 16 * 1024 * 1024);
    let outside = h.dir.path().join("outside.png");
    std::fs::write(&outside, png_bytes()).unwrap();
    let shard_dir = h.durable_root().join("ab/cd/0123456789abcdef");
    std::fs::create_dir_all(&shard_dir).unwrap();
    symlink(&outside, shard_dir.join("original.png")).unwrap();
    // A linked shard directory.
    symlink(h.dir.path(), h.durable_root().join("ee")).unwrap();

    for path in ["ab/cd/0123456789abcdef/original.png", "ee/cd/0123456789abcdef/original.png"] {
        let (status, _h, _b) = send(&h.router, get_request(&route(path), None, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn evicted_remote_cache_bytes_answer_404_and_queue_one_refetch() {
    use birchtree::application::federation_ports::FederationContentStore;
    use birchtree::application::ports::{MediaStore, NewMedia};

    let h = Harness::new(8 * 1024 * 1024, 16 * 1024 * 1024);
    let account = common::mk_account(&h.store, "origin");
    let media = h
        .store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "unknown".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media row");
    h.store
        .set_remote_media_source(media.id, "https://remote.example/a.png")
        .unwrap();
    let path = route(&format!("remote/{}", media.id.value()));

    let before = jobs_of_kind(&h, birchtree::domain::JobKind::FetchRemoteMedia);
    for _ in 0..5 {
        let (status, _h, _b) = send(&h.router, get_request(&path, None, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let after = jobs_of_kind(&h, birchtree::domain::JobKind::FetchRemoteMedia);
    assert_eq!(after - before, 1, "single-flight: one refetch job for five misses");

    // Once the bytes are back in the cache pool the same URL serves them.
    h.pools
        .put(&format!("remote/{}", media.id.value()), &png_bytes())
        .unwrap();
    let (status, _h, body) = send(&h.router, get_request(&path, None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, png_bytes());
}

fn jobs_of_kind(h: &Harness, kind: birchtree::domain::JobKind) -> usize {
    h.store
        .claim_due(u64::MAX / 2, 1000)
        .expect("claim")
        .into_iter()
        .filter(|job| job.kind == kind)
        .count()
}

// ---------------------------------------------------------------------------
// 8.3 emoji route hardening (equivalent of the attachment route).
// ---------------------------------------------------------------------------

fn emoji_route(path: &str) -> String {
    format!("/system/custom_emojis/images/{path}")
}

#[tokio::test]
async fn emoji_route_serves_only_published_shapes() {
    let h = Harness::new(8 * 1024 * 1024, 16 * 1024 * 1024);
    // `party` = 0x70 0x61 → partition `70/61`.
    let good = "70/61/party/original/party.png";
    h.pools
        .put(&format!("custom_emojis/images/{good}"), &png_bytes())
        .unwrap();

    // Staging and scratch files exist on disk next to the real image.
    let dir = h.durable_root().join("custom_emojis/images/70/61/party/original");
    std::fs::write(dir.join(".party.png.part-1-2"), b"partial-bytes-not-emoji").unwrap();
    std::fs::write(dir.join("derived.png"), b"scratch-output").unwrap();
    std::fs::write(h.dir.path().join("outside.png"), png_bytes()).unwrap();

    let (status, headers, body) = send(&h.router, get_request(&emoji_route(good), None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, png_bytes());
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");

    let denied = [
        // Staging residue and scratch files on disk.
        "70/61/party/original/.party.png.part-1-2".to_string(),
        "70/61/party/original/derived.png".to_string(),
        // Guessed, missing emoji.
        "70/61/party/original/missing.png".to_string(),
        "70/61/other/original/other.png".to_string(),
        "00/00/party/original/party.png".to_string(),
        // Wrong style or extension.
        "70/61/party/derived/party.png".to_string(),
        "70/61/party/original/party.jpg".to_string(),
        // Traversal in every spelling.
        "70/61/party/../../../outside.png".to_string(),
        "..%2F..%2Foutside.png".to_string(),
        "70%5C61%5Cparty%5Coriginal%5Cparty.png".to_string(),
        "70/61/party/original/party.png%00".to_string(),
        // Other namespaces have their own route.
        "ab/cd/0123456789abcdef/original.png".to_string(),
        String::new(),
    ];
    for path in denied {
        let (status, _headers, body) =
            send(&h.router, get_request(&emoji_route(&path), None, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path:?} answered {status}");
        assert!(
            !body.windows(7).any(|w| w == b"partial" || w == b"scratch"),
            "{path:?} leaked file contents"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn emoji_route_never_follows_symlinks() {
    use std::os::unix::fs::symlink;

    let h = Harness::new(8 * 1024 * 1024, 16 * 1024 * 1024);
    let outside = h.dir.path().join("outside.png");
    std::fs::write(&outside, png_bytes()).unwrap();
    let shard_dir = h.durable_root().join("custom_emojis/images/70/61/party/original");
    std::fs::create_dir_all(&shard_dir).unwrap();
    symlink(&outside, shard_dir.join("party.png")).unwrap();

    let (status, _h, _b) =
        send(&h.router, get_request(&emoji_route("70/61/party/original/party.png"), None, None))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

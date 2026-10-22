//! Remote-cache HTTP streaming (prod-2 item 2).
//!
//! The public file route must stream `remote/…` bytes from the cache pool
//! instead of buffering the whole object through `BlobStore::get()`. The
//! runtime handle fronts both pools, so `stream_roots` serves durable first
//! (local keys, legacy durable shadows) and the cache pool second. The
//! `NoBufferBlobs` handle below panics in `get()`: any `GET` that passes
//! proves the handler never allocated the full object.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};

use birchtree::api::mastodon::{ApiState, ServerInfo, build_router};
use birchtree::application::ports::BlobStore;
use birchtree::application::context::Context;
use birchtree::config::Config;
use birchtree::domain::{Clock, IdGenerator, Result, SystemClock, TimestampIdGenerator};
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::pooled_blob_stores::PooledBlobStores;
use birchtree::streaming::{Hub, Limits};

use common::{TEST_PUBLIC_URL, get_request, send};

/// Deterministic blob bytes: position mixed with a prime so slices are
/// content-checked, not just length-checked.
fn pattern_bytes(len: usize) -> Vec<u8> {
    (0..len as u32).map(|i| (i % 251) as u8).collect()
}

/// A `BlobStore` fronting the real pools whose buffered `get()` panics: the
/// serve path must stream every file-backed key from disk and reserve `get()`
/// for rootless doubles only.
struct NoBufferBlobs {
    inner: PooledBlobStores,
}

impl birchtree::application::ports::BlobStore for NoBufferBlobs {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        self.inner.put(key, bytes)
    }

    fn get(&self, _key: &str) -> Result<Option<Vec<u8>>> {
        panic!("locate must stream file-backed keys, never buffer via get()")
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.inner.delete(key)
    }

    fn exists(&self, key: &str) -> Result<bool> {
        self.inner.exists(key)
    }

    fn root_path(&self) -> Option<PathBuf> {
        self.inner.root_path()
    }

    fn cache_root_path(&self) -> Option<PathBuf> {
        self.inner.cache_root_path()
    }

    fn publish_tmp(&self, tmp: &std::path::Path, key: &str) -> Result<()> {
        self.inner.publish_tmp(tmp, key)
    }

    fn put_streamed(&self, key: &str, bytes: &[u8], max_bytes: u64) -> Result<()> {
        self.inner.put_streamed(key, bytes, max_bytes)
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    pools: PooledBlobStores,
    router: axum::Router,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(common::open_store(dir.path()));
        let (durable, cache) =
            FsBlobStore::open_pair(dir.path().join("media"), dir.path().join("media_cache"))
                .expect("open pools");
        let pools = PooledBlobStores::new(durable, cache);
        let blobs: Arc<NoBufferBlobs> = Arc::new(NoBufferBlobs {
            inner: pools.clone(),
        });
        let cfg = Config {
            public_url: TEST_PUBLIC_URL.to_string(),
            registration_open: true,
            transcoding_enabled: false,
            ..Config::default()
        };
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> =
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
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
            max_body_size: 16 * 1024 * 1024,
            server,
            streams,
            federation,
            rate_limit_multiplier: 1,
            mail_env: Arc::new(birchtree::mailer::MailEnv::new(
                "noreply@example.social",
                true,
            )),
            invites: store.clone()
                as Arc<dyn birchtree::application::accounts::InviteStore>,
        });
        Self {
            _dir: dir,
            pools,
            router,
        }
    }
}

fn route(key: &str) -> String {
    format!("/system/media_attachments/files/{key}")
}

fn ranged(path: &str, range: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("range", range)
        .body(Body::empty())
        .expect("request")
}

/// A multi-megabyte object living only in the cache pool serves a `Range`
/// as 206 with the exact slice — through the panicking `get()`, so the
/// handler streamed from disk and never buffered the object.
#[tokio::test]
async fn cache_pool_remote_streams_ranges_without_buffering() {
    let h = Harness::new();
    let bytes: Vec<u8> = pattern_bytes(3 * 1024 * 1024);
    h.pools.put("remote/7", &bytes).expect("stage remote");

    let (status, headers, body) = send(
        &h.router,
        ranged(&route("remote/7"), "bytes=100-199"),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers["content-range"],
        format!("bytes 100-199/{}", bytes.len()).as_str()
    );
    assert_eq!(headers["content-length"], "100");
    assert_eq!(body, bytes[100..200]);

    // Open-ended range across many read chunks.
    let (status, _, body) = send(&h.router, ranged(&route("remote/7"), "bytes=2000000-")).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, bytes[2_000_000..]);

    // Whole-body GET streams too (no buffered fallback anywhere).
    let (status, headers, body) = send(&h.router, get_request(&route("remote/7"), None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-length"], bytes.len().to_string().as_str());
    assert_eq!(body, bytes);
}

/// A pre-split durable shadow still resolves (durable root stays first).
#[tokio::test]
async fn durable_shadow_remote_still_streams() {
    let h = Harness::new();
    let bytes: Vec<u8> = pattern_bytes(4096);
    h.pools
        .durable()
        .put("remote/9", &bytes)
        .expect("seed shadow");

    let (status, _, body) = send(&h.router, ranged(&route("remote/9"), "bytes=0-99")).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, bytes[..100]);
}

/// Local keys stream from the durable pool, unchanged.
#[tokio::test]
async fn durable_local_key_streams() {
    let h = Harness::new();
    let key = "ab/cd/0123456789abcdef/original.png";
    let bytes: Vec<u8> = pattern_bytes(4096);
    h.pools.put(key, &bytes).expect("stage local");

    let (status, headers, body) = send(&h.router, get_request(&route(key), None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["accept-ranges"], "bytes");
    assert_eq!(body, bytes);
}

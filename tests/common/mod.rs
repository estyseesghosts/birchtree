//! Shared helpers for integration tests.
#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use birchtree::api::mastodon::{ApiState, ServerInfo, build_router};
use birchtree::application::accounts::{create_account, CreateAccountCommand};
use birchtree::application::context::Context;
use birchtree::config::Config;
use birchtree::domain::{Account, Clock, IdGenerator, SystemClock, TimestampIdGenerator};
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};
use tower::ServiceExt;

/// The public URL used by tests, so DTOs are deterministic.
pub const TEST_PUBLIC_URL: &str = "https://example.social";

/// A `ServerInfo` for the test public URL.
pub fn server_info() -> Arc<ServerInfo> {
    let cfg = Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    };
    Arc::new(ServerInfo::from_config(&cfg))
}

/// Open a fresh store in a temporary directory. The `TempDir` must be kept
/// alive for the duration of the test.
pub fn temp_store() -> (tempfile::TempDir, Arc<RedbStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    (dir, Arc::new(store))
}

/// Open a store at an arbitrary directory (used to reopen after a restart).
pub fn open_store(dir: &std::path::Path) -> RedbStore {
    RedbStore::open(
        dir.join("birchtree.redb"),
        Arc::new(SystemClock),
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
    )
    .expect("open store")
}

/// A fixed timestamp used across tests for determinism.
pub const T0: u64 = 1_700_000_000_000;

/// Create a local account through the application use case.
pub fn mk_account(store: &RedbStore, username: &str) -> Account {
    create_account(
        store,
        "https://example.social",
        T0,
        CreateAccountCommand {
            username: username.to_string(),
            email: None,
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create account")
}

/// An in-process server backed by a fresh temporary store and the real router.
///
/// The `TempDir` must be kept alive for the duration of the test.
pub struct TestServer {
    pub dir: tempfile::TempDir,
    pub store: Arc<RedbStore>,
    pub blobs: Arc<birchtree::media::blob_store::FsBlobStore>,
    /// The streaming hub the router publishes to (also `context.streaming`).
    pub streams: Hub,
    pub router: axum::Router,
}

impl TestServer {
    /// Open a fresh server with the given registration policy.
    pub fn new(registration_open: bool) -> Self {
        let (dir, store) = temp_store();
        Self::assemble(dir, store, registration_open, Limits::default())
    }

    /// Open a fresh server whose streaming hub uses `limits` (streaming tests
    /// shorten the heartbeat and stall timers and tighten the caps).
    pub fn with_limits(limits: Limits) -> Self {
        let (dir, store) = temp_store();
        Self::assemble(dir, store, false, limits)
    }

    /// Open a fresh server with a config tweak (core-extension policy
    /// tests set `license_policy` / `place_policy` / `alt_text_policy`).
    pub fn with_config(
        registration_open: bool,
        configure: impl FnOnce(&mut Config),
    ) -> Self {
        let (dir, store) = temp_store();
        let mut cfg = Config {
            public_url: TEST_PUBLIC_URL.to_string(),
            registration_open,
            transcoding_enabled: false,
            ..Config::default()
        };
        configure(&mut cfg);
        Self::assemble_with_cfg(dir, store, cfg, Limits::default())
    }

    fn assemble(
        dir: tempfile::TempDir,
        store: Arc<RedbStore>,
        registration_open: bool,
        limits: Limits,
    ) -> Self {
        let cfg = Config {
            public_url: TEST_PUBLIC_URL.to_string(),
            registration_open,
            // Hermetic HTTP tests: no ffmpeg on the test machine, so the
            // transcode pipeline stays off (unit coverage drives
            // `run_transcode_job` directly with explicit configs).
            transcoding_enabled: false,
            ..Config::default()
        };
        Self::assemble_with_cfg(dir, store, cfg, limits)
    }

    fn assemble_with_cfg(
        dir: tempfile::TempDir,
        store: Arc<RedbStore>,
        cfg: Config,
        limits: Limits,
    ) -> Self {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> =
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
        let blobs: Arc<birchtree::media::blob_store::FsBlobStore> = Arc::new(
            birchtree::media::blob_store::FsBlobStore::open(dir.path().join("media"))
                .expect("open blob store"),
        );
        let server = Arc::new(ServerInfo::from_config(&cfg));
        let streams = Hub::new(store.clone(), clock.clone(), server.clone(), limits);
        let context = Arc::new(Context::new(
            clock,
            ids,
            store.clone(),
            blobs.clone(),
            Arc::new(streams.clone()),
        ));
        let federation = federation_config(context.clone(), TEST_PUBLIC_URL);
        let router = build_router(ApiState {
            context,
            trusted_proxies: Arc::new(Vec::new()),
            max_body_size: 16 * 1024 * 1024,
            server,
            streams: streams.clone(),
            federation,
            rate_limit_multiplier: 1,
            mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
            // Durable redb invites, like production (`main.rs`): the
            // harness's store is the invite store, so invite-gated
            // registration paths are exercisable end to end.
            invites: store.clone()
                as std::sync::Arc<dyn birchtree::application::accounts::InviteStore>,
        });
        Self {
            dir,
            store,
            blobs,
            streams,
            router,
        }
    }
}

/// Send a request through the router and return `(status, headers, body)`.
pub async fn send(router: &axum::Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body")
        .to_vec();
    (status, headers, body)
}

/// Build a form-encoded request, optionally carrying a bearer token.
pub fn form_request(
    method: &str,
    uri: &str,
    body: &str,
    token: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// Build a GET request with optional headers/cookie.
pub fn get_request(uri: &str, token: Option<&str>, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    builder.body(Body::empty()).expect("request")
}

/// Parse a JSON body, panicking on failure.
pub fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("valid json")
}

/// Extract the `birchtree_session` cookie value from a `Set-Cookie` header.
pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("set-cookie")?.to_str().ok()?;
    let first = raw.split(';').next()?;
    Some(first.to_string())
}

/// The crate's configuration for a test server. The crate spawns its queue
/// tasks when it is built, so it is built on a runtime of its own, on its own
/// thread: test servers are constructed from synchronous code, sometimes inside
/// a test runtime, where a nested `block_on` would panic.
#[allow(dead_code)]
pub fn federation_config(
    context: Arc<birchtree::application::context::Context>,
    public_url: &str,
) -> activitypub_federation::config::FederationConfig<birchtree::activitypub::FedContext> {
    federation_config_with(
        context,
        public_url,
        &birchtree::federation::net::SafeHttpConfig::default(),
        false,
    )
}

/// As [`federation_config`], with an explicit transport policy and the crate's debug switch.
/// Debug mode skips the crate's DNS-based production checks, so tests that name
/// a loopback target or an unresolvable host do not depend on the network.
#[allow(dead_code)]
pub fn federation_config_with(
    context: Arc<birchtree::application::context::Context>,
    public_url: &str,
    http: &birchtree::federation::net::SafeHttpConfig,
    debug: bool,
) -> activitypub_federation::config::FederationConfig<birchtree::activitypub::FedContext> {
    let public_url = public_url.to_string();
    let http = http.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("federation runtime")
            .block_on(birchtree::activitypub::build_config(
                context,
                &public_url,
                false,
                "Birchtree Test",
                "Test instance",
                &http,
                debug,
                None,
                false,
                "off",
                "off",
            ))
            .expect("federation config")
    })
    .join()
    .expect("federation config thread")
}

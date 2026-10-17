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
    pub router: axum::Router,
}

impl TestServer {
    /// Open a fresh server with the given registration policy.
    pub fn new(registration_open: bool) -> Self {
        let (dir, store) = temp_store();
        let server = Self::assemble(dir, store, registration_open);
        server
    }

    fn assemble(dir: tempfile::TempDir, store: Arc<RedbStore>, registration_open: bool) -> Self {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ids: Arc<dyn IdGenerator> =
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
        let context = Arc::new(Context::new(clock, ids, store.clone()));
        let cfg = Config {
            public_url: TEST_PUBLIC_URL.to_string(),
            registration_open,
            ..Config::default()
        };
        let server = Arc::new(ServerInfo::from_config(&cfg));
        let router = build_router(ApiState {
            context,
            trusted_proxies: Arc::new(Vec::new()),
            max_body_size: 16 * 1024 * 1024,
            server,
        });
        Self { dir, store, router }
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

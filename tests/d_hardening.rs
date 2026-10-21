//! Work packages 3, 8.2, 9 and 11 (D): client-IP trust and per-route rate
//! limits, production/lab mode, monotonic IDs across restarts, duplicate-ID
//! guards, and readiness.

mod common;

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use tower::ServiceExt;

use birchtree::api::mastodon::rate_limits::{self, RateLimitState, RateLimiter};
use birchtree::api::mastodon::router::build_router_with_probes;
use birchtree::api::mastodon::router::readiness::RuntimeProbes;
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::context::Context;
use birchtree::application::statuses::{create_status, CreateStatusCommand};
use birchtree::config::{Config, RuntimeMode};
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator};
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};

fn ip(text: &str) -> IpAddr {
    text.parse().expect("test address")
}

fn limited_router(state: RateLimitState) -> Router {
    Router::new()
        .route("/oauth/token", post(|| async { "ok" }))
        .route("/oauth/authorize", post(|| async { "ok" }))
        .route("/api/v1/emails/password_reset", post(|| async { "ok" }))
        .route("/api/v1/emails/confirmations", post(|| async { "ok" }))
        .route("/api/v1/other", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(state, rate_limits::middleware))
}

fn state_with(trusted: &[&str]) -> RateLimitState {
    let proxies: Vec<IpAddr> = trusted.iter().map(|t| ip(t)).collect();
    RateLimitState::new(Arc::new(proxies), 1)
}

async fn post_from(
    router: &Router,
    path: &str,
    peer: &str,
    forwarded_for: Option<&str>,
    body: &str,
) -> StatusCode {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(value) = forwarded_for {
        builder = builder.header("x-forwarded-for", value);
    }
    let mut request = builder.body(Body::from(body.to_string())).expect("request");
    let addr = SocketAddr::new(ip(peer), 40_000);
    request.extensions_mut().insert(ConnectInfo(addr));
    router
        .clone()
        .oneshot(request)
        .await
        .expect("response")
        .status()
}

#[tokio::test]
async fn spoofed_forwarded_for_from_untrusted_peer_cannot_mint_identities() {
    let router = limited_router(state_with(&[]));
    // 30 token requests per window per client; every request claims a fresh
    // forwarded identity, which must be ignored.
    for n in 0..30 {
        let spoof = format!("198.51.100.{n}");
        let status = post_from(&router, "/oauth/token", "203.0.113.9", Some(&spoof), "").await;
        assert_eq!(status, StatusCode::OK, "request {n}");
    }
    let blocked = post_from(&router, "/oauth/token", "203.0.113.9", Some("198.51.100.200"), "").await;
    assert_eq!(blocked, StatusCode::TOO_MANY_REQUESTS);
    // A genuinely different peer is unaffected.
    let other = post_from(&router, "/oauth/token", "203.0.113.10", None, "").await;
    assert_eq!(other, StatusCode::OK);
}

#[tokio::test]
async fn trusted_proxy_chain_resolves_the_real_client() {
    let router = limited_router(state_with(&["10.0.0.1", "10.0.0.2"]));
    // Client A (behind two proxies) exhausts its login budget.
    for n in 0..10 {
        let status = post_from(
            &router,
            "/oauth/authorize",
            "10.0.0.2",
            Some("198.51.100.7, 10.0.0.1"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "request {n}");
    }
    let blocked = post_from(
        &router,
        "/oauth/authorize",
        "10.0.0.2",
        Some("198.51.100.7, 10.0.0.1"),
        "",
    )
    .await;
    assert_eq!(blocked, StatusCode::TOO_MANY_REQUESTS);
    // Client A cannot dodge the limit by prepending a forged leading entry.
    let forged = post_from(
        &router,
        "/oauth/authorize",
        "10.0.0.2",
        Some("9.9.9.9, 198.51.100.7, 10.0.0.1"),
        "",
    )
    .await;
    assert_eq!(forged, StatusCode::TOO_MANY_REQUESTS);
    // Client B behind the same proxies has its own window.
    let other = post_from(
        &router,
        "/oauth/authorize",
        "10.0.0.2",
        Some("198.51.100.8, 10.0.0.1"),
        "",
    )
    .await;
    assert_eq!(other, StatusCode::OK);
}

#[tokio::test]
async fn flooding_identities_does_not_reset_other_users_windows() {
    let limiter = Arc::new(RateLimiter::with_capacity(16));
    let state = RateLimitState::with_limiter(limiter.clone(), Arc::new(Vec::new()), 1);
    let router = limited_router(state);
    for _ in 0..10 {
        post_from(&router, "/oauth/authorize", "203.0.113.50", None, "").await;
    }
    assert_eq!(
        post_from(&router, "/oauth/authorize", "203.0.113.50", None, "").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    for n in 0..300u32 {
        let peer = format!("100.64.{}.{}", n / 250, n % 250 + 1);
        post_from(&router, "/oauth/authorize", &peer, None, "").await;
    }
    assert!(limiter.stats().tracked_keys <= 16, "{:?}", limiter.stats());
    assert_eq!(
        post_from(&router, "/oauth/authorize", "203.0.113.50", None, "").await,
        StatusCode::TOO_MANY_REQUESTS,
        "the flooded table must not have cleared the existing window"
    );
}

#[tokio::test]
async fn routes_have_independent_thresholds() {
    let router = limited_router(state_with(&[]));
    for _ in 0..10 {
        post_from(&router, "/oauth/authorize", "203.0.113.20", None, "").await;
    }
    assert_eq!(
        post_from(&router, "/oauth/authorize", "203.0.113.20", None, "").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Token issuance and the general budget are separate windows.
    assert_eq!(
        post_from(&router, "/oauth/token", "203.0.113.20", None, "").await,
        StatusCode::OK
    );
    let mut request = Request::builder()
        .uri("/api/v1/other")
        .body(Body::empty())
        .expect("request");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip("203.0.113.20"), 1)));
    let response = router.clone().oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-ratelimit-remaining"));
}

#[tokio::test]
async fn limited_responses_carry_retry_after_and_error_body() {
    let router = limited_router(state_with(&[]));
    for _ in 0..10 {
        post_from(&router, "/oauth/authorize", "203.0.113.21", None, "").await;
    }
    let mut request = Request::builder()
        .method("POST")
        .uri("/oauth/authorize")
        .body(Body::empty())
        .expect("request");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip("203.0.113.21"), 1)));
    let response = router.clone().oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key("retry-after"));
    let bytes = axum::body::to_bytes(response.into_body(), 4096).await.expect("body");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")["error"],
        "Too many requests"
    );
}

#[tokio::test]
async fn password_reset_is_limited_per_address_across_sources() {
    let router = limited_router(state_with(&[]));
    // Three requests for one address from three different sources pass...
    for n in 0..3 {
        let peer = format!("203.0.113.{}", 100 + n);
        let status = post_from(
            &router,
            "/api/v1/emails/password_reset",
            &peer,
            None,
            "email=Target%40Example.com",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "request {n}");
    }
    // ...the fourth, from a fourth source and with different letter case, is
    // refused with the same 429 an unknown address would get.
    let blocked = post_from(
        &router,
        "/api/v1/emails/password_reset",
        "203.0.113.150",
        None,
        "email=target%40example.com",
    )
    .await;
    assert_eq!(blocked, StatusCode::TOO_MANY_REQUESTS);
    // A different address from the same fresh source still works.
    let other = post_from(
        &router,
        "/api/v1/emails/password_reset",
        "203.0.113.150",
        None,
        "email=someone-else%40example.com",
    )
    .await;
    assert_eq!(other, StatusCode::OK);
    // The address window is shared with JSON bodies of the same route.
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/v1/emails/password_reset")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"email":" TARGET@example.com "}"#))
        .expect("request");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip("203.0.113.151"), 1)));
    let json_status = router.clone().oneshot(request).await.expect("response").status();
    assert_eq!(json_status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn confirmation_resend_has_its_own_address_window() {
    let router = limited_router(state_with(&[]));
    for n in 0..3 {
        let peer = format!("203.0.113.{}", 160 + n);
        post_from(
            &router,
            "/api/v1/emails/password_reset",
            &peer,
            None,
            "email=shared%40example.com",
        )
        .await;
    }
    let resend = post_from(
        &router,
        "/api/v1/emails/confirmations",
        "203.0.113.170",
        None,
        "email=shared%40example.com",
    )
    .await;
    assert_eq!(resend, StatusCode::OK);
}

#[test]
fn production_refuses_dangerous_flags_and_lab_allows_them() {
    let hardened = Config {
        transcoding_enabled: false,
        ..Config::default()
    };
    assert!(hardened.check_runtime_mode().is_ok());
    let dangerous = [
        Config {
            federation_allow_http: true,
            ..hardened.clone()
        },
        Config {
            rate_limit_multiplier: 5,
            ..hardened.clone()
        },
        Config {
            federation_allowlist: birchtree::federation::policy::parse_allowlist("10.0.0.0/8").expect("cidr"),
            ..hardened.clone()
        },
    ];
    for cfg in &dangerous {
        assert!(cfg.check_runtime_mode().is_err());
        let lab = Config {
            mode: RuntimeMode::Lab,
            ..cfg.clone()
        };
        assert!(lab.check_runtime_mode().is_ok());
        assert!(
            lab.startup_summary()
                .warnings
                .iter()
                .any(|w| w.contains("lab mode"))
        );
    }
}

fn open_with(dir: &std::path::Path, ids: Arc<dyn IdGenerator>) -> RedbStore {
    RedbStore::open(dir.join("birchtree.redb"), Arc::new(SystemClock), ids).expect("open store")
}

fn post_status(store: &RedbStore, account: Id, text: &str) -> birchtree::domain::Result<Id> {
    create_status(
        store,
        "https://example.social",
        common::T0,
        CreateStatusCommand {
            account_id: account,
            text: Some(text.to_string()),
            in_reply_to_id: None,
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: None,
            language: None,
            media_ids: Vec::new(),
            poll: None,
            application_name: None,
            application_website: None,
            idempotency_key: None,
        },
        500,
    )
    .map(|created| created.status.id)
}

/// Clock fixed in the past, to simulate a rolled-back or wrong wall clock.
struct FixedClock(u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

#[test]
fn restart_with_older_clock_continues_above_persisted_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    // First run: clock far in the future relative to the second run.
    let first_ids = Arc::new(TimestampIdGenerator::new(Box::new(FixedClock(
        4_000_000_000_000,
    ))));
    let store = open_with(dir.path(), first_ids);
    let alice = common::mk_account(&store, "alice").id;
    let mut highest = post_status(&store, alice, "one").expect("first status");
    highest = highest.max(post_status(&store, alice, "two").expect("second status"));
    drop(store);

    // Restart: the wall clock is behind everything on disk (rollback or an
    // old backup restored). Without seeding this would reuse old IDs.
    let second_ids = Arc::new(TimestampIdGenerator::new(Box::new(FixedClock(
        1_000_000_000_000,
    ))));
    let store = open_with(dir.path(), second_ids.clone());
    let persisted = store.highest_persisted_id().expect("scan highest id");
    assert!(persisted >= highest, "{persisted} < {highest}");
    let skew_ms = second_ids.observe(persisted);
    assert!(skew_ms > 0, "restored data should report clock skew");
    let next = post_status(&store, alice, "three").expect("status after restart");
    assert!(next > persisted, "{next} must exceed {persisted}");
}

/// Hands out a scripted sequence so a collision can be forced.
struct ReplayIds(AtomicU64);

impl IdGenerator for ReplayIds {
    fn generate(&self) -> Id {
        Id::new(self.0.fetch_add(1, Ordering::SeqCst))
    }
}

#[test]
fn duplicate_generated_status_id_is_refused_not_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ids = Arc::new(ReplayIds(AtomicU64::new(1_000)));
    let store = open_with(dir.path(), ids.clone());
    let account = common::mk_account(&store, "bob");
    let original = post_status(&store, account.id, "original").expect("first status");

    // Rewind the generator so the next status would reuse the same ID.
    ids.0.store(original.value(), Ordering::SeqCst);
    let collision = post_status(&store, account.id, "impostor");
    assert!(collision.is_err(), "a duplicate ID must not succeed");

    let stored = birchtree::application::ports::StatusStore::status_by_id(&store, original)
        .expect("read")
        .expect("original still present");
    assert_eq!(stored.source_text, "original");
}

#[test]
fn concurrent_generation_across_threads_is_unique() {
    let ids = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let ids = ids.clone();
        handles.push(std::thread::spawn(move || {
            (0..2_000).map(|_| ids.generate()).collect::<Vec<Id>>()
        }));
    }
    let mut seen = std::collections::HashSet::new();
    for handle in handles {
        for id in handle.join().expect("thread") {
            assert!(seen.insert(id), "duplicate {id}");
        }
    }
}

fn api_state(dir: &std::path::Path) -> ApiState {
    let store = Arc::new(open_with(
        dir,
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
    ));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blob store"),
    );
    let cfg = Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        transcoding_enabled: false,
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
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 1024 * 1024,
        server,
        streams,
        federation,
        rate_limit_multiplier: 1,
        mail_env: Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: store as Arc<dyn birchtree::application::accounts::InviteStore>,
    }
}

async fn get_text(router: &Router, path: &str, loopback_peer: bool) -> (StatusCode, String) {
    let mut request = Request::builder().uri(path).body(Body::empty()).expect("request");
    if loopback_peer {
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip("127.0.0.1"), 5)));
    }
    let response = router.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn ready_reflects_registered_probes_without_leaking_details() {
    let dir = tempfile::tempdir().expect("tempdir");
    let worker_alive = Arc::new(AtomicBool::new(true));
    let alive = worker_alive.clone();
    let probes = RuntimeProbes::new()
        .with_check("worker_progress", move || alive.load(Ordering::SeqCst))
        .with_check("media_pool_durable_secret_path", || true)
        .with_gauge("job_queue_backlog", || 7);
    let router = build_router_with_probes(api_state(dir.path()), probes);

    let (status, body) = get_text(&router, "/ready", false).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "ready"));

    worker_alive.store(false, Ordering::SeqCst);
    let (status, body) = get_text(&router, "/ready", false).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, "not ready");
    assert!(!body.contains("worker"), "failing check names must not leak");

    // Liveness is unaffected by readiness failures.
    let (status, _) = get_text(&router, "/health", false).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn metrics_are_loopback_only_and_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let probes = RuntimeProbes::new().with_gauge("job_queue_backlog", || 7);
    let router = build_router_with_probes(api_state(dir.path()), probes);

    let (remote_status, _) = get_text(&router, "/metrics", false).await;
    assert_eq!(remote_status, StatusCode::NOT_FOUND);

    let (status, body) = get_text(&router, "/metrics", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("birchtree_job_queue_backlog 7"), "{body}");
    assert!(body.contains("birchtree_rate_limit_denied_total"), "{body}");
    assert!(body.lines().count() < 64, "metric set must stay small");
}

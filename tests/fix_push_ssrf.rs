//! Web Push SSRF hardening tests (P1 fix).
//!
//! An authenticated user can register any push endpoint URL, so every URL
//! the server dials on notification delivery is untrusted. These tests lock
//! the three enforcement points in `application::push_client` and the
//! delivery loop in `application::push`:
//!
//! - write-time validation refuses prohibited targets (loopback, private,
//!   link-local/cloud-metadata, CGNAT, unique-local IPv6 and the other
//!   special-purpose ranges) and requires HTTPS;
//! - delivery revalidates the initial URL and every redirect target, follows
//!   at most a bounded number of redirects, and strips the VAPID
//!   `Authorization` header on cross-origin hops;
//! - DNS answers are judged as a whole, so one prohibited address among
//!   public ones refuses the endpoint (rebinding defence).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use birchtree::application::push;
use birchtree::application::push_client;
use birchtree::application::push_client::PushClientConfig;
use birchtree::domain::Id;
use birchtree::domain::WebPushSubscription;
use birchtree::domain::conversation::validate_push_endpoint;

/// Generate a fresh VAPID pair (same construction as the `ring`-only unit
/// tests in `application::push`: the scalar and point are carved out of a
/// generated PKCS#8 document by their unique DER markers).
fn test_vapid_keys() -> push::VapidKeys {
    use base64::Engine as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .expect("generate keypair");
    let der = doc.as_ref();
    let priv_off = der
        .windows(2)
        .position(|w| *w == [0x04, 0x20])
        .expect("private marker");
    let pub_off = der
        .windows(4)
        .position(|w| *w == [0x03, 0x42, 0x00, 0x04])
        .expect("public marker");
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    push::VapidKeys {
        public_b64: enc.encode(&der[pub_off + 3..pub_off + 3 + 65]),
        private_b64: enc.encode(&der[priv_off + 2..priv_off + 34]),
    }
}

fn tickle(endpoint: &str, keys: &push::VapidKeys) -> push::PushRequest {
    let subscription = WebPushSubscription {
        id: Id::new(1),
        token_id: Id::new(2),
        user_id: Id::new(3),
        endpoint: endpoint.to_string(),
        key_p256dh: String::new(),
        key_auth: String::new(),
        standard: false,
        policy: "all".to_string(),
        alerts: HashMap::new(),
        unsubscribe_token: String::new(),
        unsubscribe_url: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    push::build_push_request(
        &subscription,
        keys,
        "mailto:admin@example.social",
        1_700_000_000_000,
    )
    .expect("tickle builds")
}

/// Like [`tickle`] but with an `Unsubscribe-URL` set (upstream sends it on
/// every standard + legacy request once the subscription minted a token).
/// Cross-origin redirect tests use this to prove the header is dropped with
/// `Authorization`; same-origin tests prove both are kept.
fn tickle_with_unsubscribe(endpoint: &str, keys: &push::VapidKeys) -> push::PushRequest {
    let mut request = tickle(endpoint, keys);
    request.unsubscribe_url = Some("https://example.social/push/unsubscribe/abc".to_string());
    request
}

fn lab_config() -> PushClientConfig {
    PushClientConfig::lab_loopback()
}

fn lab_client() -> reqwest::Client {
    push_client::build_push_client(&lab_config()).expect("lab client builds")
}

/// The default-deny policy refuses every prohibited literal without any
/// network access (IP literals are judged directly, no DNS involved).
#[test]
fn default_policy_rejects_prohibited_literals() {
    let config = PushClientConfig::default();
    for literal in [
        "https://127.0.0.1/",
        "https://127.5.6.7/",
        "https://[::1]/",
        "https://10.0.0.5/",
        "https://192.168.1.1/",
        "https://172.16.0.1/",
        "https://172.31.255.255/",
        "https://169.254.169.254/latest/meta-data/",
        "https://0.0.0.0/",
        "https://[fe80::1]/",
        "https://[fc00::1]/",
        "https://[fd12::1]/",
        "https://[ff02::1]/",
        "https://100.64.0.1/",
        "https://192.0.2.1/",
        "https://198.51.100.1/",
        "https://203.0.113.1/",
        "https://224.0.0.1/",
    ] {
        assert!(
            push_client::validate_push_url(literal, &config).is_err(),
            "{literal} must be refused"
        );
    }
    assert!(push_client::validate_push_url("https://push.example/sub", &config).is_ok());
}

/// HTTPS is required; credentials are refused; the lab config is the only
/// path that admits plain HTTP (and only its shape — delivery still checks
/// addresses).
#[test]
fn https_is_required_by_default() {
    let config = PushClientConfig::default();
    assert!(push_client::validate_push_url("https://push.example/sub", &config).is_ok());
    assert!(push_client::validate_push_url("http://push.example/sub", &config).is_err());
    assert!(push_client::validate_push_url("https://user:pw@push.example/", &config).is_err());
    assert!(validate_push_endpoint("http://push.example/sub").is_err());
    assert!(validate_push_endpoint("https://user:pw@push.example/").is_err());
    assert!(validate_push_endpoint("https://push.example/sub").is_ok());
    let lab = lab_config();
    assert!(push_client::validate_push_url("http://127.0.0.1:1/sub", &lab).is_ok());
}

/// Subscription-time validation resolves the host: loopback literals and
/// loopback-resolving names are refused, while a name that does not resolve
/// at all is accepted (with no addresses nothing can be dialled; delivery
/// revalidates with fresh DNS before every send).
#[tokio::test]
async fn subscription_validation_resolves_before_accepting() {
    let config = PushClientConfig::default();
    assert!(
        push_client::validate_endpoint_for_subscription("https://127.0.0.1:8443/sub", &config)
            .await
            .is_err()
    );
    assert!(
        push_client::validate_endpoint_for_subscription("https://[::1]/sub", &config)
            .await
            .is_err()
    );
    assert!(
        push_client::validate_endpoint_for_subscription("https://localhost/sub", &config)
            .await
            .is_err()
    );
    assert!(
        push_client::validate_endpoint_for_subscription(
            "https://push-endpoint.invalid/sub",
            &config
        )
        .await
        .is_ok()
    );
}

/// A DNS answer is refused as a whole when any address it contains is
/// prohibited: a rebinding name that alternates public and private answers
/// can never be registered or dialled.
#[test]
fn mixed_dns_answers_are_refused() {
    let public: std::net::IpAddr = "8.8.8.8".parse().expect("public");
    let also_public: std::net::IpAddr = "1.1.1.1".parse().expect("public");
    let metadata: std::net::IpAddr = "169.254.169.254".parse().expect("metadata");
    let mapped_loopback: std::net::IpAddr = "::ffff:127.0.0.1".parse().expect("mapped");
    assert!(push_client::check_resolved_addrs(&[public, also_public], &[]).is_ok());
    assert!(push_client::check_resolved_addrs(&[public, metadata], &[]).is_err());
    assert!(push_client::check_resolved_addrs(&[mapped_loopback], &[]).is_err());
    assert!(push_client::check_resolved_addrs(&[], &[]).is_err());
}

/// Origin comparison treats an explicit default port as the implicit one
/// but distinguishes any other port, scheme or host.
#[test]
fn origin_comparison_ignores_only_default_ports() {
    let url = url::Url::parse("https://push.example:443/sub").expect("url");
    assert!(push_client::same_origin("https://push.example", &url));
    assert!(!push_client::same_origin("https://push.example:8443", &url));
    assert!(!push_client::same_origin("http://push.example", &url));
    assert!(!push_client::same_origin("https://other.example", &url));
    let custom = url::Url::parse("https://push.example:8443/sub").expect("url");
    assert!(!push_client::same_origin("https://push.example", &custom));
    assert!(push_client::same_origin(
        "https://push.example:8443",
        &custom
    ));
}

// ---------------------------------------------------------------------------
// Live-server delivery tests (loopback only, under the lab policy).
// ---------------------------------------------------------------------------

/// What a fake push endpoint observed.
#[derive(Debug, Default, Clone)]
struct Captured {
    method: String,
    authorization: Option<String>,
    crypto_key: Option<String>,
    ttl: Option<String>,
    unsubscribe_url: Option<String>,
}

fn slot() -> Arc<Mutex<Option<Captured>>> {
    Arc::new(Mutex::new(None))
}

fn record(slot: &Arc<Mutex<Option<Captured>>>, headers: &axum::http::HeaderMap, method: &str) {
    let mut guard = slot.lock().expect("capture");
    *guard = Some(Captured {
        method: method.to_string(),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        crypto_key: headers
            .get("crypto-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        ttl: headers
            .get("ttl")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        unsubscribe_url: headers
            .get("unsubscribe-url")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    });
}

/// Serve `router` on loopback; returns its address.
async fn serve(router: axum::Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, router.into_make_service())
            .await
            .expect("serve");
    });
    (addr, handle)
}

/// The lab policy delivers to a loopback endpoint with the VAPID headers.
#[tokio::test]
async fn lab_delivery_sends_vapid_headers() {
    let seen = slot();
    let seen_reader = seen.clone();
    let router = axum::Router::new().route(
        "/push",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let seen = seen.clone();
            async move {
                record(&seen, &headers, "POST");
                "ok"
            }
        }),
    );
    let (addr, handle) = serve(router).await;

    let keys = test_vapid_keys();
    let request = tickle(&format!("http://{addr}/push"), &keys);
    let outcome = push::deliver_push_with_policy(&lab_client(), &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Delivered);
    let captured = seen_reader
        .lock()
        .expect("capture")
        .clone()
        .expect("captured");
    assert_eq!(captured.method, "POST");
    let authorization = captured.authorization.expect("authorization sent");
    assert!(authorization.starts_with("vapid t="), "{authorization}");
    assert!(
        captured
            .crypto_key
            .expect("crypto-key sent")
            .starts_with("p256ecdsa=")
    );
    assert_eq!(captured.ttl.as_deref(), Some("172800"));
    handle.abort();
}

/// The production default-deny policy refuses a loopback endpoint before
/// any connection: the same server the lab policy delivers to is unreachable
/// by default.
#[tokio::test]
async fn default_policy_refuses_loopback_delivery() {
    let router = axum::Router::new().route("/push", axum::routing::post(async || "ok"));
    let (addr, handle) = serve(router).await;

    let keys = test_vapid_keys();
    let request = tickle(&format!("http://{addr}/push"), &keys);
    let outcome = push::deliver_push(&reqwest::Client::new(), &request).await;
    assert!(matches!(outcome, push::PushOutcome::Gone), "{outcome:?}");
    handle.abort();
}

/// Same-origin redirects (including relative `Location` values) keep the
/// VAPID headers and `Unsubscribe-URL`, and re-POST.
#[tokio::test]
async fn same_origin_redirect_keeps_authorization() {
    let seen = slot();
    let seen_reader = seen.clone();
    let router = axum::Router::new()
        .route(
            "/old",
            axum::routing::post(async || {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, "/new")],
                    "",
                )
            }),
        )
        .route(
            "/new",
            axum::routing::post(
                move |method: axum::http::Method, headers: axum::http::HeaderMap| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, &headers, method.as_str());
                        "ok"
                    }
                },
            ),
        );
    let (addr, handle) = serve(router).await;

    let keys = test_vapid_keys();
    let request = tickle_with_unsubscribe(&format!("http://{addr}/old"), &keys);
    let outcome = push::deliver_push_with_policy(&lab_client(), &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Delivered);
    let captured = seen_reader
        .lock()
        .expect("capture")
        .clone()
        .expect("captured");
    assert_eq!(captured.method, "POST");
    assert!(
        captured
            .authorization
            .is_some_and(|v| v.starts_with("vapid t="))
    );
    assert_eq!(
        captured.unsubscribe_url.as_deref(),
        Some("https://example.social/push/unsubscribe/abc")
    );
    handle.abort();
}

/// A cross-origin redirect strips `Authorization` (the VAPID bearer
/// credential) and `Unsubscribe-URL` (the subscription-management URL)
/// while still delivering when the target validates.
#[tokio::test]
async fn cross_origin_redirect_strips_authorization() {
    let seen_b = slot();
    let seen_b_reader = seen_b.clone();
    let router_b = axum::Router::new().route(
        "/target",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let seen_b = seen_b.clone();
            async move {
                record(&seen_b, &headers, "POST");
                "ok"
            }
        }),
    );
    let (addr_b, handle_b) = serve(router_b).await;

    let location = format!("http://{addr_b}/target");
    let seen_a = slot();
    let seen_a_reader = seen_a.clone();
    let router_a = axum::Router::new().route(
        "/push",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let seen_a = seen_a.clone();
            async move {
                record(&seen_a, &headers, "POST");
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, location.clone())],
                    "",
                )
            }
        }),
    );
    let (addr_a, handle_a) = serve(router_a).await;

    let keys = test_vapid_keys();
    let request = tickle_with_unsubscribe(&format!("http://{addr_a}/push"), &keys);
    let outcome = push::deliver_push_with_policy(&lab_client(), &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Delivered);
    let first = seen_a_reader
        .lock()
        .expect("capture")
        .clone()
        .expect("first hop");
    assert!(
        first
            .authorization
            .is_some_and(|v| v.starts_with("vapid t="))
    );
    assert_eq!(
        first.unsubscribe_url.as_deref(),
        Some("https://example.social/push/unsubscribe/abc")
    );
    let second = seen_b_reader
        .lock()
        .expect("capture")
        .clone()
        .expect("second hop");
    assert_eq!(second.authorization, None);
    assert_eq!(second.unsubscribe_url, None);
    handle_a.abort();
    handle_b.abort();
}

/// A redirect to a prohibited address settles as gone: the subscription can
/// never deliver and may be weaponised, so it is deleted.
#[tokio::test]
async fn redirect_to_prohibited_target_is_gone() {
    let router = axum::Router::new().route(
        "/push",
        axum::routing::post(async || {
            (
                axum::http::StatusCode::FOUND,
                [(axum::http::header::LOCATION, "http://169.254.169.254/")],
                "",
            )
        }),
    );
    let (addr, handle) = serve(router).await;

    let keys = test_vapid_keys();
    let request = tickle(&format!("http://{addr}/push"), &keys);
    let outcome = push::deliver_push_with_policy(&lab_client(), &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Gone);
    handle.abort();
}

/// A redirect cycle is bounded: the attempt retries instead of looping.
#[tokio::test]
async fn redirect_loop_is_bounded() {
    let router = axum::Router::new().route(
        "/loop",
        axum::routing::post(async || {
            (
                axum::http::StatusCode::FOUND,
                [(axum::http::header::LOCATION, "/loop")],
                "",
            )
        }),
    );
    let (addr, handle) = serve(router).await;

    let keys = test_vapid_keys();
    let request = tickle(&format!("http://{addr}/loop"), &keys);
    let outcome = push::deliver_push_with_policy(&lab_client(), &request, &lab_config()).await;
    assert!(
        matches!(outcome, push::PushOutcome::Retry(ref message) if message.contains("too many")),
        "{outcome:?}"
    );
    handle.abort();
}

/// A prohibited literal never reaches the network at delivery time either.
#[tokio::test]
async fn delivery_to_prohibited_literal_is_gone() {
    let keys = test_vapid_keys();
    let request = tickle("https://169.254.169.254/sub", &keys);
    let outcome = push::deliver_push_with_policy(&lab_client(), &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Gone);
}

// ---------------------------------------------------------------------------
// Production-constructor tests: the same client `main.rs` hands the worker.
// ---------------------------------------------------------------------------

/// The production constructor builds under the default-deny policy. A
/// loopback endpoint is refused before any connection: the server observes
/// zero requests, so nothing private is dialed.
#[tokio::test]
async fn production_client_does_not_dial_loopback() {
    let seen = slot();
    let seen_reader = seen.clone();
    let router = axum::Router::new().route(
        "/push",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let seen = seen.clone();
            async move {
                record(&seen, &headers, "POST");
                "ok"
            }
        }),
    );
    let (addr, handle) = serve(router).await;

    let client = push_client::production_push_client().expect("production client builds");
    let keys = test_vapid_keys();
    let request = tickle(&format!("http://{addr}/push"), &keys);
    let outcome =
        push::deliver_push_with_policy(&client, &request, &PushClientConfig::default()).await;
    assert!(matches!(outcome, push::PushOutcome::Gone), "{outcome:?}");
    assert!(
        seen_reader.lock().expect("capture").is_none(),
        "production client must not dial the loopback endpoint"
    );
    handle.abort();
}

/// The production-built client follows redirects manually under policy, not
/// via reqwest: a loopback endpoint that 302s to a private literal settles
/// as gone (a client with automatic redirects would attempt the private
/// target and settle as a network retry instead).
#[tokio::test]
async fn production_client_redirect_to_private_is_gone_not_dialed() {
    let router = axum::Router::new().route(
        "/push",
        axum::routing::post(async || {
            (
                axum::http::StatusCode::FOUND,
                [(axum::http::header::LOCATION, "http://169.254.169.254/")],
                "",
            )
        }),
    );
    let (addr, handle) = serve(router).await;

    // The lab policy lets the first (loopback) hop through so the redirect
    // hop is actually exercised; the client itself is the production build
    // (`Policy::none` via `net::client`), which is what this test proves.
    let client = push_client::production_push_client().expect("production client builds");
    let keys = test_vapid_keys();
    let request = tickle(&format!("http://{addr}/push"), &keys);
    let outcome = push::deliver_push_with_policy(&client, &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Gone);
    handle.abort();
}

/// The production-built client drops both `Authorization` and
/// `Unsubscribe-URL` on a cross-origin hop: reqwest-level automatic
/// redirects would forward the custom `Unsubscribe-URL` header, so its
/// absence proves redirects are followed manually (`Policy::none`).
#[tokio::test]
async fn production_client_cross_origin_drops_both_headers() {
    let seen_b = slot();
    let seen_b_reader = seen_b.clone();
    let router_b = axum::Router::new().route(
        "/target",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let seen_b = seen_b.clone();
            async move {
                record(&seen_b, &headers, "POST");
                "ok"
            }
        }),
    );
    let (addr_b, handle_b) = serve(router_b).await;

    let location = format!("http://{addr_b}/target");
    let router_a = axum::Router::new().route(
        "/push",
        axum::routing::post(move || {
            let location = location.clone();
            async move {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, location)],
                    "",
                )
            }
        }),
    );
    let (addr_a, handle_a) = serve(router_a).await;

    let client = push_client::production_push_client().expect("production client builds");
    let keys = test_vapid_keys();
    let request = tickle_with_unsubscribe(&format!("http://{addr_a}/push"), &keys);
    let outcome = push::deliver_push_with_policy(&client, &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Delivered);
    let second = seen_b_reader
        .lock()
        .expect("capture")
        .clone()
        .expect("second hop");
    assert_eq!(second.authorization, None);
    assert_eq!(second.unsubscribe_url, None);
    handle_a.abort();
    handle_b.abort();
}

/// The production-built client keeps both headers on a same-origin hop.
#[tokio::test]
async fn production_client_same_origin_keeps_both_headers() {
    let seen = slot();
    let seen_reader = seen.clone();
    let router = axum::Router::new()
        .route(
            "/old",
            axum::routing::post(async || {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, "/new")],
                    "",
                )
            }),
        )
        .route(
            "/new",
            axum::routing::post(
                move |method: axum::http::Method, headers: axum::http::HeaderMap| {
                    let seen = seen.clone();
                    async move {
                        record(&seen, &headers, method.as_str());
                        "ok"
                    }
                },
            ),
        );
    let (addr, handle) = serve(router).await;

    let client = push_client::production_push_client().expect("production client builds");
    let keys = test_vapid_keys();
    let request = tickle_with_unsubscribe(&format!("http://{addr}/old"), &keys);
    let outcome = push::deliver_push_with_policy(&client, &request, &lab_config()).await;
    assert_eq!(outcome, push::PushOutcome::Delivered);
    let captured = seen_reader
        .lock()
        .expect("capture")
        .clone()
        .expect("captured");
    assert!(
        captured
            .authorization
            .is_some_and(|v| v.starts_with("vapid t="))
    );
    assert_eq!(
        captured.unsubscribe_url.as_deref(),
        Some("https://example.social/push/unsubscribe/abc")
    );
    handle.abort();
}

//! Phase 7 (Subagent A) contract tests: conversations, conversation mutes and
//! Web Push.
//!
//! Driven through the real router (routes wired by the parent patch), plus a
//! delivery-job unit test against a fake push endpoint (no real network).

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{Request, StatusCode};
use common::{T0, TestServer, form_request, get_request, json, send};
use serde_json::Value;

use birchtree::application::auth;
use birchtree::application::ports::{AccountStore, JobQueue, NotificationStore};
use birchtree::application::push as push_app;

const WAIT: Duration = Duration::from_secs(5);

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

fn encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", enc(key), enc(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A user token with exactly `scopes` (direct store issuance, as the
/// streaming tests do).
fn token(srv: &TestServer, username: &str, scopes: &str) -> String {
    let account = match srv
        .store
        .account_by_handle(username, "example.social")
        .expect("store")
    {
        Some(account) => account,
        None => common::mk_account(&srv.store, username),
    };
    let app = auth::register_app(
        &*srv.store,
        format!("Phase7 {username}"),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .expect("register app");
    let scopes: Vec<String> = scopes.split_whitespace().map(str::to_string).collect();
    let (plaintext, _) =
        auth::issue_user_token(&*srv.store, app.app.id, account.id, scopes, T0).expect("token");
    plaintext
}

/// Call the REST surface; returns `(status, body)`.
async fn call(
    srv: &TestServer,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let request = match method {
        "GET" => get_request(path, token, None),
        _ => form_request(method, path, body, token),
    };
    let (status, _headers, bytes) = send(&srv.router, request).await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn ok(srv: &TestServer, method: &str, path: &str, token: &str, body: &str) -> Value {
    let (status, value) = call(srv, method, path, Some(token), body).await;
    assert_eq!(status, StatusCode::OK, "{method} {path}: {value}");
    value
}

/// Post a direct status mentioning `@mention`; returns the status id.
async fn dm(srv: &TestServer, token: &str, mention: &str, text: &str) -> String {
    let body = encode(&[
        ("status", &format!("@{mention} {text}")),
        ("visibility", "direct"),
    ]);
    let value = ok(srv, "POST", "/api/v1/statuses", token, &body).await;
    assert_eq!(value["visibility"], Value::String("direct".into()));
    value["id"].as_str().unwrap().to_string()
}

fn dm_link_params(link: Option<&str>) -> String {
    link.unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------
// Auth and scopes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversations_require_auth_and_scopes() {
    let srv = TestServer::new(true);
    let full = token(
        &srv,
        "alice",
        "read read:statuses write write:conversations write:mutes write:statuses",
    );
    let read_only = token(&srv, "bob", "read read:statuses");

    // No token: 401.
    let (status, _) = call(&srv, "GET", "/api/v1/conversations", None, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Read-only token on a write route: 403.
    let (status, value) = call(
        &srv,
        "POST",
        "/api/v1/conversations/123/read",
        Some(&read_only),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{value}");

    // Unknown conversation: 404 with the explicit body.
    let (status, value) = call(
        &srv,
        "POST",
        "/api/v1/conversations/123456789/read",
        Some(&full),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"], Value::String("Not Found".into()));

    // Non-numeric id: same 404.
    let (status, value) = call(
        &srv,
        "DELETE",
        "/api/v1/conversations/abc",
        Some(&full),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"], Value::String("Not Found".into()));
}

// ---------------------------------------------------------------------------
// Conversations: list, read, unread, delete.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversations_list_read_unread_delete() {
    let srv = TestServer::new(true);
    let scopes = "read read:statuses write write:conversations write:statuses";
    let alice = token(&srv, "alice", scopes);
    let bob = token(&srv, "bob", scopes);

    // Empty for both.
    let (status, value) = call(&srv, "GET", "/api/v1/conversations", Some(&bob), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, Value::Array(Vec::new()));

    let dm_id = dm(&srv, &alice, "bob", "hello bob").await;

    // Bob: one conversation, unread, alice as participant.
    let value = ok(&srv, "GET", "/api/v1/conversations", &bob, "").await;
    let items = value.as_array().unwrap();
    assert_eq!(items.len(), 1);
    let conv = &items[0];
    assert_eq!(
        {
            let mut keys: Vec<&str> = conv
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys
        },
        ["accounts", "id", "last_status", "unread"]
    );
    assert_eq!(conv["id"].as_str().unwrap(), dm_id);
    assert_eq!(conv["unread"], Value::Bool(true));
    assert_eq!(conv["last_status"]["id"].as_str().unwrap(), dm_id);
    let accts = conv["accounts"].as_array().unwrap();
    assert_eq!(accts.len(), 1);
    assert_eq!(accts[0]["username"], Value::String("alice".into()));

    // Alice (author): same thread, read.
    let value = ok(&srv, "GET", "/api/v1/conversations", &alice, "").await;
    let items = value.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["unread"], Value::Bool(false));

    // Read is idempotent; unread forces the flag; read clears it again.
    for _ in 0..2 {
        let value = ok(&srv, "POST", &format!("/api/v1/conversations/{dm_id}/read"), &bob, "").await;
        assert_eq!(value["unread"], Value::Bool(false));
    }
    for _ in 0..2 {
        let value = ok(&srv, "POST", &format!("/api/v1/conversations/{dm_id}/unread"), &bob, "").await;
        assert_eq!(value["unread"], Value::Bool(true));
    }
    let value = ok(&srv, "POST", &format!("/api/v1/conversations/{dm_id}/read"), &bob, "").await;
    assert_eq!(value["unread"], Value::Bool(false));

    // Delete removes the viewer's participation only.
    let (status, value) = call(
        &srv,
        "DELETE",
        &format!("/api/v1/conversations/{dm_id}"),
        Some(&bob),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value, serde_json::json!({}));
    let (_, value) = call(&srv, "GET", "/api/v1/conversations", Some(&bob), "").await;
    assert_eq!(value, Value::Array(Vec::new()));
    // A second delete 404s; alice still sees her side.
    let (status, _) = call(
        &srv,
        "DELETE",
        &format!("/api/v1/conversations/{dm_id}"),
        Some(&bob),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let value = ok(&srv, "GET", "/api/v1/conversations", &alice, "").await;
    assert_eq!(value.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn conversations_group_replies_and_page_by_last_status() {
    let srv = TestServer::new(true);
    let scopes = "read read:statuses write write:conversations write:statuses";
    let alice = token(&srv, "alice", scopes);
    let bob = token(&srv, "bob", scopes);

    let first = dm(&srv, &alice, "bob", "one").await;
    // Bob replies in the thread mentioning alice; alice replies again.
    let reply_body = encode(&[
        ("status", "@alice two"),
        ("visibility", "direct"),
        ("in_reply_to_id", &first),
    ]);
    let reply = ok(&srv, "POST", "/api/v1/statuses", &bob, &reply_body).await;
    let reply_id = reply["id"].as_str().unwrap().to_string();
    let reply2_body = encode(&[
        ("status", "@bob three"),
        ("visibility", "direct"),
        ("in_reply_to_id", &reply_id),
    ]);
    let reply2 = ok(&srv, "POST", "/api/v1/statuses", &bob, &reply2_body).await;
    let _reply2_id = reply2["id"].as_str().unwrap().to_string();
    // A separate thread stays separate.
    let _other = dm(&srv, &alice, "bob", "separate").await;

    // Bob sees two conversations (one per thread root); the long thread's
    // last status is the newest reply.
    let value = ok(&srv, "GET", "/api/v1/conversations", &bob, "").await;
    let items = value.as_array().unwrap();
    assert_eq!(items.len(), 2);
    let thread = items.iter().find(|item| item["id"].as_str() == Some(&first)).unwrap();
    assert_eq!(thread["unread"], Value::Bool(true));
    assert_eq!(
        thread["last_status"]["content"]
            .as_str()
            .unwrap()
            .contains("three"),
        true
    );

    // Pagination over `last_status_id`: limit=1 yields a full page with a
    // `next` cursor; `max_id` excludes newer threads.
    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/conversations?limit=1", Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = json(&bytes);
    assert_eq!(page.as_array().unwrap().len(), 1);
    let link = headers
        .get(axum::http::header::LINK)
        .expect("Link on a full page")
        .to_str()
        .unwrap()
        .to_string();
    assert!(link.contains("max_id="), "{link}");
    let _ = dm_link_params(Some(&link));
    let last = page[0]["last_status"]["id"].as_str().unwrap().to_string();
    let value = ok(
        &srv,
        "GET",
        &format!("/api/v1/conversations?max_id={last}"),
        &bob,
        "",
    )
    .await;
    for item in value.as_array().unwrap() {
        let id: u64 = item["last_status"]["id"].as_str().unwrap().parse().unwrap();
        assert!(id < last.parse::<u64>().unwrap());
    }
}

#[tokio::test]
async fn conversations_are_direct_and_private() {
    let srv = TestServer::new(true);
    let scopes = "read read:statuses write write:conversations write:statuses";
    let alice = token(&srv, "alice", scopes);
    let bob = token(&srv, "bob", scopes);
    let carol = token(&srv, "carol", scopes);

    // A public mention is not a conversation.
    let body = encode(&[("status", "@bob public hi"), ("visibility", "public")]);
    ok(&srv, "POST", "/api/v1/statuses", &alice, &body).await;
    let (_, value) = call(&srv, "GET", "/api/v1/conversations", Some(&bob), "").await;
    assert_eq!(value, Value::Array(Vec::new()));

    // A DM is invisible to non-participants, by list and by id.
    let dm_id = dm(&srv, &alice, "bob", "secret").await;
    let (_, value) = call(&srv, "GET", "/api/v1/conversations", Some(&carol), "").await;
    assert_eq!(value, Value::Array(Vec::new()));
    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/conversations/{dm_id}/read"),
        Some(&carol),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"], Value::String("Not Found".into()));
}

// ---------------------------------------------------------------------------
// Conversation mutes on statuses.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_mute_unmute_flips_muted_flag() {
    let srv = TestServer::new(true);
    let scopes = "read read:statuses write write:statuses write:mutes";
    let alice = token(&srv, "alice", scopes);
    let bob = token(&srv, "bob", scopes);

    let dm_id = dm(&srv, &alice, "bob", "mute me").await;

    // Mute is idempotent and reflected on the DTO and on show.
    for _ in 0..2 {
        let (status, value) = call(
            &srv,
            "POST",
            &format!("/api/v1/statuses/{dm_id}/mute"),
            Some(&bob),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["id"].as_str().unwrap(), dm_id);
        assert_eq!(value["muted"], Value::Bool(true));
    }
    let (_, value) = call(
        &srv,
        "GET",
        &format!("/api/v1/statuses/{dm_id}"),
        Some(&bob),
        "",
    )
    .await;
    assert_eq!(value["muted"], Value::Bool(true));

    // Unmute clears it (idempotent no-op the second time).
    for _ in 0..2 {
        let (status, value) = call(
            &srv,
            "POST",
            &format!("/api/v1/statuses/{dm_id}/unmute"),
            Some(&bob),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["muted"], Value::Bool(false));
    }

    // Unknown or invisible statuses 404 with the explicit body.
    let (status, value) = call(
        &srv,
        "POST",
        "/api/v1/statuses/987654321/mute",
        Some(&bob),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"], Value::String("Not Found".into()));
}

// ---------------------------------------------------------------------------
// Web Push subscriptions.
// ---------------------------------------------------------------------------

fn push_body(endpoint: &str, p256dh: &str, auth_key: &str, extra: &str) -> String {
    let mut body = format!(
        "{}={}&{}={}&{}={}",
        enc("subscription[endpoint]"),
        enc(endpoint),
        enc("subscription[keys][p256dh]"),
        enc(p256dh),
        enc("subscription[keys][auth]"),
        enc(auth_key),
    );
    if !extra.is_empty() {
        body.push('&');
        body.push_str(extra);
    }
    body
}

fn test_push_keys() -> (String, String) {
    use base64::Engine as _;
    let mut p256dh = vec![0x04u8];
    p256dh.extend([7u8; 64]);
    let auth = [9u8; 16];
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    (enc.encode(&p256dh), enc.encode(auth))
}

#[tokio::test]
async fn push_subscription_crud_and_validation() {
    let srv = TestServer::new(true);
    let full = token(&srv, "alice", "read push");
    let read_only = token(&srv, "bob", "read");
    let (p256dh, auth_key) = test_push_keys();

    // No token: 401. Wrong scope: 403.
    let (status, _) = call(&srv, "GET", "/api/v1/push/subscription", None, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, value) = call(&srv, "GET", "/api/v1/push/subscription", Some(&read_only), "").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{value}");

    // Nothing stored yet: show and update 404 with the explicit body.
    let (status, value) = call(&srv, "GET", "/api/v1/push/subscription", Some(&full), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value["error"], Value::String("Not Found".into()));

    // Validation: missing endpoint, bad endpoint, bad keys.
    let (status, value) = call(
        &srv,
        "POST",
        "/api/v1/push/subscription",
        Some(&full),
        &push_body("", &p256dh, &auth_key, ""),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        value["error"],
        Value::String("Validation failed: Endpoint can't be blank".into())
    );
    let (status, value) = call(
        &srv,
        "POST",
        "/api/v1/push/subscription",
        Some(&full),
        &push_body("gopher://x/y", &p256dh, &auth_key, ""),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        value["error"],
        Value::String("Validation failed: Endpoint is invalid".into())
    );
    let (status, value) = call(
        &srv,
        "POST",
        "/api/v1/push/subscription",
        Some(&full),
        &push_body("https://push.example/sub", "bogus", &auth_key, ""),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        value["error"],
        Value::String("Validation failed: Invalid key".into())
    );

    // Create: exact serializer shape.
    let (status, value) = call(
        &srv,
        "POST",
        "/api/v1/push/subscription",
        Some(&full),
        &push_body("https://push.example/sub", &p256dh, &auth_key, ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(
        {
            let mut keys: Vec<&str> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys
        },
        ["alerts", "endpoint", "id", "policy", "server_key", "standard"]
    );
    assert_eq!(value["endpoint"], Value::String("https://push.example/sub".into()));
    assert_eq!(value["standard"], Value::Bool(false));
    assert_eq!(value["policy"], Value::String("all".into()));
    assert!(value["server_key"].is_string());

    // GET returns the same row.
    let created = value;
    let (status, value) = call(&srv, "GET", "/api/v1/push/subscription", Some(&full), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, created);

    // PUT replaces only data; PATCH is an alias.
    let data = encode(&[
        ("data[policy]", "follower"),
        ("data[alerts][mention]", "true"),
    ]);
    let (status, value) = call(&srv, "PUT", "/api/v1/push/subscription", Some(&full), &data).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["policy"], Value::String("follower".into()));
    assert_eq!(value["alerts"]["mention"], Value::Bool(true));
    assert_eq!(value["alerts"]["follow"], Value::Bool(false));
    let (status, value) = call(&srv, "PATCH", "/api/v1/push/subscription", Some(&full), &data).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["policy"], Value::String("follower".into()));

    // A second create replaces the token's row (one subscription per token).
    let (status, second) = call(
        &srv,
        "POST",
        "/api/v1/push/subscription",
        Some(&full),
        &push_body("https://push.example/other", &p256dh, &auth_key, ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["endpoint"], Value::String("https://push.example/other".into()));

    // DELETE clears it; show and update 404 afterwards.
    let (status, value) = call(&srv, "DELETE", "/api/v1/push/subscription", Some(&full), "").await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value, serde_json::json!({}));
    let (status, _) = call(&srv, "GET", "/api/v1/push/subscription", Some(&full), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&srv, "PUT", "/api/v1/push/subscription", Some(&full), &data).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Push delivery job (fake endpoint, no real network).
// ---------------------------------------------------------------------------

/// A captured push request (headers + body).
#[derive(Debug, Default, Clone)]
struct Captured {
    authorization: String,
    crypto_key: String,
    ttl: String,
    body_len: usize,
}

/// Serve a fake push endpoint on loopback; records the last request.
/// `status` is answered to every POST.
async fn fake_push_endpoint(
    status: u16,
) -> (SocketAddr, Arc<Mutex<Option<Captured>>>, tokio::task::JoinHandle<()>) {
    let captured: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = axum::Router::new().route(
        "/sub",
        axum::routing::post(move |request: Request<axum::body::Body>| async move {
            let (parts, body) = request.into_parts();
            let bytes = axum::body::to_bytes(body, 1024).await.unwrap_or_default();
            *seen.lock().unwrap() = Some(Captured {
                authorization: parts
                    .headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
                crypto_key: parts
                    .headers
                    .get("crypto-key")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
                ttl: parts
                    .headers
                    .get("ttl")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
                body_len: bytes.len(),
            });
            axum::http::StatusCode::from_u16(status).unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    (addr, captured, handle)
}

fn fake_vapid() -> push_app::VapidKeys {
    use base64::Engine as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .expect("generate");
    let der = doc.as_ref();
    let priv_off = der.windows(2).position(|w| *w == [0x04, 0x20]).expect("priv");
    let pub_off = der.windows(4).position(|w| *w == [0x03, 0x42, 0x00, 0x04]).expect("pub");
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    push_app::VapidKeys {
        public_b64: enc.encode(&der[pub_off + 3..pub_off + 3 + 65]),
        private_b64: enc.encode(&der[priv_off + 2..priv_off + 34]),
    }
}

#[tokio::test]
async fn push_delivery_sends_vapid_tickle_and_classifies() {
    use birchtree::domain::WebPushSubscription;

    let keys = fake_vapid();
    let client = reqwest::Client::new();

    // 201: delivered; the request carries the VAPID headers and no body.
    let (addr, captured, handle) = fake_push_endpoint(201).await;
    let sub = WebPushSubscription {
        id: birchtree::domain::Id::new(1),
        token_id: birchtree::domain::Id::new(2),
        user_id: birchtree::domain::Id::new(3),
        endpoint: format!("http://{addr}/sub"),
        key_p256dh: "unused".into(),
        key_auth: "unused".into(),
        standard: false,
        policy: "all".into(),
        alerts: HashMap::from([("mention".to_string(), true)]),
        unsubscribe_token: String::new(),
        unsubscribe_url: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    let request =
        push_app::build_push_request(&sub, &keys, "mailto:admin@example.social", T0).expect("request");
    assert_eq!(push_app::deliver_push(&client, &request).await, push_app::PushOutcome::Delivered);
    let seen = captured.lock().unwrap().clone().expect("captured");
    assert!(seen.authorization.starts_with("vapid t="), "{}", seen.authorization);
    assert!(seen.authorization.contains(", k="), "{}", seen.authorization);
    assert!(seen.crypto_key.starts_with("p256ecdsa="), "{}", seen.crypto_key);
    assert_eq!(seen.ttl, (push_app::PUSH_TTL_SECS).to_string());
    assert_eq!(seen.body_len, 0);
    handle.abort();

    // 404/410: gone. 429/500: retry.
    assert_eq!(push_app::classify_push_response(404), push_app::PushOutcome::Gone);
    assert_eq!(push_app::classify_push_response(410), push_app::PushOutcome::Gone);
    assert!(matches!(
        push_app::classify_push_response(429),
        push_app::PushOutcome::Retry(_)
    ));
}

#[tokio::test]
async fn push_job_end_to_end_through_fake_endpoint() {
    let srv = TestServer::new(true);
    let alice = token(&srv, "alice", "read write write:statuses");
    let bob = token(&srv, "bob", "read push read:notifications write:notifications");
    let (p256dh, auth_key) = test_push_keys();

    // Bob subscribes, pointed at a fake endpoint that answers 410 (gone).
    let (addr, _captured, handle) = fake_push_endpoint(410).await;
    let endpoint = format!("http://{addr}/sub");
    let (status, created) = call(
        &srv,
        "POST",
        "/api/v1/push/subscription",
        Some(&bob),
        &push_body(
            &endpoint,
            &p256dh,
            &auth_key,
            &format!("{}=true", enc("data[alerts][mention]")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let sub_id: u64 = created["id"].as_str().unwrap().parse().unwrap();

    // Round 3 area C made the upstream-faithful `private_mentions=filter`
    // default real: a stranger-DM mention now files a notification request
    // instead of a Mention row. This legacy push test wants the Mention row,
    // so it opts out explicitly (as a client would via the policy UI).
    let (status, _) = call(&srv, "PUT", "/api/v1/notifications/policy", Some(&bob), "filter_private_mentions=false").await;
    assert_eq!(status, StatusCode::OK);

    // Alice mentions bob: a Mention notification row exists after the drain.
    let _ = dm(&srv, &alice, "bob", "job hello").await;
    {
        let mail = birchtree::mail::LoggingMailSender::default();
        for job in srv.store.claim_due(4_000_000_000_000, 64).unwrap() {
            birchtree::jobs::handlers::handle(
                &job,
                &*srv.store,
                &mail,
                &*srv.blobs,
                &birchtree::application::ports::NoStreaming,
            )
            .unwrap();
            srv.store.complete(job.id).unwrap();
        }
    }
    let notifications = srv
        .store
        .notifications_page(
            srv.store
                .account_by_handle("bob", "example.social")
                .unwrap()
                .unwrap()
                .id,
            10,
            None,
            None,
            None,
        )
        .unwrap();
    let mention = notifications
        .iter()
        .find(|n| n.r#type == birchtree::domain::NotificationType::Mention)
        .expect("mention notification");
    assert_ne!(mention.id.value(), 0);

    // The job sees a gone endpoint and asks for the subscription's deletion.
    // (`now` is the real clock: the store stamps rows with `SystemClock`, so
    // a far-future value would wrongly age the notification past its TTL.)
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    let outcome = push_app::handle_push_job(
        &*srv.store,
        &reqwest::Client::new(),
        &fake_vapid(),
        "mailto:admin@example.social",
        birchtree::domain::Id::new(sub_id),
        mention.id,
        now_ms,
    )
    .await
    .expect("job");
    assert_eq!(
        outcome,
        push_app::PushJobOutcome::DeleteSubscriptionAndComplete(birchtree::domain::Id::new(sub_id))
    );
    handle.abort();
}

// ---------------------------------------------------------------------------
// The `direct` stream carries `conversation` events (closes D18).
// ---------------------------------------------------------------------------

/// One parsed SSE block (kept for documentation; the reader below returns
/// plain `(name, data)` pairs).
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum Block {
    Event { name: String, data: String },
}

/// Minimal SSE reader (comments skipped by the caller).
struct Sse {
    response: reqwest::Response,
    buffer: String,
    name: Option<String>,
    data: Option<String>,
}

impl Sse {
    async fn next_event(&mut self, wait: Duration) -> Option<(String, String)> {
        let deadline = Instant::now() + wait;
        loop {
            while let Some(end) = self.buffer.find('\n') {
                let line: String = self.buffer.drain(..=end).collect();
                let line = line.trim_end_matches('\n').trim_end_matches('\r');
                if let Some(value) = line.strip_prefix("event: ") {
                    self.name = Some(value.to_string());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    self.data = Some(value.to_string());
                } else if line.is_empty() {
                    if let Some(name) = self.name.take() {
                        let data = self.data.take().unwrap_or_default();
                        return Some((name, data));
                    }
                }
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            match tokio::time::timeout(left, self.response.chunk()).await {
                Ok(Ok(Some(chunk))) => self.buffer.push_str(&String::from_utf8_lossy(&chunk)),
                _ => return None,
            }
        }
    }
}

async fn serve(srv: &TestServer) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = srv.router.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    addr
}

#[tokio::test]
async fn direct_stream_carries_conversation_events() {
    let srv = TestServer::new(true);
    let scopes = "read read:statuses write write:statuses";
    let alice = token(&srv, "alice", scopes);
    let bob = token(&srv, "bob", scopes);
    let carol = token(&srv, "carol", scopes);
    let addr = serve(&srv).await;

    let mut bob_stream = {
        let response = reqwest::Client::new()
            .get(format!("http://{addr}/api/v1/streaming/direct"))
            .header("authorization", format!("Bearer {bob}"))
            .send()
            .await
            .expect("sse");
        assert_eq!(response.status(), StatusCode::OK);
        Sse {
            response,
            buffer: String::new(),
            name: None,
            data: None,
        }
    };
    let mut carol_stream = {
        let response = reqwest::Client::new()
            .get(format!("http://{addr}/api/v1/streaming/direct"))
            .header("authorization", format!("Bearer {carol}"))
            .send()
            .await
            .expect("sse");
        assert_eq!(response.status(), StatusCode::OK);
        Sse {
            response,
            buffer: String::new(),
            name: None,
            data: None,
        }
    };

    let dm_id = dm(&srv, &alice, "bob", "stream me").await;

    // Bob's direct stream gets the `conversation` event for the thread;
    // carol (not a participant) gets nothing.
    let (name, data) = bob_stream
        .next_event(WAIT)
        .await
        .expect("conversation event");
    assert_eq!(name, "conversation");
    let payload: Value = serde_json::from_str(&data).expect("json payload");
    assert_eq!(payload["id"].as_str().unwrap(), dm_id);
    assert_eq!(payload["unread"], Value::Bool(true));
    assert!(
        carol_stream
            .next_event(Duration::from_millis(500))
            .await
            .is_none(),
        "no leak to non-participants"
    );
}

//! Round 3, area C: notification requests + encrypted push (D20/D42).
//!
//! - Request pipeline: a filtered mention files a request row (hidden from
//!   the index), stays silent on the streams, and accept unhides it while
//!   emitting `notifications_merged` (payload `"1"`) — only on a real merge.
//! - No-request paths emit nothing: a bogus accept is 404 and silent, and
//!   the default policy leaves the requests surface `[]` with `merged: true`.
//! - Push: `aes128gcm` encryption round-trips through an independent
//!   receiver-side decrypt (fresh DH from the receiver key + header parse +
//!   AES-GCM open), including the full `handle_push_job` delivery against a
//!   fake endpoint; the legacy tickle stays bodiless.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{T0, TestServer, form_request, send};
use serde_json::Value;

use birchtree::application::auth;
use birchtree::application::ports::AccountStore;
use birchtree::application::push::PushSubscriptionStore;
use birchtree::application::{push as push_app, push_crypto};

const SCOPES: &str = "read write follow read:statuses write:statuses read:notifications write:notifications";

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(500);

async fn serve(srv: &TestServer) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
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
        format!("Round3C {username}"),
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

async fn call(srv: &TestServer, method: &str, path: &str, token: &str, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(&srv.router, form_request(method, path, body, Some(token))).await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

fn drain_jobs(srv: &TestServer) {
    use birchtree::application::ports::JobQueue;
    let mail = birchtree::mail::LoggingMailSender;
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as u64
            + 3_600_000;
        let jobs = srv.store.claim_due(now, 64).expect("claim");
        if jobs.is_empty() {
            break;
        }
        for job in jobs {
            birchtree::jobs::handlers::handle(&job, &*srv.store, &mail, &*srv.blobs, &srv.streams)
                .expect("handle job");
            srv.store.complete(job.id).expect("complete");
        }
    }
}

// ---------------------------------------------------------------------------
// SSE client (minimal; mirrors tests/phase7_gaps_round2_b.rs).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Block {
    Comment(String),
    Event { name: String, data: String },
}

struct Sse {
    response: reqwest::Response,
    buffer: String,
    pending: std::collections::VecDeque<Block>,
    name: Option<String>,
    data: Option<String>,
}

impl Sse {
    async fn next_block(&mut self, wait: Duration) -> Option<Block> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(block) = self.pending.pop_front() {
                return Some(block);
            }
            while let Some(end) = self.buffer.find('\n') {
                let line: String = self.buffer.drain(..=end).collect();
                let line = line.trim_end_matches('\n').trim_end_matches('\r');
                if let Some(comment) = line.strip_prefix(':') {
                    self.pending.push_back(Block::Comment(comment.to_string()));
                } else if let Some(value) = line.strip_prefix("event: ") {
                    self.name = Some(value.to_string());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    self.data = Some(value.to_string());
                } else if line.is_empty() {
                    if let (Some(name), Some(data)) = (self.name.take(), self.data.take()) {
                        self.pending.push_back(Block::Event { name, data });
                    }
                }
            }
            if let Some(block) = self.pending.pop_front() {
                return Some(block);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let chunk = tokio::time::timeout(left, self.response.chunk())
                .await
                .ok()?
                .ok()??;
            if chunk.is_empty() {
                return None;
            }
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    async fn collect(&mut self, wait: Duration) -> Vec<Block> {
        let deadline = Instant::now() + wait;
        let mut blocks = Vec::new();
        while let Some(block) = self.next_block(deadline.saturating_duration_since(Instant::now())).await {
            blocks.push(block);
        }
        blocks
    }
}

async fn open_sse(addr: SocketAddr, path: &str, token: Option<&str>) -> Sse {
    let mut request = reqwest::Client::new().get(format!("http://{addr}{path}"));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = request.send().await.expect("sse request");
    assert_eq!(response.status(), StatusCode::OK, "sse {path}");
    Sse {
        response,
        buffer: String::new(),
        pending: std::collections::VecDeque::new(),
        name: None,
        data: None,
    }
}

fn encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn url_encode(value: &str) -> String {
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

// ---------------------------------------------------------------------------
// Request pipeline: filter -> request queue -> accept -> notifications_merged.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_create_merge_fires_event() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "c_alice", SCOPES);
    let bob = token(&srv, "c_bob", SCOPES);

    // Bob filters mentions from accounts he does not follow.
    let (status, policy) = call(&srv, "PUT", "/api/v1/notifications/policy", &bob, "filter_not_following=true").await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    assert_eq!(policy["filter_not_following"], Value::Bool(true));

    // Streams open BEFORE the mention, so every later frame is observed.
    let mut bob_notes = open_sse(addr, "/api/v1/streaming/user/notification", Some(&bob)).await;
    let mut bob_user = open_sse(addr, "/api/v1/streaming/user", Some(&bob)).await;

    // Alice mentions Bob; she is not followed, so the mention filters.
    let body = encode(&[("status", "hi @c_bob, secret plan"), ("visibility", "public")]);
    let (status, value) = call(&srv, "POST", "/api/v1/statuses", &alice, &body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    drain_jobs(&srv);

    // Hidden from the index ...
    let (status, index) = call(&srv, "GET", "/api/v1/notifications", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert_eq!(index, Value::Array(Vec::new()), "filtered mention stays out of the index");

    // ... but filed on the request queue, honestly shaped ...
    let (status, requests) = call(&srv, "GET", "/api/v1/notifications/requests", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{requests}");
    let rows = requests.as_array().expect("requests array");
    assert_eq!(rows.len(), 1, "{requests}");
    let row = &rows[0];
    assert_eq!(row["account"]["username"], Value::String("c_alice".into()), "{row}");
    assert_eq!(row["notifications_count"], Value::String("1".into()), "{row}");
    assert!(row["last_status"]["id"].is_string(), "{row}");
    let request_id = row["id"].as_str().expect("request id").to_string();

    // ... and the streams stay silent (no `notification`, no merge yet).
    for blocks in [bob_notes.collect(QUIET).await, bob_user.collect(QUIET).await] {
        assert!(
            blocks.iter().all(|b| !matches!(b, Block::Event { name, .. } if name == "notification" || name == "notifications_merged")),
            "filtered mention streams nothing: {blocks:?}"
        );
    }
    let (status, merged) = call(&srv, "GET", "/api/v1/notifications/requests/merged", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged, serde_json::json!({"merged": false}), "{merged}");

    // Accept: unhide + merge signal on BOTH notification streams.
    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/notifications/requests/{request_id}/accept"),
        &bob,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");

    for stream in [&mut bob_notes, &mut bob_user] {
        let mut saw_merged = false;
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match stream.next_block(left).await {
                Some(Block::Event { name, data }) if name == "notifications_merged" => {
                    assert_eq!(data, "1", "upstream payload is the raw string '1'");
                    saw_merged = true;
                    break;
                }
                Some(_) => continue,
                None => break,
            }
        }
        assert!(saw_merged, "notifications_merged fires on a real merge");
    }

    // The queue drains, the mention surfaces, merged reads true.
    let (status, requests) = call(&srv, "GET", "/api/v1/notifications/requests", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{requests}");
    assert_eq!(requests, Value::Array(Vec::new()));
    let (status, index) = call(&srv, "GET", "/api/v1/notifications", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert_eq!(index.as_array().expect("index").len(), 1, "{index}");
    assert_eq!(index[0]["type"], Value::String("mention".into()));
    let (status, merged) = call(&srv, "GET", "/api/v1/notifications/requests/merged", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged, serde_json::json!({"merged": true}), "{merged}");

    // Accept is sticky: a second mention from Alice now delivers directly
    // (the permission bypass), with no new request.
    let body = encode(&[("status", "hi again @c_bob"), ("visibility", "public")]);
    let (status, value) = call(&srv, "POST", "/api/v1/statuses", &alice, &body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    drain_jobs(&srv);
    let (status, requests) = call(&srv, "GET", "/api/v1/notifications/requests", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{requests}");
    assert_eq!(requests, Value::Array(Vec::new()), "permission bypass files no request");
    let (status, index) = call(&srv, "GET", "/api/v1/notifications", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert_eq!(index.as_array().expect("index").len(), 2, "{index}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_request_no_event() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let bob = token(&srv, "n_bob", SCOPES);
    let mut bob_notes = open_sse(addr, "/api/v1/streaming/user/notification", Some(&bob)).await;

    // Default policy: the surface stays honest and empty.
    let (status, requests) = call(&srv, "GET", "/api/v1/notifications/requests", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{requests}");
    assert_eq!(requests, Value::Array(Vec::new()));
    let (status, merged) = call(&srv, "GET", "/api/v1/notifications/requests/merged", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged, serde_json::json!({"merged": true}));

    // A bogus accept is 404 and streams nothing.
    let (status, _) = call(&srv, "POST", "/api/v1/notifications/requests/999999/accept", &bob, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&srv, "POST", "/api/v1/notifications/requests/999999/dismiss", &bob, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for block in bob_notes.collect(QUIET).await {
        assert!(
            !matches!(&block, Block::Event { name, .. } if name == "notifications_merged"),
            "no merge without a request: {block:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dismiss_deletes_silently() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "d_alice", SCOPES);
    let bob = token(&srv, "d_bob", SCOPES);
    let (status, _) = call(&srv, "PUT", "/api/v1/notifications/policy", &bob, "filter_not_following=true").await;
    assert_eq!(status, StatusCode::OK);
    let mut bob_notes = open_sse(addr, "/api/v1/streaming/user/notification", Some(&bob)).await;

    let body = encode(&[("status", "hi @d_bob, never mind"), ("visibility", "public")]);
    let (status, value) = call(&srv, "POST", "/api/v1/statuses", &alice, &body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    drain_jobs(&srv);
    let (_, requests) = call(&srv, "GET", "/api/v1/notifications/requests", &bob, "").await;
    let request_id = requests[0]["id"].as_str().expect("request id").to_string();

    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/notifications/requests/{request_id}/dismiss"),
        &bob,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    // The filtered row is gone (not unhidden), the queue drains, and no
    // merge signal fires: a dismissal is silent.
    let (status, index) = call(&srv, "GET", "/api/v1/notifications", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert_eq!(index, Value::Array(Vec::new()));
    let (status, merged) = call(&srv, "GET", "/api/v1/notifications/requests/merged", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged, serde_json::json!({"merged": true}));
    for block in bob_notes.collect(QUIET).await {
        assert!(
            !matches!(&block, Block::Event { name, .. } if name == "notifications_merged"),
            "dismiss streams no merge: {block:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Push: aes128gcm encrypt + decrypt-check, tickle preserved.
// ---------------------------------------------------------------------------

/// Generate a VAPID pair the way `src/application/push.rs` tests do.
fn test_vapid_keys() -> push_app::VapidKeys {
    use base64::Engine as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .expect("generate");
    let der = doc.as_ref();
    let priv_off = der.windows(2).position(|w| *w == [0x04, 0x20]).expect("priv marker");
    let priv_raw = &der[priv_off + 2..priv_off + 34];
    let pub_off = der
        .windows(4)
        .position(|w| *w == [0x03, 0x42, 0x00, 0x04])
        .expect("pub marker");
    let pub_raw = &der[pub_off + 3..pub_off + 3 + 65];
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    push_app::VapidKeys {
        public_b64: enc.encode(pub_raw),
        private_b64: enc.encode(priv_raw),
    }
}

/// Generate a receiver (client) P-256 pair; the private half stays in-test
/// for the independent decrypt.
fn test_receiver_keys() -> (ring::agreement::EphemeralPrivateKey, String, String) {
    use base64::Engine as _;
    let rng = ring::rand::SystemRandom::new();
    let private =
        ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::ECDH_P256, &rng)
            .expect("receiver key");
    let public = private.compute_public_key().expect("receiver pub");
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut auth = [0u8; 16];
    use ring::rand::SecureRandom as _;
    rng.fill(&mut auth).expect("auth secret");
    (
        private,
        enc.encode(public.as_ref()),
        enc.encode(auth),
    )
}

/// Decrypt an `aes128gcm` body from the receiver side only: parse the
/// header, run a fresh DH against the carried sender point, re-derive, open.
fn receiver_decrypt(
    private: ring::agreement::EphemeralPrivateKey,
    receiver_pub_b64: &str,
    auth_b64: &str,
    body: &[u8],
) -> Vec<u8> {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (salt, rs, sender_pub, mut ciphertext) = push_crypto::parse_body(body).expect("parse");
    assert_eq!(rs, push_crypto::RECORD_SIZE);
    let auth_raw = enc.decode(auth_b64).expect("auth");
    let receiver_raw = enc.decode(receiver_pub_b64).expect("p256dh");
    let mut receiver_point = [0u8; 65];
    receiver_point.copy_from_slice(&receiver_raw);
    ring::agreement::agree_ephemeral(
        private,
        &ring::agreement::UnparsedPublicKey::new(&ring::agreement::ECDH_P256, &sender_pub),
        |shared| {
            // The header salt feeds the second KDF stage (RFC 8291 §3.4).
            let (cek, nonce) =
                push_crypto::derive_content_keys(&salt, &auth_raw, shared, &receiver_point, &sender_pub)
                    .expect("kdf");
            push_crypto::open_record(&cek, &nonce, &mut ciphertext).expect("open");
            assert_eq!(ciphertext.last(), Some(&0x02), "final-record delimiter");
            ciphertext[..ciphertext.len() - 1].to_vec()
        },
    )
    .expect("agree")
}

#[test]
fn push_encrypt_round_trip() {

    let (private, p256dh, auth) = test_receiver_keys();
    let plaintext = br#"{"notification_id":"42","notification_type":"mention","title":"t","body":"b"}"#;
    let payload = push_crypto::encrypt_aes128gcm(&p256dh, &auth, plaintext).expect("encrypt");
    assert_eq!(payload.content_encoding, "aes128gcm");
    let opened = receiver_decrypt(private, &p256dh, &auth, &payload.body);
    assert_eq!(opened, plaintext);
}

/// RFC 8291 Appendix A intermediates (independent of our encrypt path: the
/// ECDH secret is taken straight from the RFC, so this locks the KDF +
/// framing byte-exact against the published vector).
fn rfc8291_a() -> (Vec<u8>, Vec<u8>, [u8; 16], [u8; 65], [u8; 65], Vec<u8>) {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let no_ws = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let salt = enc.decode("DGv6ra1nlYgDCS1FRnbzlw").expect("salt");
    let auth = enc.decode("BTBZMqHH6r4Tts7J_aSIgg").expect("auth");
    let ecdh = enc.decode("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs").expect("ecdh");
    let mut rx = [0u8; 65];
    let rx_raw = enc
        .decode(no_ws(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        ))
        .expect("rx");
    rx.copy_from_slice(&rx_raw);
    let mut tx = [0u8; 65];
    let tx_raw = enc
        .decode(no_ws(
            "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8",
        ))
        .expect("tx");
    tx.copy_from_slice(&tx_raw);
    let mut salt_arr = [0u8; 16];
    salt_arr.copy_from_slice(&salt);
    (salt, auth, salt_arr, rx, tx, ecdh)
}

#[test]
fn rfc8291_appendix_a_intermediates() {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (salt_vec, auth, salt, rx, tx, ecdh) = rfc8291_a();
    assert_eq!(salt_vec, salt.to_vec());
    let (cek, nonce) =
        push_crypto::derive_content_keys(&salt, &auth, &ecdh, &rx, &tx).expect("kdf");
    assert_eq!(enc.encode(cek), "oIhVW04MRdy2XN9CiKLxTg");
    assert_eq!(enc.encode(nonce), "4h_95klXJ5E_qnoN");
}

#[test]
fn rfc8291_section5_example_decrypts() {
    let (_salt_vec, _auth, salt, rx, tx, ecdh) = rfc8291_a();
    // auth needed for the KDF; re-derive from the helper inputs.
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let auth = enc.decode("BTBZMqHH6r4Tts7J_aSIgg").expect("auth");
    let (cek, nonce) =
        push_crypto::derive_content_keys(&salt, &auth, &ecdh, &rx, &tx).expect("kdf");
    let body_b64 = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";
    let body = enc.decode(body_b64).expect("body");
    let (_parsed_salt, _rs, _sender, mut ciphertext) =
        push_crypto::parse_body(&body).expect("parse");
    push_crypto::open_record(&cek, &nonce, &mut ciphertext).expect("open");
    assert_eq!(ciphertext.last(), Some(&0x02));
    assert_eq!(
        &ciphertext[..ciphertext.len() - 1],
        b"When I grow up, I want to be a watermelon"
    );
}

#[derive(Debug, Clone, Default)]
struct Captured {
    authorization: String,
    content_encoding: String,
    content_type: String,
    ttl: String,
    body: Vec<u8>,
}

async fn fake_push_endpoint(
    status: u16,
) -> (SocketAddr, Arc<Mutex<Option<Captured>>>, tokio::task::JoinHandle<()>) {
    let captured: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let app = axum::Router::new().route(
        "/sub",
        axum::routing::post(move |request: Request<Body>| async move {
            let (parts, body) = request.into_parts();
            let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap_or_default();
            let header = |name: &str| {
                parts
                    .headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            };
            *seen.lock().unwrap() = Some(Captured {
                authorization: header("authorization"),
                content_encoding: header("content-encoding"),
                content_type: header("content-type"),
                ttl: header("ttl"),
                body: bytes.to_vec(),
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

/// Full encrypted delivery through `handle_push_job`: a standard
/// subscription receives the `aes128gcm` body, which decrypts to the
/// notification JSON under the receiver key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_encrypted_delivery_decrypts() {
    use birchtree::application::ports::NotificationStore;

    let srv = TestServer::new(false);
    let alice = token(&srv, "p_alice", SCOPES);
    let bob_token = token(&srv, "p_bob", SCOPES);
    let bob = srv
        .store
        .account_by_handle("p_bob", "example.social")
        .expect("store")
        .expect("bob");

    // A standard subscription with fresh receiver keys.
    let (private, p256dh, auth) = test_receiver_keys();
    let (addr, captured, handle) = fake_push_endpoint(201).await;
    // The token row behind `bob_token` binds the subscription.
    let app = auth::register_app(
        &*srv.store,
        "Round3C push".to_string(),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .expect("app");
    let (_, token_row) = auth::issue_user_token(
        &*srv.store,
        app.app.id,
        bob.id,
        vec!["read".to_string(), "read:notifications".to_string()],
        T0,
    )
    .expect("token");
    let sub = srv
        .store
        .upsert_push_subscription(
            token_row.id,
            bob.id,
            format!("http://{addr}/sub"),
            p256dh.clone(),
            auth.clone(),
            true,
            "all".to_string(),
            HashMap::from([("mention".to_string(), true)]),
            T0,
        )
        .expect("subscribe");
    let _ = &bob_token;

    // Alice mentions Bob (default policy: delivered, not filtered).
    let body = encode(&[("status", "hi @p_bob, encrypted hello"), ("visibility", "public")]);
    let (status, value) = call(&srv, "POST", "/api/v1/statuses", &alice, &body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    drain_jobs(&srv);
    let notifications = srv
        .store
        .notifications_page(bob.id, 10, None, None, None)
        .expect("page");
    assert_eq!(notifications.len(), 1, "the mention is stored unfiltered");
    let notification = &notifications[0];

    let keys = test_vapid_keys();
    let client = reqwest::Client::new();
    let outcome = push_app::handle_push_job(
        &*srv.store,
        &client,
        &keys,
        "mailto:admin@example.social",
        sub.id,
        notification.id,
        now_ms(),
    )
    .await
    .expect("push job");
    assert_eq!(outcome, push_app::PushJobOutcome::Complete);
    handle.abort();

    let seen = captured.lock().unwrap().clone().expect("delivered");
    assert!(seen.authorization.starts_with("vapid t="), "{}", seen.authorization);
    assert_eq!(seen.content_encoding, "aes128gcm");
    assert_eq!(seen.content_type, "application/octet-stream");
    assert_eq!(seen.ttl, push_app::PUSH_TTL_SECS.to_string());
    assert!(!seen.body.is_empty(), "encrypted delivery has a body");

    let opened = receiver_decrypt(private, &p256dh, &auth, &seen.body);
    let payload: Value = serde_json::from_slice(&opened).expect("payload json");
    assert_eq!(payload["notification_id"], Value::String(notification.id.to_string()));
    assert_eq!(payload["notification_type"], Value::String("mention".into()));
    assert!(payload["title"].is_string(), "{payload}");
    assert!(payload["body"].as_str().expect("body").contains("encrypted hello"), "{payload}");
}

/// The legacy tickle is preserved: empty body, no `Content-Encoding`,
/// same VAPID/TTL headers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_tickle_fallback_preserved() {
    let keys = test_vapid_keys();
    let client = reqwest::Client::new();
    let (addr, captured, handle) = fake_push_endpoint(201).await;
    let sub = birchtree::domain::WebPushSubscription {
        id: birchtree::domain::Id::new(1),
        token_id: birchtree::domain::Id::new(2),
        user_id: birchtree::domain::Id::new(3),
        endpoint: format!("http://{addr}/sub"),
        key_p256dh: "unused".into(),
        key_auth: "unused".into(),
        standard: false,
        policy: "all".into(),
        alerts: HashMap::from([("mention".to_string(), true)]),
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    let request =
        push_app::build_push_request(&sub, &keys, "mailto:admin@example.social", T0).expect("request");
    assert!(request.content_encoding.is_none(), "tickle has no content encoding");
    assert!(request.body.is_empty(), "tickle has no body");
    assert_eq!(
        push_app::deliver_push(&client, &request).await,
        push_app::PushOutcome::Delivered
    );
    let seen = captured.lock().unwrap().clone().expect("captured");
    assert!(seen.authorization.starts_with("vapid t="), "{}", seen.authorization);
    assert_eq!(seen.content_encoding, "", "no content-encoding on a tickle");
    assert!(seen.body.is_empty(), "no body on a tickle");
    assert_eq!(seen.ttl, push_app::PUSH_TTL_SECS.to_string());
    handle.abort();
}

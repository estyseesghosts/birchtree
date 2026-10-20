//! Round 4, D42 remainder: legacy `aesgcm` push encoding.
//!
//! Upstream reference (both sides): Mastodon v4.7.3 (`8ff11c2b`)
//! `app/workers/web/push_notification_worker.rb:31-35` (selection:
//! `web_push_request.legacy`, i.e. `!subscription.standard`, →
//! `perform_legacy_request`, else `perform_standard_request`; the `standard`
//! flag itself comes from the client at subscribe time —
//! `app/controllers/api/v1/push/subscriptions_controller.rb:56`,
//! `standard: subscription_params[:standard] || false`) and
//! `mastodon/webpush@9631ac6` `lib/webpush/legacy/encryption.rb`
//! (draft-ietf-webpush-encryption-04 §3 as implemented by the gem).
//!
//! Covered here:
//! - `aesgcm` encrypt round-trips through an independent receiver-side
//!   decrypt (fresh DH from the receiver key + header-carried salt/sender
//!   key + KDF re-derive + AES-GCM open + padding strip).
//! - The KDF/ciphertext matches an independent Node `crypto` transcription
//!   of the gem byte for byte on fixed vectors (`LEG_*` below).
//! - Selection rule: `standard` → `aes128gcm`, legacy with keys → `aesgcm`
//!   (with upstream's `WebPush <jwt>` auth, `dh=…` Crypto-Key and
//!   `salt=…` Encryption headers), legacy without keys → bodiless tickle.
//! - Full `handle_push_job` delivery of a legacy subscription against a
//!   fake local endpoint decrypts to the notification JSON; the
//!   header-carried salt/sender key are proven to be the actual KDF inputs
//!   by decrypting with the captured header values only.
//! - The keyless tickle is preserved byte-for-shape (no body, no
//!   `Content-Encoding`, unchanged `vapid` auth).
//!
//! Out of scope (no live push service in this environment): delivery against
//! a real push service or browser client. Never fabricated: `access_token`
//! and `icon` stay empty in the payload, as documented on
//! `push_notification_payload`.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{T0, TestServer, form_request, send};
use serde_json::Value;

use birchtree::application::auth;
use birchtree::application::ports::AccountStore;
use birchtree::application::push::PushSubscriptionStore;
use birchtree::application::{push as push_app, push_crypto};

const SCOPES: &str = "read write follow read:statuses write:statuses read:notifications write:notifications";

// ---------------------------------------------------------------------------
// Local helpers (mirroring tests/round3_notif_push.rs, which owns its copies).
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

/// Decrypt a legacy `aesgcm` delivery from the receiver side only: fresh DH
/// against the header-carried sender point, KDF re-derive with the
/// header-carried salt, AES-GCM open, padding strip. Mirrors the gem's
/// `Legacy::Encryption` inputs, never the sender's internals.
fn receiver_decrypt_aesgcm(
    private: ring::agreement::EphemeralPrivateKey,
    receiver_pub_b64: &str,
    auth_b64: &str,
    salt: &[u8; 16],
    server_pub: &[u8; 65],
    ciphertext: &mut Vec<u8>,
) -> Vec<u8> {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let auth_raw = enc.decode(auth_b64).expect("auth");
    let receiver_raw = enc.decode(receiver_pub_b64).expect("p256dh");
    let mut receiver_point = [0u8; 65];
    receiver_point.copy_from_slice(&receiver_raw);
    ring::agreement::agree_ephemeral(
        private,
        &ring::agreement::UnparsedPublicKey::new(&ring::agreement::ECDH_P256, &server_pub[..]),
        |shared| {
            let (cek, nonce) =
                push_crypto::derive_aesgcm_keys(&auth_raw, shared, salt, &receiver_point, server_pub)
                    .expect("kdf");
            push_crypto::open_record(&cek, &nonce, ciphertext).expect("open");
            // Legacy padding (gem `encrypt_payload`): two leading zero octets.
            assert!(ciphertext.len() >= 2, "legacy record holds its padding");
            assert_eq!(&ciphertext[..2], &[0u8, 0u8], "legacy padding octets");
            ciphertext[2..].to_vec()
        },
    )
    .expect("agree")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
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
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
        format!("Round4 {username}"),
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

// ---------------------------------------------------------------------------
// aesgcm encrypt round-trip via independent receiver-side decrypt.
// ---------------------------------------------------------------------------

#[test]
fn aesgcm_encrypt_round_trip() {
    let (private, p256dh, auth) = test_receiver_keys();
    let plaintext = br#"{"notification_id":"7","notification_type":"mention","title":"t","body":"b"}"#;
    let payload = push_crypto::encrypt_aesgcm(&p256dh, &auth, plaintext).expect("encrypt");
    assert_eq!(payload.content_encoding, "aesgcm");
    assert_eq!(payload.salt.len(), 16);
    assert_eq!(payload.server_pub.len(), 65);
    assert_eq!(payload.server_pub[0], 0x04);
    // Ciphertext-only body: plaintext + 2 padding octets + 16-byte tag.
    assert_eq!(payload.ciphertext.len(), plaintext.len() + 2 + 16);
    let mut body = payload.ciphertext.clone();
    let opened = receiver_decrypt_aesgcm(
        private,
        &p256dh,
        &auth,
        &payload.salt,
        &payload.server_pub,
        &mut body,
    );
    assert_eq!(opened, plaintext);
    // Every message draws fresh randomness (salt + sender key).
    let again = push_crypto::encrypt_aesgcm(&p256dh, &auth, plaintext).expect("encrypt");
    assert_ne!(again.ciphertext, payload.ciphertext, "fresh sender key per message");
    assert_ne!(again.salt, payload.salt, "fresh salt per message");
}

// ---------------------------------------------------------------------------
// Fixed vectors: independent Node `crypto` transcription of
// mastodon/webpush@9631ac6 lib/webpush/legacy/encryption.rb.
// receiver priv = 0x02, sender priv = 0x03 (P-256),
// auth = 00112233445566778899aabbccddeeff,
// salt = 0f0e0d0c0b0a09080706050403020100,
// plaintext = {"notification_id":"7","notification_type":"mention"}.
// ---------------------------------------------------------------------------

fn hex_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "even hex");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn aesgcm_kdf_matches_gem_transcription() {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let rx = enc
        .decode("BHzyexiNA09-ilI4AwS1GsPAiWnid_IbNaYLSPxHZpl4B3dVENuO0EApPZrGn3Qw27p9reY86YIpngS3nSJ4c9E")
        .expect("rx");
    let tx = enc
        .decode("BF7L5NGmMwpEyPfvlR1L8WXmxrch762phftBZhvG5_1shzRkDEmY_343SwbOGmSi7NgqsDY4T7g9mnmxJ6J9UDI")
        .expect("tx");
    let mut rxp = [0u8; 65];
    rxp.copy_from_slice(&rx);
    let mut txp = [0u8; 65];
    txp.copy_from_slice(&tx);
    let auth = hex_bytes("00112233445566778899aabbccddeeff");
    let secret = hex_bytes("b01a172a76a4602c92d3242cb897dde3024c740debb215b4c6b0aae93c2291a9");
    let salt_raw = hex_bytes("0f0e0d0c0b0a09080706050403020100");
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&salt_raw);
    let plaintext = br#"{"notification_id":"7","notification_type":"mention"}"#;

    let (cek, nonce) =
        push_crypto::derive_aesgcm_keys(&auth, &secret, &salt, &rxp, &txp).expect("kdf");
    assert_eq!(hex::encode(cek), "d6c9f4bda8a3f975d0df127fdd80db69");
    assert_eq!(hex::encode(nonce), "0f81cdfd758cbfa501e3a421");

    let ciphertext =
        push_crypto::seal_aesgcm_record(&auth, &secret, &salt, &rxp, &txp, plaintext)
            .expect("seal");
    assert_eq!(
        hex::encode(&ciphertext),
        "404a00cbbde657f103fc60d50ffbbd12715ac0260c528926e75c9495fcc540683ceffbe6ff37cd18789cf5ae8ca6628db758b30b6f7de91f337be25021e40a4e915f9d66cce9ab"
    );
}

#[test]
fn aesgcm_refuses_bad_keys_without_leaking_them() {
    let (private, p256dh, auth) = test_receiver_keys();
    let _ = private;
    // Corrupt base64.
    let err = push_crypto::encrypt_aesgcm("!!!", &auth, b"x").expect_err("bad p256dh refused");
    assert!(err.contains("p256dh"), "{err}");
    // Wrong point length (valid base64, 64 bytes).
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let err = push_crypto::encrypt_aesgcm(&enc.encode([4u8; 64]), &auth, b"x")
        .expect_err("short point refused");
    assert!(err.contains("p256dh"), "{err}");
    // Short auth secret.
    let err =
        push_crypto::encrypt_aesgcm(&p256dh, "short", b"x").expect_err("short auth refused");
    assert!(err.contains("auth"), "{err}");
    // No error carries key bytes: the only quoted material is roles/lengths.
    for message in [
        push_crypto::encrypt_aesgcm("!!!", &auth, b"x").expect_err("e1"),
        push_crypto::encrypt_aesgcm(&p256dh, "short", b"x").expect_err("e2"),
    ] {
        assert!(!message.contains("!!!"), "{message}");
    }
}

// ---------------------------------------------------------------------------
// Selection rule at the request-builder level.
// ---------------------------------------------------------------------------

fn keyed_sub(standard: bool) -> birchtree::domain::WebPushSubscription {
    let (_, p256dh, auth) = test_receiver_keys();
    birchtree::domain::WebPushSubscription {
        id: birchtree::domain::Id::new(11),
        token_id: birchtree::domain::Id::new(12),
        user_id: birchtree::domain::Id::new(13),
        endpoint: "https://push.example/sub/1".into(),
        key_p256dh: p256dh,
        key_auth: auth,
        standard,
        policy: "all".into(),
        alerts: HashMap::from([("mention".to_string(), true)]),
        unsubscribe_token: String::new(),
        unsubscribe_url: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    }
}

#[test]
fn selection_rule_encodings() {
    let keys = test_vapid_keys();
    let payload = br#"{"notification_id":"1"}"#;

    // Standard → aes128gcm under the `vapid` scheme, no Encryption header.
    let standard = push_app::build_push_request_encrypted(&keyed_sub(true), &keys, "mailto:a@b.c", T0, payload)
        .expect("standard");
    assert_eq!(standard.content_encoding.as_deref(), Some("aes128gcm"));
    assert!(standard.encryption.is_none(), "no Encryption header on aes128gcm");
    assert!(standard.authorization.starts_with("vapid t="), "{}", standard.authorization);
    assert!(!standard.body.is_empty());

    // Legacy with keys → aesgcm under the `WebPush` scheme, salt + dh headers.
    let legacy = push_app::build_push_request_aesgcm(&keyed_sub(false), &keys, "mailto:a@b.c", T0, payload)
        .expect("legacy");
    assert_eq!(legacy.content_encoding.as_deref(), Some("aesgcm"));
    assert!(legacy.authorization.starts_with("WebPush "), "{}", legacy.authorization);
    assert!(legacy.crypto_key.starts_with("dh="), "{}", legacy.crypto_key);
    assert!(legacy.crypto_key.contains(";p256ecdsa="), "{}", legacy.crypto_key);
    let encryption = legacy.encryption.clone().expect("Encryption header value");
    assert!(encryption.starts_with("salt="), "{encryption}");
    assert_eq!(legacy.ttl_secs, push_app::PUSH_TTL_SECS, "same TTL");
    assert!(!legacy.body.is_empty());

    // Legacy without keys → tickle: bodiless, no encoding, unchanged scheme.
    let mut keyless = keyed_sub(false);
    keyless.key_p256dh = String::new();
    keyless.key_auth = String::new();
    let tickle =
        push_app::build_push_request(&keyless, &keys, "mailto:a@b.c", T0).expect("tickle");
    assert!(tickle.content_encoding.is_none(), "tickle has no content encoding");
    assert!(tickle.encryption.is_none(), "tickle has no Encryption header");
    assert!(tickle.body.is_empty(), "tickle has no body");
    assert!(tickle.authorization.starts_with("vapid t="), "{}", tickle.authorization);
}

// ---------------------------------------------------------------------------
// Fake push endpoint capturing the legacy headers.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Captured {
    authorization: String,
    crypto_key: String,
    encryption: String,
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
                crypto_key: header("crypto-key"),
                encryption: header("encryption"),
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

// ---------------------------------------------------------------------------
// Full legacy delivery through `handle_push_job`: the captured headers carry
// the actual KDF inputs (proven by decrypting with the header values only).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_aesgcm_delivery_decrypts() {
    use birchtree::application::ports::NotificationStore;

    let srv = TestServer::new(false);
    let alice = token(&srv, "q4_alice", SCOPES);
    let bob_token = token(&srv, "q4_bob", SCOPES);
    let bob = srv
        .store
        .account_by_handle("q4_bob", "example.social")
        .expect("store")
        .expect("bob");

    // A legacy subscription with fresh receiver keys.
    let (private, p256dh, auth) = test_receiver_keys();
    let (addr, captured, handle) = fake_push_endpoint(201).await;
    let app = auth::register_app(
        &*srv.store,
        "Round4 push".to_string(),
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
            false,
            "all".to_string(),
            HashMap::from([("mention".to_string(), true)]),
            None,
            T0,
        )
        .expect("subscribe");
    assert!(!sub.standard, "legacy row");
    let _ = &bob_token;

    // Alice mentions Bob (default policy: delivered, not filtered).
    let body = encode(&[("status", "hi @q4_bob, legacy hello"), ("visibility", "public")]);
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
    assert!(seen.authorization.starts_with("WebPush "), "{}", seen.authorization);
    assert_eq!(seen.content_encoding, "aesgcm");
    assert_eq!(seen.content_type, "application/octet-stream");
    assert_eq!(seen.ttl, push_app::PUSH_TTL_SECS.to_string());
    assert!(seen.crypto_key.starts_with("dh="), "{}", seen.crypto_key);
    assert!(seen.crypto_key.contains(";p256ecdsa="), "{}", seen.crypto_key);
    assert!(seen.encryption.starts_with("salt="), "{}", seen.encryption);
    assert!(!seen.body.is_empty(), "legacy delivery has a body");

    // Decrypt with the HEADER values only: this proves the headers carry the
    // actual KDF inputs (salt + sender key), as upstream's wire requires.
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let salt_raw = enc
        .decode(seen.encryption.trim_start_matches("salt="))
        .expect("header salt");
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&salt_raw);
    let dh_b64 = seen
        .crypto_key
        .split(';')
        .next()
        .expect("dh part")
        .trim_start_matches("dh=");
    let server_raw = enc.decode(dh_b64).expect("header dh");
    let mut server_pub = [0u8; 65];
    server_pub.copy_from_slice(&server_raw);
    let mut body = seen.body.clone();
    let opened = receiver_decrypt_aesgcm(private, &p256dh, &auth, &salt, &server_pub, &mut body);
    let payload: Value = serde_json::from_slice(&opened).expect("payload json");
    assert_eq!(payload["notification_id"], Value::String(notification.id.to_string()));
    assert_eq!(payload["notification_type"], Value::String("mention".into()));
    assert!(payload["title"].is_string(), "{payload}");
    assert!(payload["body"].as_str().expect("body").contains("legacy hello"), "{payload}");
    assert_eq!(payload["access_token"], Value::String(String::new()), "never fabricated");
    assert_eq!(payload["icon"], Value::String(String::new()), "never fabricated");
}

/// Corrupt legacy keys delete the subscription through the job (upstream's
/// invalid-row destroy), exactly like the standard path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_legacy_keys_drop_subscription() {
    use birchtree::application::ports::NotificationStore;

    let srv = TestServer::new(false);
    let alice = token(&srv, "q4c_alice", SCOPES);
    let bob = srv
        .store
        .account_by_handle("q4c_bob", "example.social")
        .expect("store")
        .unwrap_or_else(|| common::mk_account(&srv.store, "q4c_bob"));
    let (addr, _captured, handle) = fake_push_endpoint(201).await;
    let app = auth::register_app(
        &*srv.store,
        "Round4 corrupt".to_string(),
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
    // Present but undecodable keys: the row passes the "has keys" gate and
    // then fails encryption, like upstream's `unless valid? → destroy`.
    let sub = srv
        .store
        .upsert_push_subscription(
            token_row.id,
            bob.id,
            format!("http://{addr}/sub"),
            "corrupt".to_string(),
            "corrupt".to_string(),
            false,
            "all".to_string(),
            HashMap::from([("mention".to_string(), true)]),
            None,
            T0,
        )
        .expect("subscribe");

    let body = encode(&[("status", "hi @q4c_bob, broken keys"), ("visibility", "public")]);
    let (status, value) = call(&srv, "POST", "/api/v1/statuses", &alice, &body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    drain_jobs(&srv);
    let notifications = srv
        .store
        .notifications_page(bob.id, 10, None, None, None)
        .expect("page");
    assert_eq!(notifications.len(), 1);

    let keys = test_vapid_keys();
    let client = reqwest::Client::new();
    let outcome = push_app::handle_push_job(
        &*srv.store,
        &client,
        &keys,
        "mailto:admin@example.social",
        sub.id,
        notifications[0].id,
        now_ms(),
    )
    .await
    .expect("push job");
    assert_eq!(outcome, push_app::PushJobOutcome::DeleteSubscriptionAndComplete(sub.id));
    handle.abort();
}

// ---------------------------------------------------------------------------
// The keyless tickle is preserved: empty body, no `Content-Encoding`, same
// VAPID/TTL headers, and no `Encryption` header on the wire.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tickle_for_keyless_legacy_preserved() {
    let keys = test_vapid_keys();
    let client = reqwest::Client::new();
    let (addr, captured, handle) = fake_push_endpoint(201).await;
    let sub = birchtree::domain::WebPushSubscription {
        id: birchtree::domain::Id::new(21),
        token_id: birchtree::domain::Id::new(22),
        user_id: birchtree::domain::Id::new(23),
        endpoint: format!("http://{addr}/sub"),
        key_p256dh: String::new(),
        key_auth: String::new(),
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
    assert!(request.content_encoding.is_none(), "tickle has no content encoding");
    assert!(request.encryption.is_none(), "tickle has no Encryption header");
    assert!(request.body.is_empty(), "tickle has no body");
    assert_eq!(
        push_app::deliver_push(&client, &request).await,
        push_app::PushOutcome::Delivered
    );
    let seen = captured.lock().unwrap().clone().expect("captured");
    assert!(seen.authorization.starts_with("vapid t="), "{}", seen.authorization);
    assert_eq!(seen.content_encoding, "", "no content-encoding on a tickle");
    assert_eq!(seen.encryption, "", "no encryption header on a tickle");
    assert!(seen.body.is_empty(), "no body on a tickle");
    assert_eq!(seen.ttl, push_app::PUSH_TTL_SECS.to_string());
    handle.abort();
}

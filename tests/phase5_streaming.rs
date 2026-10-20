//! Phase 5 streaming contract tests: real sockets, the real router.
//!
//! Every test serves the router on an ephemeral port and connects with an HTTP
//! client (SSE) or a WebSocket client. Statuses and relationships are created
//! through the REST surface, so the events come from the same commits a client
//! would produce. Covered: auth and error text, each declared stream, the
//! visibility, block, mute, unfollow and delete rules, token revocation, the
//! heartbeat, the caps, and a concurrency check for leakage after a block.

mod common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::http::{Request, StatusCode};
use common::{TEST_PUBLIC_URL, T0, TestServer, form_request, json, send};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

use birchtree::application::auth;
use birchtree::application::ports::AccountStore;
use birchtree::streaming::Limits;

/// Scopes for ordinary client tokens.
const SCOPES: &str = "read write follow read:statuses write:statuses read:notifications \
    read:lists write:lists read:accounts write:accounts read:blocks write:blocks \
    read:mutes write:mutes read:filters";

/// How long an expected event may take. Generous: CI is slow, and the fan-out
/// thread sits between the commit and the socket.
const WAIT: Duration = Duration::from_secs(5);
/// How long to watch for an event that must not arrive.
const QUIET: Duration = Duration::from_millis(500);
/// How long to collect the events one action produces.
const SETTLE: Duration = Duration::from_millis(800);

// ---------------------------------------------------------------------------
// Setup helpers.
// ---------------------------------------------------------------------------

/// Serve the test router on an ephemeral port.
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

/// An account (created on first use) and a user token with `scopes`.
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
        format!("Streams {username}"),
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

/// Upload a 2x2 PNG and return the attachment id (multipart, as clients do).
async fn upload_png(srv: &TestServer, token: &str) -> String {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(2, 2, |x, y| {
        image::Rgba([(x * 90) as u8, (y * 90) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png).expect("encode png");
    let boundary = "birchtree-phase5";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"pixel.png\"\r\nContent-Type: image/png\r\n\r\n",
    );
    body.extend_from_slice(&cursor.into_inner());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/media")
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(body))
        .expect("request");
    let (status, _h, bytes) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().to_string()
}

/// Run the durable jobs the way the worker does (the test server has none):
/// notification fan-out publishes its streaming event from the handler.
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

fn account_id(srv: &TestServer, username: &str) -> String {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
        .to_string()
}

/// `application/x-www-form-urlencoded` encoding of a field list.
fn encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", enc(key), enc(value)))
        .collect::<Vec<_>>()
        .join("&")
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

/// Call the REST surface; returns `(status, body)`.
async fn call(srv: &TestServer, method: &str, path: &str, token: &str, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(method, path, body, Some(token)),
    )
    .await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create a status and return its id (asserts 200).
async fn post(srv: &TestServer, token: &str, text: &str, visibility: &str) -> String {
    let body = encode(&[("status", text), ("visibility", visibility)]);
    let (status, value) = call(srv, "POST", "/api/v1/statuses", token, &body).await;
    assert_eq!(status, StatusCode::OK, "post {text:?}: {value}");
    value["id"].as_str().unwrap().to_string()
}

async fn post_as_reply(srv: &TestServer, token: &str, text: &str, reply_to: &str) -> String {
    let body = encode(&[
        ("status", text),
        ("visibility", "public"),
        ("in_reply_to_id", reply_to),
    ]);
    let (status, value) = call(srv, "POST", "/api/v1/statuses", token, &body).await;
    assert_eq!(status, StatusCode::OK, "reply {text:?}: {value}");
    value["id"].as_str().unwrap().to_string()
}

async fn ok(srv: &TestServer, method: &str, path: &str, token: &str, body: &str) -> Value {
    let (status, value) = call(srv, method, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "{method} {path}: {value}");
    value
}

async fn follow(srv: &TestServer, token: &str, target: &str) {
    ok(srv, "POST", &format!("/api/v1/accounts/{target}/follow"), token, "").await;
}

async fn unfollow(srv: &TestServer, token: &str, target: &str) {
    ok(srv, "POST", &format!("/api/v1/accounts/{target}/unfollow"), token, "").await;
}

async fn block(srv: &TestServer, token: &str, target: &str) {
    ok(srv, "POST", &format!("/api/v1/accounts/{target}/block"), token, "").await;
}

async fn mute(srv: &TestServer, token: &str, target: &str) {
    ok(srv, "POST", &format!("/api/v1/accounts/{target}/mute"), token, "").await;
}

// ---------------------------------------------------------------------------
// SSE client.
// ---------------------------------------------------------------------------

/// One parsed SSE block: a comment, or an event with its data.
#[derive(Debug, Clone, PartialEq)]
enum Block {
    Comment(String),
    Event { name: String, data: String },
}

/// A streaming SSE response read chunk by chunk, parsed line by line. A
/// comment line is a block of its own: the opening `:)` shares a chunk with the
/// first event and has no blank line after it.
struct Sse {
    response: reqwest::Response,
    buffer: String,
    pending: std::collections::VecDeque<Block>,
    name: Option<String>,
    data: Option<String>,
}

impl Sse {
    /// The next block, or `None` when the stream ends or `wait` passes.
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
                    if let Some(name) = self.name.take() {
                        let data = self.data.take().unwrap_or_default();
                        self.pending.push_back(Block::Event { name, data });
                    }
                }
            }
            if !self.pending.is_empty() {
                continue;
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            match tokio::time::timeout(left, self.response.chunk()).await {
                Ok(Ok(Some(chunk))) => self.buffer.push_str(&String::from_utf8_lossy(&chunk)),
                Ok(Ok(None)) | Ok(Err(_)) => return None,
                Err(_) => return None,
            }
        }
    }

    /// Every block that arrives within `wait`.
    async fn collect(&mut self, wait: Duration) -> Vec<Block> {
        let deadline = Instant::now() + wait;
        let mut blocks = Vec::new();
        while let Some(block) = self.next_block(deadline.saturating_duration_since(Instant::now())).await {
            blocks.push(block);
        }
        blocks
    }

    /// The next event, skipping comments.
    async fn next_event(&mut self, wait: Duration) -> Option<(String, String)> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.next_block(left).await? {
                Block::Event { name, data } => return Some((name, data)),
                Block::Comment(_) => continue,
            }
        }
    }
}

/// Open an SSE stream; asserts the 200 and returns the reader.
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

/// The refusal an SSE open gets: status and the `error` text.
async fn sse_refusal(addr: SocketAddr, path: &str, token: Option<&str>) -> (StatusCode, String) {
    let mut request = reqwest::Client::new().get(format!("http://{addr}{path}"));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = request.send().await.expect("sse request");
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    (status, body["error"].as_str().unwrap_or_default().to_string())
}

/// Payload ids of every event called `name` in `blocks`.
fn ids_of(blocks: &[Block], name: &str) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Event { name: event, data } if event == name => {
                Some(serde_json::from_str::<Value>(data).ok()?["id"].as_str()?.to_string())
            }
            _ => None,
        })
        .collect()
}

/// The events in `blocks`, without the comments (the opening `:)` and heartbeats).
fn events_only(blocks: &[Block]) -> Vec<Block> {
    blocks
        .iter()
        .filter(|block| matches!(block, Block::Event { .. }))
        .cloned()
        .collect()
}

/// The `delete` payloads (the bare status id).
fn deletes_of(blocks: &[Block]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Event { name, data } if name == "delete" => Some(data.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Auth, channel names and errors.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_refusals_carry_upstream_status_and_text() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let app_only = {
        let app = auth::register_app(
            &*srv.store,
            "App only".into(),
            vec!["https://client.example/cb".into()],
            vec!["read".into()],
            None,
            true,
            T0,
        )
        .unwrap();
        auth::issue_client_credentials_token(&*srv.store, app.app.id, vec!["read".into()], T0)
            .unwrap()
            .0
    };
    let write_only = {
        let alice = token(&srv, "scopes_alice", "write");
        alice
    };

    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/user", None).await,
        (StatusCode::UNAUTHORIZED, "Missing access token".into())
    );
    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/user", Some("nope")).await,
        (StatusCode::UNAUTHORIZED, "Invalid access token".into())
    );
    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/user", Some(&app_only)).await,
        (StatusCode::UNAUTHORIZED, "Invalid access token".into()),
        "an app-only token has no user to stream"
    );
    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/public", Some(&write_only)).await,
        (
            StatusCode::UNAUTHORIZED,
            "Access token does not have the required scopes".into()
        )
    );
    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/nope", None).await,
        (StatusCode::BAD_REQUEST, "Unknown channel requested".into()),
        "an unknown channel is refused before authentication"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parameter_refusals_match_upstream_text() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "params_alice", SCOPES);
    let bob = token(&srv, "params_bob", SCOPES);
    let bobs_list = {
        let body = encode(&[("title", "Bob's list")]);
        ok(&srv, "POST", "/api/v1/lists", &bob, &body).await["id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/hashtag", Some(&alice)).await,
        (StatusCode::BAD_REQUEST, "Missing tag name parameter".into())
    );
    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/list", Some(&alice)).await,
        (StatusCode::BAD_REQUEST, "Missing list name parameter".into())
    );
    assert_eq!(
        sse_refusal(addr, &format!("/api/v1/streaming/list?list={bobs_list}"), Some(&alice)).await,
        (
            StatusCode::UNAUTHORIZED,
            "Not authorized to stream this list".into()
        ),
        "a list the viewer does not own"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_is_open_and_opens_with_a_comment() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let health = reqwest::get(format!("http://{addr}/api/v1/streaming/health"))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(health.text().await.unwrap(), "OK");

    let alice = token(&srv, "open_alice", SCOPES);
    let mut sse = open_sse(addr, "/api/v1/streaming/user", Some(&alice)).await;
    let first = sse.next_block(WAIT).await.expect("opening comment");
    assert_eq!(first, Block::Comment(")".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_headers_match_upstream() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "headers_alice", SCOPES);
    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/v1/streaming/public"))
        .header("authorization", format!("Bearer {alice}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["cache-control"], "private, no-store");
}

/// The bare host redirect: a separately configured streaming host answers
/// `301` with the same path and query (upstream's StreamingController).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_path_redirects_to_a_configured_host() {
    let (dir, store) = common::temp_store();
    let cfg = birchtree::config::Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        streaming_api_base_url: Some("wss://stream.example.social".to_string()),
        ..birchtree::config::Config::default()
    };
    let server = std::sync::Arc::new(birchtree::api::mastodon::ServerInfo::from_config(&cfg));
    assert!(server.streaming_host.is_some());
    let clock: std::sync::Arc<dyn birchtree::domain::Clock> =
        std::sync::Arc::new(birchtree::domain::SystemClock);
    let ids: std::sync::Arc<dyn birchtree::domain::IdGenerator> = std::sync::Arc::new(
        birchtree::domain::TimestampIdGenerator::new(Box::new(birchtree::domain::SystemClock)),
    );
    let blobs = std::sync::Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.path().join("media")).unwrap(),
    );
    let streams = birchtree::streaming::Hub::new(
        store.clone(),
        clock.clone(),
        server.clone(),
        Limits::default(),
    );
    let context = std::sync::Arc::new(birchtree::application::context::Context::new(
        clock,
        ids,
        store,
        blobs,
        std::sync::Arc::new(streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    let router = birchtree::api::mastodon::build_router(birchtree::api::mastodon::ApiState {
        context,
        trusted_proxies: std::sync::Arc::new(Vec::new()),
        max_body_size: 1024 * 1024,
        server,
        streams,
        federation,
        rate_limit_multiplier: 1,
    });
    let response = send(
        &router,
        Request::builder()
            .uri("/api/v1/streaming/user?access_token=x")
            .header("host", "example.social")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.0, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        response.1.get("location").unwrap().to_str().unwrap(),
        "https://stream.example.social/api/v1/streaming/user?access_token=x"
    );
}

// ---------------------------------------------------------------------------
// Public family.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_stream_carries_public_statuses_and_self_replies_only() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "pub_alice", SCOPES);
    let bob = token(&srv, "pub_bob", SCOPES);
    let viewer = token(&srv, "pub_viewer", SCOPES);
    let mut sse = open_sse(addr, "/api/v1/streaming/public", Some(&viewer)).await;

    let public = post(&srv, &alice, "public post", "public").await;
    post(&srv, &alice, "private post", "private").await;
    post(&srv, &alice, "unlisted post", "unlisted").await;
    post(&srv, &alice, "direct post", "direct").await;
    let bobs = post(&srv, &bob, "bob's public", "public").await;
    post_as_reply(&srv, &alice, "reply to bob", &bobs).await;
    let self_reply = post_as_reply(&srv, &alice, "alice self-reply", &public).await;

    let blocks = sse.collect(QUIET + Duration::from_millis(500)).await;
    let updates = ids_of(&blocks, "update");
    assert!(updates.contains(&public), "public status: {blocks:?}");
    assert!(updates.contains(&bobs));
    assert!(updates.contains(&self_reply), "self-replies stay: {updates:?}");
    assert_eq!(updates.len(), 3, "private, unlisted, direct and replies to others stay out: {updates:?}");

    // Logged-in payloads carry `filtered` and no per-viewer flags (upstream's
    // anonymous render plus the filtered array).
    let Block::Event { data, .. } = blocks
        .iter()
        .find(|b| matches!(b, Block::Event { name, .. } if name == "update"))
        .cloned()
        .unwrap()
    else {
        unreachable!()
    };
    let payload: Value = serde_json::from_str(&data).unwrap();
    assert!(payload["filtered"].is_array(), "{payload}");
    assert!(payload.get("favourited").is_none(), "anonymous render: {payload}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_media_and_hashtag_streams_select_their_statuses() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "sel_alice", SCOPES);
    let viewer = token(&srv, "sel_viewer", SCOPES);
    let mut local = open_sse(addr, "/api/v1/streaming/public/local", Some(&viewer)).await;
    let mut remote = open_sse(addr, "/api/v1/streaming/public/remote", Some(&viewer)).await;
    let mut media = open_sse(addr, "/api/v1/streaming/public?only_media=true", Some(&viewer)).await;
    let mut tag = open_sse(addr, "/api/v1/streaming/hashtag?tag=%23Rust", Some(&viewer)).await;
    let mut tag_local = open_sse(addr, "/api/v1/streaming/hashtag/local?tag=rust", Some(&viewer)).await;

    let plain = post(&srv, &alice, "plain", "public").await;
    let tagged = post(&srv, &alice, "a #rust post", "public").await;

    let local_blocks = local.collect(SETTLE).await;
    assert_eq!(ids_of(&local_blocks, "update"), vec![plain.clone(), tagged.clone()]);
    assert!(ids_of(&remote.collect(QUIET).await, "update").is_empty(), "no remote statuses until federation");
    assert!(ids_of(&media.collect(QUIET).await, "update").is_empty(), "no media, no media stream");
    assert_eq!(
        ids_of(&tag.collect(QUIET + Duration::from_millis(500)).await, "update"),
        vec![tagged.clone()],
        "the tag is normalized: #Rust matches #rust"
    );
    assert_eq!(
        ids_of(&tag_local.collect(QUIET).await, "update"),
        vec![tagged.clone()],
        "hashtag/local carries local tagged statuses"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_stream_carries_only_statuses_with_media() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "media_alice", SCOPES);
    let viewer = token(&srv, "media_viewer", SCOPES);
    // Upstream selects media statuses with `only_media`, not a path.
    let mut media = open_sse(addr, "/api/v1/streaming/public?only_media=true", Some(&viewer)).await;
    let mut all = open_sse(addr, "/api/v1/streaming/public", Some(&viewer)).await;

    let plain = post(&srv, &alice, "no media here", "public").await;
    let media_id = upload_png(&srv, &alice).await;
    let with_media = encode(&[
        ("status", "with a picture"),
        ("visibility", "public"),
        ("media_ids[]", &media_id),
    ]);
    let with_media = ok(&srv, "POST", "/api/v1/statuses", &alice, &with_media).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let everything = ids_of(&all.collect(SETTLE).await, "update");
    assert_eq!(everything, vec![plain, with_media.clone()]);
    assert_eq!(
        ids_of(&media.collect(QUIET).await, "update"),
        vec![with_media],
        "only statuses with attachments"
    );
}

// ---------------------------------------------------------------------------
// User, notifications, direct, list.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_stream_carries_followed_and_own_statuses_until_unfollow() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "home_alice", SCOPES);
    let bob = token(&srv, "home_bob", SCOPES);
    let carol = token(&srv, "home_carol", SCOPES);
    follow(&srv, &bob, &account_id(&srv, "home_alice")).await;
    let mut bob_home = open_sse(addr, "/api/v1/streaming/user", Some(&bob)).await;

    let first = post(&srv, &alice, "from alice", "public").await;
    let own = post(&srv, &bob, "bob's own", "public").await;
    let carols = post(&srv, &carol, "carol is not followed", "public").await;

    let bob_blocks = bob_home.collect(QUIET + Duration::from_millis(400)).await;
    let bob_updates = ids_of(&bob_blocks, "update");
    assert!(bob_updates.contains(&first) && bob_updates.contains(&own), "{bob_updates:?}");
    assert!(!bob_updates.contains(&carols), "unfollowed authors stay out of home");

    unfollow(&srv, &bob, &account_id(&srv, "home_alice")).await;
    post(&srv, &alice, "after unfollow", "public").await;
    let after = bob_home.collect(QUIET + Duration::from_millis(400)).await;
    assert!(ids_of(&after, "update").is_empty(), "no delivery after unfollow: {after:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_stream_delivers_favourites_on_both_user_streams() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "notif_alice", SCOPES);
    let bob = token(&srv, "notif_bob", SCOPES);
    let mut notes = open_sse(addr, "/api/v1/streaming/user/notification", Some(&alice)).await;
    let mut home = open_sse(addr, "/api/v1/streaming/user", Some(&alice)).await;

    let status = post(&srv, &alice, "favourite me", "public").await;
    ok(&srv, "POST", &format!("/api/v1/statuses/{status}/favourite"), &bob, "").await;
    drain_jobs(&srv);

    let note = notes.next_event(WAIT).await.expect("notification event");
    assert_eq!(note.0, "notification");
    let payload: Value = serde_json::from_str(&note.1).unwrap();
    assert_eq!(payload["type"], "favourite");
    assert_eq!(payload["status"]["id"], status.as_str());

    let home_blocks = home.collect(QUIET + Duration::from_millis(400)).await;
    assert!(
        home_blocks.iter().any(|b| matches!(b, Block::Event { name, .. } if name == "notification")),
        "the user stream carries notifications when the token may read them: {home_blocks:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_and_remote_public_streams_authorize_and_stay_quiet() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "quiet_alice", SCOPES);
    let mut direct = open_sse(addr, "/api/v1/streaming/direct", Some(&alice)).await;
    let mut remote = open_sse(addr, "/api/v1/streaming/public/remote", Some(&alice)).await;
    post(&srv, &alice, "nothing for direct", "direct").await;
    // D18 closed (Phase 7): the author's own direct status opens a
    // conversation thread, so `direct` carries one `conversation` event.
    let direct_events = events_only(&direct.collect(QUIET).await);
    let conversation_count = direct_events
        .iter()
        .filter(|block| matches!(block, Block::Event { name, .. } if name == "conversation"))
        .count();
    assert_eq!(
        conversation_count, 1,
        "direct carries the conversation event: {direct_events:?}"
    );
    let remote_events = events_only(&remote.collect(QUIET).await);
    assert!(remote_events.is_empty(), "{remote_events:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_stream_carries_members_and_honors_replies_policy() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let owner = token(&srv, "list_owner", SCOPES);
    let member = token(&srv, "list_member", SCOPES);
    let outsider = token(&srv, "list_outsider", SCOPES);
    let member_id = account_id(&srv, "list_member");
    let list_id = ok(
        &srv,
        "POST",
        "/api/v1/lists",
        &owner,
        &encode(&[("title", "Friends")]),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    // A list member must be followed first (Phase 4 membership rule).
    follow(&srv, &owner, &member_id).await;
    ok(
        &srv,
        "POST",
        &format!("/api/v1/lists/{list_id}/accounts"),
        &owner,
        &encode(&[("account_ids[]", &member_id)]),
    )
    .await;

    let mut list = open_sse(addr, &format!("/api/v1/streaming/list?list={list_id}"), Some(&owner)).await;
    let from_member = post(&srv, &member, "member speaks", "public").await;
    post(&srv, &outsider, "outsider speaks", "public").await;

    let blocks = list.collect(QUIET + Duration::from_millis(400)).await;
    assert_eq!(ids_of(&blocks, "update"), vec![from_member]);
}

// ---------------------------------------------------------------------------
// Edits, deletes and mention edits.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edit_and_delete_reach_the_public_and_home_streams() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "edit_alice", SCOPES);
    let mut public = open_sse(addr, "/api/v1/streaming/public", Some(&alice)).await;
    let mut home = open_sse(addr, "/api/v1/streaming/user", Some(&alice)).await;

    // Each step waits for its event before the next commit. A delete that
    // commits before its create is processed is correctly never announced.
    let status = post(&srv, &alice, "first draft", "public").await;
    let (name, data) = public.next_event(WAIT).await.expect("create");
    assert_eq!((name.as_str(), payload_id(&data)), ("update", status.clone()));

    let edited = encode(&[("status", "second draft")]);
    ok(&srv, "PUT", &format!("/api/v1/statuses/{status}"), &alice, &edited).await;
    let (name, data) = public.next_event(WAIT).await.expect("edit");
    assert_eq!((name.as_str(), payload_id(&data)), ("status.update", status.clone()));

    ok(&srv, "DELETE", &format!("/api/v1/statuses/{status}"), &alice, "").await;
    let (name, data) = public.next_event(WAIT).await.expect("delete");
    assert_eq!((name.as_str(), data.as_str()), ("delete", status.as_str()));

    // The home stream carries the same delete (the author's own status).
    let home_blocks = home.collect(SETTLE).await;
    assert_eq!(deletes_of(&home_blocks), vec![status.clone()]);
    assert!(ids_of(&home_blocks, "update").len() <= 1, "{home_blocks:?}");
}

/// The `id` of a status payload.
fn payload_id(data: &str) -> String {
    serde_json::from_str::<Value>(data).unwrap()["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mentioned_viewers_get_edits_on_their_notification_stream() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "mention_alice", SCOPES);
    let bob = token(&srv, "mention_bob", SCOPES);
    let mut bob_notes = open_sse(addr, "/api/v1/streaming/user/notification", Some(&bob)).await;

    let status = post(&srv, &alice, "hi @mention_bob", "unlisted").await;
    let edited = encode(&[("status", "hi again @mention_bob")]);
    ok(&srv, "PUT", &format!("/api/v1/statuses/{status}"), &alice, &edited).await;

    let blocks = bob_notes.collect(QUIET + Duration::from_millis(500)).await;
    let edits = ids_of(&blocks, "status.update");
    assert_eq!(edits, vec![status], "{blocks:?}");
}

// ---------------------------------------------------------------------------
// Live policy: block, mute, unfollow.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_retracts_home_statuses_and_silences_both_directions() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "blk_alice", SCOPES);
    let bob = token(&srv, "blk_bob", SCOPES);
    follow(&srv, &bob, &account_id(&srv, "blk_alice")).await;
    let mut bob_home = open_sse(addr, "/api/v1/streaming/user", Some(&bob)).await;
    let mut bob_public = open_sse(addr, "/api/v1/streaming/public", Some(&bob)).await;
    let mut alice_public = open_sse(addr, "/api/v1/streaming/public", Some(&alice)).await;

    let before = post(&srv, &alice, "before the block", "public").await;
    assert_eq!(ids_of(&bob_home.collect(SETTLE).await, "update"), vec![before.clone()]);
    ids_of(&bob_public.collect(QUIET).await, "update");
    ids_of(&alice_public.collect(QUIET).await, "update");

    block(&srv, &bob, &account_id(&srv, "blk_alice")).await;

    let retracted = bob_home.collect(QUIET + Duration::from_millis(500)).await;
    assert_eq!(deletes_of(&retracted), vec![before], "the block removes the feed's copy");

    let after = post(&srv, &alice, "after the block", "public").await;
    let bob_home_after = bob_home.collect(QUIET).await;
    assert!(ids_of(&bob_home_after, "update").is_empty(), "no delivery after block: {bob_home_after:?}");
    let bob_public_after = ids_of(&bob_public.collect(QUIET).await, "update");
    assert!(!bob_public_after.contains(&after), "{bob_public_after:?}");

    // The blocked author's own public stream does not get the blocker's posts.
    let bob_after = post(&srv, &bob, "bob after block", "public").await;
    let alice_sees = ids_of(&alice_public.collect(QUIET).await, "update");
    assert!(!alice_sees.contains(&bob_after), "blocked_by hides the blocker: {alice_sees:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mute_retracts_and_hides_on_public_streams() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "mute_alice", SCOPES);
    let viewer = token(&srv, "mute_viewer", SCOPES);
    follow(&srv, &viewer, &account_id(&srv, "mute_alice")).await;
    let mut home = open_sse(addr, "/api/v1/streaming/user", Some(&viewer)).await;
    let mut public = open_sse(addr, "/api/v1/streaming/public", Some(&viewer)).await;

    let before = post(&srv, &alice, "seen before the mute", "public").await;
    let seen = home.collect(SETTLE).await;
    assert!(ids_of(&seen, "update").contains(&before));
    public.collect(QUIET).await;

    mute(&srv, &viewer, &account_id(&srv, "mute_alice")).await;
    assert_eq!(deletes_of(&home.collect(QUIET + Duration::from_millis(500)).await), vec![before]);

    post(&srv, &alice, "after the mute", "public").await;
    assert!(ids_of(&home.collect(QUIET).await, "update").is_empty());
    assert!(ids_of(&public.collect(QUIET).await, "update").is_empty());
}

// ---------------------------------------------------------------------------
// Revocation and WebSocket.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_a_token_closes_its_streams() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    // The revoke endpoint authenticates the client, so the token belongs to an
    // app registered through the API.
    let app = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/apps",
            &encode(&[
                ("client_name", "Revoker"),
                ("redirect_uris", "https://client.example/cb"),
                ("scopes", "read write"),
            ]),
            None,
        ),
    )
    .await;
    let app_json = json(&app.2);
    let client_id = app_json["client_id"].as_str().unwrap().to_string();
    let client_secret = app_json["client_secret"].as_str().unwrap().to_string();
    let account = common::mk_account(&srv.store, "revoke_alice");
    let app_row = birchtree::application::ports::OAuthStore::app_by_client_id(&*srv.store, &client_id)
        .unwrap()
        .unwrap();
    let (plaintext, _) = auth::issue_user_token(
        &*srv.store,
        app_row.id,
        account.id,
        SCOPES.split_whitespace().map(str::to_string).collect(),
        T0,
    )
    .unwrap();

    let mut sse = open_sse(addr, "/api/v1/streaming/user", Some(&plaintext)).await;
    let (ws, _) = connect_ws(addr, "/api/v1/streaming?stream=user", Some(&plaintext))
        .await
        .expect("ws connect");
    let (mut ws_tx, mut ws_rx) = ws.split();

    let revoke = encode(&[
        ("client_id", &client_id),
        ("client_secret", &client_secret),
        ("token", &plaintext),
    ]);
    let (status, _h, _b) = send(
        &srv.router,
        form_request("POST", "/oauth/revoke", &revoke, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let started = Instant::now();
    let ended = loop {
        match sse.next_block(WAIT).await {
            Some(Block::Comment(_)) => continue,
            Some(event) => break Some(event),
            None => break None,
        }
    };
    assert!(ended.is_none(), "the SSE response ends after revoke: {ended:?}");
    assert!(started.elapsed() < WAIT, "the stream closed, rather than timing out");
    let frame = tokio::time::timeout(WAIT, ws_rx.next()).await.expect("ws reacts");
    assert!(
        matches!(frame, None | Some(Ok(Message::Close(_))) | Some(Err(_))),
        "the WebSocket closes: {frame:?}"
    );
    let _ = ws_tx.close().await;
}

async fn connect_ws(
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        tungstenite::http::Response<Option<Vec<u8>>>,
    ),
    tungstenite::Error,
> {
    let mut request = format!("ws://{addr}{path}").into_client_request()?;
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await
}

/// Read the next text frame as JSON, within `WAIT`.
async fn next_text<S>(rx: &mut S) -> Option<Value>
where
    S: futures_util::Stream<Item = Result<Message, tungstenite::Error>> + Unpin,
{
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match tokio::time::timeout(left, rx.next()).await.ok()? {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).ok(),
            Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
            _ => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_protocol_subscribes_by_query_and_by_message() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "ws_alice", SCOPES);
    let viewer = token(&srv, "ws_viewer", SCOPES);

    let (ws, _) = connect_ws(addr, "/api/v1/streaming?stream=public", Some(&viewer))
        .await
        .expect("connect");
    let (mut tx, mut rx) = ws.split();

    let public = post(&srv, &alice, "ws public", "public").await;
    let frame = next_text(&mut rx).await.expect("update frame");
    assert_eq!(frame["stream"], serde_json::json!(["public"]));
    assert_eq!(frame["event"], "update");
    let payload: Value = serde_json::from_str(frame["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["id"], public.as_str());

    // Subscribe to a hashtag by message: the frame's stream carries the tag.
    tx.send(Message::Text(
        r##"{"type":"subscribe","stream":"hashtag","tag":"#Ferris"}"##.into(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let tagged = post(&srv, &alice, "a #ferris day", "public").await;
    let mut seen = Vec::new();
    for _ in 0..3 {
        if let Some(frame) = next_text(&mut rx).await {
            seen.push(frame);
            if seen.len() == 2 {
                break;
            }
        }
    }
    let hashtag_frame = seen
        .iter()
        .find(|f| f["stream"] == serde_json::json!(["hashtag", "#Ferris"]))
        .unwrap_or_else(|| panic!("hashtag frame: {seen:?}"));
    let payload: Value = serde_json::from_str(hashtag_frame["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["id"], tagged.as_str());

    // Unsubscribe the hashtag: no further hashtag frames.
    tx.send(Message::Text(
        r##"{"type":"unsubscribe","stream":"hashtag","tag":"#Ferris"}"##.into(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    post(&srv, &alice, "a #ferris day two", "public").await;
    let next = tokio::time::timeout(QUIET, rx.next()).await;
    if let Ok(Some(Ok(Message::Text(text)))) = next {
        let frame: Value = serde_json::from_str(&text).unwrap();
        assert_ne!(frame["stream"], serde_json::json!(["hashtag", "#Ferris"]), "{frame}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_errors_come_back_as_error_frames() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let viewer = token(&srv, "wserr_viewer", SCOPES);
    let (ws, _) = connect_ws(addr, "/api/v1/streaming", Some(&viewer)).await.unwrap();
    let (mut tx, mut rx) = ws.split();

    tx.send(Message::Text(r#"{"type":"subscribe","stream":"nope"}"#.into()))
        .await
        .unwrap();
    assert_eq!(
        next_text(&mut rx).await.unwrap(),
        serde_json::json!({"error": "Unknown stream type", "status": 400})
    );

    tx.send(Message::Text(r#"{"type":"subscribe","stream":"hashtag"}"#.into()))
        .await
        .unwrap();
    assert_eq!(
        next_text(&mut rx).await.unwrap(),
        serde_json::json!({"error": "Missing tag name parameter", "status": 400})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_refuses_the_upgrade_with_an_error_header() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let refused = connect_ws(addr, "/api/v1/streaming?stream=user", None).await;
    match refused {
        Err(tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()["x-error-message"], "Missing access token");
        }
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Bounds: caps, heartbeat, stalls.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_caps_refuse_with_the_documented_status() {
    let srv = TestServer::with_limits(Limits {
        max_connections_per_token: 2,
        max_connections: 8,
        ..Limits::default()
    });
    let addr = serve(&srv).await;
    let alice = token(&srv, "cap_alice", SCOPES);
    let _a = open_sse(addr, "/api/v1/streaming/user", Some(&alice)).await;
    let _b = open_sse(addr, "/api/v1/streaming/public", Some(&alice)).await;
    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/user", Some(&alice)).await,
        (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many streaming connections for this token".into()
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_per_websocket_are_capped() {
    let srv = TestServer::with_limits(Limits {
        max_subscriptions_per_connection: 2,
        ..Limits::default()
    });
    let addr = serve(&srv).await;
    let viewer = token(&srv, "subcap_viewer", SCOPES);
    let (ws, _) = connect_ws(addr, "/api/v1/streaming", Some(&viewer)).await.unwrap();
    let (mut tx, mut rx) = ws.split();
    for stream in ["public", "public:local", "public:remote"] {
        tx.send(Message::Text(
            serde_json::json!({ "type": "subscribe", "stream": stream }).to_string().into(),
        ))
        .await
        .unwrap();
    }
    assert_eq!(
        next_text(&mut rx).await.unwrap(),
        serde_json::json!({"error": "Too many streaming subscriptions", "status": 429})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_is_sent_on_an_idle_stream() {
    let srv = TestServer::with_limits(Limits {
        sse_heartbeat: Duration::from_millis(80),
        ..Limits::default()
    });
    let addr = serve(&srv).await;
    let viewer = token(&srv, "beat_viewer", SCOPES);
    let mut sse = open_sse(addr, "/api/v1/streaming/public", Some(&viewer)).await;
    assert_eq!(sse.next_block(WAIT).await, Some(Block::Comment(")".into())));
    assert_eq!(
        sse.next_block(WAIT).await,
        Some(Block::Comment("thump".into())),
        "the idle heartbeat"
    );
}

/// The hub's bounds, exercised without sockets: a session that never polls
/// its outbox cannot grow its queue past `queue_frames`. The connection is
/// closed rather than buffered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_never_reads_is_disconnected_at_the_queue_bound() {
    use birchtree::streaming::channels::{Locality, StreamKind};
    let srv = TestServer::with_limits(Limits {
        queue_frames: 2,
        stall_timeout: Duration::from_secs(60),
        ..Limits::default()
    });
    let alice = token(&srv, "queue_alice", SCOPES);
    let _ = token(&srv, "queue_viewer", SCOPES);
    let viewer = account_of(&srv, "queue_viewer");
    let (session, _outbox) = srv
        .streams
        .register(
            birchtree::domain::Id::new(1),
            viewer,
            SCOPES.split_whitespace().map(str::to_string).collect(),
        )
        .expect("register");
    session
        .subscribe(
            StreamKind::Public { locality: Locality::Any, media: false },
            Vec::new(),
        )
        .expect("subscribe");
    for n in 0..4 {
        post(&srv, &alice, &format!("flood {n}"), "public").await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(session.connection().is_closed(), "the bound closes the connection");
    assert_eq!(srv.streams.stats().connections, 0);
}

fn account_of(srv: &TestServer, username: &str) -> birchtree::domain::Id {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
}

// ---------------------------------------------------------------------------
// Leakage under concurrent change, in-flight frames and the overflow and stall
// rules.
// ---------------------------------------------------------------------------

/// The §8 item 8 race: while the author posts and deletes concurrently with
/// the viewer's follow and unfollow churn, a block commits. Every status the
/// author creates after the block returns must never reach the viewer, on the
/// home stream or the public stream. An unfollow is checked the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_follow_unfollow_block_and_delete_never_leak_after_commit() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let author = token(&srv, "race_author", SCOPES);
    let viewer = token(&srv, "race_viewer", SCOPES);
    let author_id = account_id(&srv, "race_author");
    let viewer_id = account_id(&srv, "race_viewer");
    follow(&srv, &viewer, &author_id).await;
    let mut home = open_sse(addr, "/api/v1/streaming/user", Some(&viewer)).await;
    let mut public = open_sse(addr, "/api/v1/streaming/public", Some(&viewer)).await;

    // Storm: the author posts and deletes; the viewer toggles follow and
    // unfollow. Deliveries from this phase may or may not arrive.
    let storm_router = srv.router.clone();
    let storm_author = author.clone();
    let storm = tokio::spawn(async move {
        for n in 0..20 {
            let body = encode(&[("status", &format!("storm {n}")), ("visibility", "public")]);
            let (status, _h, bytes) = send(
                &storm_router,
                form_request("POST", "/api/v1/statuses", &body, Some(&storm_author)),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            if n % 3 == 0 {
                let id = json(&bytes)["id"].as_str().unwrap().to_string();
                let _ = send(
                    &storm_router,
                    form_request("DELETE", &format!("/api/v1/statuses/{id}"), "", Some(&storm_author)),
                )
                .await;
            }
        }
    });
    let churn_router = srv.router.clone();
    let churn_viewer = viewer.clone();
    let churn_target = author_id.clone();
    let churn = tokio::spawn(async move {
        for _ in 0..10 {
            for path in [
                format!("/api/v1/accounts/{churn_target}/unfollow"),
                format!("/api/v1/accounts/{churn_target}/follow"),
            ] {
                let _ = send(&churn_router, form_request("POST", &path, "", Some(&churn_viewer))).await;
            }
        }
    });
    storm.await.unwrap();
    churn.await.unwrap();

    // The block commits. Every status created after it returns is a leak if
    // the viewer's stream carries it.
    block(&srv, &viewer, &author_id).await;
    let mut after = Vec::new();
    for n in 0..10 {
        after.push(post(&srv, &author, &format!("after block {n}"), "public").await);
    }
    let home_events = ids_of(&home.collect(SETTLE).await, "update");
    let public_events = ids_of(&public.collect(SETTLE).await, "update");
    for id in &after {
        assert!(!home_events.contains(id), "home leaked {id} after the block returned");
        assert!(!public_events.contains(id), "public leaked {id} after the block returned");
    }
    // Retractions are deletes; no status is re-announced after the block.
    assert!(ids_of(&home.collect(QUIET).await, "status.update").is_empty());
    let _ = viewer_id;
}

/// The policy-epoch rule: a frame queued before a visibility-removing change is
/// dropped at the transport, so a block returned before the transport read it
/// is never followed by that frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_policy_change_drops_frames_queued_before_it() {
    use birchtree::streaming::channels::{Locality, StreamKind};
    let srv = TestServer::new(false);
    let alice = token(&srv, "epoch_alice", SCOPES);
    let bob = token(&srv, "epoch_bob", SCOPES);
    let viewer = token(&srv, "epoch_viewer", SCOPES);
    let alice_id = account_id(&srv, "epoch_alice");
    let viewer_id = account_of(&srv, "epoch_viewer");
    let (session, mut outbox) = srv
        .streams
        .register(
            birchtree::domain::Id::new(9),
            viewer_id,
            SCOPES.split_whitespace().map(str::to_string).collect(),
        )
        .expect("register");
    session
        .subscribe(StreamKind::Public { locality: Locality::Any, media: false }, Vec::new())
        .expect("subscribe");
    let _ = viewer;

    // Queued under the current epoch; the outbox is not read yet.
    post(&srv, &alice, "queued before the block", "public").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    block(&srv, &viewer, &alice_id).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let stale = tokio::time::timeout(QUIET, outbox.next()).await;
    assert!(stale.is_err(), "the pre-block frame must be dropped: {stale:?}");

    // A fresh author's status, under the new epoch, is delivered.
    let bob_id = account_id(&srv, "epoch_bob");
    let fresh = post(&srv, &bob, "fresh after the block", "public").await;
    let frame = tokio::time::timeout(WAIT, outbox.next())
        .await
        .expect("fresh frame")
        .expect("frame");
    let payload: Value = serde_json::from_str(&frame.payload).unwrap();
    assert_eq!(payload["id"], fresh.as_str());
    let _ = bob_id;
}

/// Event overflow: when the publish queue is full, the hub closes every stream
/// (fail closed) rather than dropping the event silently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_queue_overflow_closes_every_stream() {
    use birchtree::application::ports::StreamingSink;
    use birchtree::application::streaming::StreamEvent;
    let srv = TestServer::with_limits(Limits {
        event_queue: 1,
        ..Limits::default()
    });
    let viewer = token(&srv, "overflow_viewer", SCOPES);
    let viewer_id = account_of(&srv, "overflow_viewer");
    let (session, _outbox) = srv
        .streams
        .register(
            birchtree::domain::Id::new(3),
            viewer_id,
            SCOPES.split_whitespace().map(str::to_string).collect(),
        )
        .expect("register");
    let _ = viewer;
    for _ in 0..20_000 {
        srv.streams.publish(StreamEvent::PolicyChanged { viewers: Vec::new() });
    }
    assert!(srv.streams.stats().dropped_events > 0, "the queue must have overflowed");
    tokio::time::timeout(WAIT, session.connection().closed())
        .await
        .expect("the overflow closes the connection");
}

/// The stall rule: frames queued and no transport progress for `stall_timeout`
/// close the connection, so a stalled client cannot pin its queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_transport_is_closed_after_the_stall_timeout() {
    use birchtree::streaming::channels::{Locality, StreamKind};
    let srv = TestServer::with_limits(Limits {
        stall_timeout: Duration::from_millis(200),
        ..Limits::default()
    });
    let alice = token(&srv, "stall_alice", SCOPES);
    let viewer = token(&srv, "stall_viewer", SCOPES);
    let _ = viewer;
    let viewer_id = account_of(&srv, "stall_viewer");
    let (session, _outbox) = srv
        .streams
        .register(
            birchtree::domain::Id::new(4),
            viewer_id,
            SCOPES.split_whitespace().map(str::to_string).collect(),
        )
        .expect("register");
    session
        .subscribe(StreamKind::Public { locality: Locality::Any, media: false }, Vec::new())
        .expect("subscribe");
    post(&srv, &alice, "never read", "public").await;
    tokio::time::timeout(WAIT, session.connection().closed())
        .await
        .expect("the sweep closes a stalled connection");
    assert_eq!(srv.streams.stats().connections, 0);
}

/// Upstream's streaming check covers the mentions too: a viewer who mutes a
/// mentioned account does not get a public status that mentions them (the
/// author is covered by the author gate).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_stream_hides_statuses_that_mention_a_muted_account() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "mentionmute_alice", SCOPES);
    let viewer = token(&srv, "mentionmute_viewer", SCOPES);
    let _target = token(&srv, "mentionmute_target", SCOPES);
    mute(&srv, &viewer, &account_id(&srv, "mentionmute_target")).await;
    let mut public = open_sse(addr, "/api/v1/streaming/public", Some(&viewer)).await;

    post(&srv, &alice, "hi @mentionmute_target", "public").await;
    let plain = post(&srv, &alice, "no mention here", "public").await;

    let seen = ids_of(&public.collect(SETTLE).await, "update");
    assert_eq!(seen, vec![plain], "the mentioning status is hidden");
}

/// The total connection cap refuses with `503`, whatever the token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn total_connection_cap_refuses_with_503() {
    let srv = TestServer::with_limits(Limits {
        max_connections: 1,
        ..Limits::default()
    });
    let addr = serve(&srv).await;
    let first = token(&srv, "total_first", SCOPES);
    let second = token(&srv, "total_second", SCOPES);
    let _held = open_sse(addr, "/api/v1/streaming/user", Some(&first)).await;
    assert_eq!(
        sse_refusal(addr, "/api/v1/streaming/user", Some(&second)).await,
        (StatusCode::SERVICE_UNAVAILABLE, "Too many streaming connections".into())
    );
}

/// A WebSocket whose peer stops answering pings is closed (upstream: a
/// connection that did not pong since the last ping terminates).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_websocket_ping_closes_the_connection() {
    let srv = TestServer::with_limits(Limits {
        ws_ping: Duration::from_millis(150),
        ..Limits::default()
    });
    let addr = serve(&srv).await;
    let viewer = token(&srv, "ping_viewer", SCOPES);
    let (ws, _) = connect_ws(addr, "/api/v1/streaming?stream=public", Some(&viewer))
        .await
        .expect("connect");
    // Hold the socket without reading: tungstenite answers pings only when it
    // reads, so the server's second ping goes unanswered.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (_tx, mut rx) = ws.split();
    let deadline = Instant::now() + WAIT;
    let mut closed = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.next()).await {
            Ok(Some(Ok(Message::Ping(_)))) => continue,
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Err(_) => continue,
        }
    }
    assert!(closed, "the unanswered ping closes the socket");
}

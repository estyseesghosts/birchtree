//! AP-B1 timeline filtering (aperture): photo-first timeline scoping.
//!
//! Stage 2 aperture mode scopes the home, public and tag timelines to media:
//! when `aperture_mode` is on, text-only top-level statuses leave those
//! feeds while photo (media-carrying) statuses stay. Replies are seeded so
//! the top-level-vs-reply distinction stays observable.
//!
//! Contract under test (parent wires the forcing; see the report):
//! - flag OFF (`aperture_mode = false`, Mastodon parity): every seeded
//!   status appears in its feeds.
//! - flag ON (`aperture_mode = true`): text-only top-levels drop out of
//!   home + public + tag; the photo status stays.
//!
//! The flag-OFF tests below are live and must stay green. The flag-ON tests
//! are `//! (`ServerInfo::aperture_mode` mirrors `Config::aperture_mode`, no read
//! path consults it yet), so they fail until the parent enables the
//! `only_media` forcing. Run them with
//! `cargo test --test aperture_b1_timelines -- --ignored`.
//!
//! New file only: all helpers are copied in (multipart/upload pattern from
//! `tests/phase4_media.rs`, timeline helpers from `tests/phase4_timelines.rs`).
//! No shared file is edited or imported beyond `tests/common`.

mod common;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use birchtree::application::ports::AccountStore;
use serde_json::Value;

use common::{TestServer, form_request, get_request, json, send};

const ALL_SCOPES: &str = "read write follow profile read:accounts write:accounts \
                          read:lists write:lists read:statuses write:statuses write:media";

/// Tag (already normalized, lowercase, no `#`) carried by the photo and the
/// text-only top-level so the tag timeline covers both.
const TAG: &str = "apertureb1tag";

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

async fn register_app(srv: &TestServer, scopes: &str) -> Value {
    let body = format!(
        "client_name=ApertureB1&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(srv: &TestServer, app: &Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn register_account(srv: &TestServer, app: &Value, username: &str) -> String {
    let app_tok = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

/// A user token carrying every scope the timeline routes read or write.
async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(srv, ALL_SCOPES).await;
    register_account(srv, &app, username).await
}

// ---------------------------------------------------------------------------
// Multipart/upload helpers (pattern from tests/phase4_media.rs).
// ---------------------------------------------------------------------------

const BOUNDARY: &str = "birchtree-test-boundary";

/// A valid 3x2 PNG encoded with the server's own image stack.
fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 2, |x, y| {
        image::Rgba([(x * 40 + 10) as u8, (y * 90 + 20) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

/// Upload a PNG as `token` and return the media attachment id.
async fn upload_png(srv: &TestServer, token: &str) -> String {
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"pixel.png\"\r\nContent-Type: image/png\r\n\r\n",
    );
    body.extend_from_slice(&png_bytes());
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/media")
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body))
        .expect("multipart request");
    let (status, _h, bytes) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Status/timeline helpers (pattern from tests/phase4_timelines.rs).
// ---------------------------------------------------------------------------

/// Form body for a status: encoded text, visibility and extra fields.
fn status_form(text: &str, visibility: &str, extra: &str) -> String {
    let mut body = format!("status={}&visibility={visibility}", enc(text));
    if !extra.is_empty() {
        body.push('&');
        body.push_str(extra);
    }
    body
}

/// POST a form as `token`, asserting 200 and returning the body.
async fn post_ok(srv: &TestServer, path: &str, token: &str, body: &str) -> Value {
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", path, body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "POST {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn create_with(srv: &TestServer, token: &str, body: &str) -> String {
    post_ok(srv, "/api/v1/statuses", token, body).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// GET a URI and return `(status, headers, parsed body)`.
async fn get(srv: &TestServer, uri: &str, token: Option<&str>) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, bytes) = send(&srv.router, get_request(uri, token, None)).await;
    (status, headers, json(&bytes))
}

/// GET a timeline and assert 200, returning the status ids in response order.
async fn ids_of(srv: &TestServer, uri: &str, token: Option<&str>) -> Vec<String> {
    let (status, _h, body) = get(srv, uri, token).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {body}");
    ids(&body)
}

fn ids(body: &Value) -> Vec<String> {
    body.as_array()
        .unwrap_or_else(|| panic!("array expected: {body}"))
        .iter()
        .map(|status| status["id"].as_str().unwrap().to_string())
        .collect()
}

async fn follow(srv: &TestServer, token: &str, target: &str, form: &str) {
    post_ok(srv, &format!("/api/v1/accounts/{target}/follow"), token, form).await;
}

fn account_id(srv: &TestServer, username: &str) -> String {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
        .to_string()
}

// ---------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------

fn server_with_aperture(aperture_mode: bool) -> TestServer {
    TestServer::with_config(true, |cfg| {
        cfg.aperture_mode = aperture_mode;
    })
}

struct Seed {
    alice: String,
    photo: String,
    text_top: String,
    #[allow(dead_code)]
    reply: String,
}

/// Flag-ON seed: identical to [`seed`], except the text-only top-level is
/// inserted directly through the store (grandfathered pre-flag row —
/// posting it via the API would 422 under the wired write gate).
async fn seed_on(srv: &TestServer) -> Seed {
    use birchtree::application::ports::{NewStatus, StatusStore};
    use birchtree::domain::Visibility;

    let alice = user_token(srv, "alice").await;
    let bob = user_token(srv, "bob").await;
    follow(srv, &alice, &account_id(srv, "bob"), "").await;

    let media_id = upload_png(srv, &bob).await;
    let photo = create_with(
        srv,
        &bob,
        &status_form(
            &format!("#{TAG} photo post"),
            "public",
            &format!("media_ids[]={}", enc(&media_id)),
        ),
    )
    .await;
    // Grandfathered text-only top-level: store-level insert, bypassing the
    // application gate exactly as pre-flag rows predate it.
    let bob_id: birchtree::domain::Id = account_id(srv, "bob").parse().expect("bob id");
    let stored = srv
        .store
        .create_status(
            NewStatus {
                account_id: bob_id,
                uri_base: format!("{}/users/bob/statuses", common::TEST_PUBLIC_URL),
                url_base: format!("{}/@bob", common::TEST_PUBLIC_URL),
                source_text: format!("#{TAG} text-only top-level"),
                content_html: format!("<p>#{TAG} text-only top-level</p>"),
                visibility: Visibility::Public,
                tags: vec![TAG.to_string()],
                ..NewStatus::default()
            },
            common::T0,
        )
        .expect("grandfathered insert")
        .status;
    let text_top = stored.id.to_string();
    let reply = create_with(
        srv,
        &bob,
        &status_form("self reply", "public", &format!("in_reply_to_id={photo}")),
    )
    .await;
    Seed { alice, photo, text_top, reply }
}

/// Alice (viewer) follows bob (author). Bob posts:
/// - `photo`: top-level public status with a PNG attachment (+ `TAG`),
/// - `text_top`: text-only top-level public status (+ `TAG`),
/// - `reply`: text-only self-reply to the photo (kept by the public reply
///   rule and the tag feed, so the top-level-vs-reply distinction is
///   observable in every feed under test).
async fn seed(srv: &TestServer) -> Seed {
    let alice = user_token(srv, "alice").await;
    let bob = user_token(srv, "bob").await;
    follow(srv, &alice, &account_id(srv, "bob"), "").await;

    let media_id = upload_png(srv, &bob).await;
    let photo = create_with(
        srv,
        &bob,
        &status_form(
            &format!("#{TAG} photo post"),
            "public",
            &format!("media_ids[]={}", enc(&media_id)),
        ),
    )
    .await;
    let text_top = create_with(
        srv,
        &bob,
        &status_form(&format!("#{TAG} text-only top-level"), "public", ""),
    )
    .await;
    let reply = create_with(
        srv,
        &bob,
        &status_form("self reply", "public", &format!("in_reply_to_id={photo}")),
    )
    .await;
    Seed { alice, photo, text_top, reply }
}

// ---------------------------------------------------------------------------
// Flag OFF (Mastodon parity): everything seeded is visible. Live, green.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn home_includes_text_top_level_when_off() {
    let srv = server_with_aperture(false);
    let seed = seed(&srv).await;
    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&seed.alice)).await;
    assert!(got.contains(&seed.photo), "photo must be on home when OFF: {got:?}");
    assert!(got.contains(&seed.text_top), "text top-level must be on home when OFF: {got:?}");
    assert!(got.contains(&seed.reply), "self-reply must be on home when OFF: {got:?}");
}

#[tokio::test]
async fn public_includes_text_top_level_when_off() {
    let srv = server_with_aperture(false);
    let seed = seed(&srv).await;
    let got = ids_of(&srv, "/api/v1/timelines/public", None).await;
    assert!(got.contains(&seed.photo), "photo must be on public when OFF: {got:?}");
    assert!(got.contains(&seed.text_top), "text top-level must be on public when OFF: {got:?}");
    assert!(got.contains(&seed.reply), "self-reply must be on public when OFF: {got:?}");
}

#[tokio::test]
async fn tag_includes_text_top_level_when_off() {
    let srv = server_with_aperture(false);
    let seed = seed(&srv).await;
    let got = ids_of(&srv, &format!("/api/v1/timelines/tag/{TAG}"), None).await;
    assert!(got.contains(&seed.photo), "photo must be on tag when OFF: {got:?}");
    assert!(got.contains(&seed.text_top), "text top-level must be on tag when OFF: {got:?}");
}

// ---------------------------------------------------------------------------
// Flag ON (aperture): text-only top-levels leave home + public + tag, the
// photo stays. `// `only_media` forcing (currently the flag is skeleton-only, so they fail).
// The reply is seeded but deliberately unasserted here: whether aperture
// keeps text-only replies (spec language is "text-only top-levels") is an
// open wiring question for the parent (see report).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn home_excludes_text_top_level_when_on() {
    let srv = server_with_aperture(true);
    let seed = seed_on(&srv).await;
    let got = ids_of(&srv, "/api/v1/timelines/home", Some(&seed.alice)).await;
    assert!(got.contains(&seed.photo), "photo must stay on home when ON: {got:?}");
    assert!(!got.contains(&seed.text_top), "text top-level must leave home when ON: {got:?}");
}

#[tokio::test]
async fn public_excludes_text_top_level_when_on() {
    let srv = server_with_aperture(true);
    let seed = seed_on(&srv).await;
    let got = ids_of(&srv, "/api/v1/timelines/public", None).await;
    assert!(got.contains(&seed.photo), "photo must stay on public when ON: {got:?}");
    assert!(!got.contains(&seed.text_top), "text top-level must leave public when ON: {got:?}");
}

#[tokio::test]
async fn tag_excludes_text_top_level_when_on() {
    let srv = server_with_aperture(true);
    let seed = seed_on(&srv).await;
    let got = ids_of(&srv, &format!("/api/v1/timelines/tag/{TAG}"), None).await;
    assert!(got.contains(&seed.photo), "photo must stay on tag when ON: {got:?}");
    assert!(!got.contains(&seed.text_top), "text top-level must leave tag when ON: {got:?}");
}

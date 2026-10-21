//! AP-A2 (aperture write gate): HTTP-level matrix, flag off vs on.
//!
//! Stage 2 aperture mode (`Config::aperture_mode`, default `false`) is a
//! photo-first policy: a top-level non-DM write with no attachments is
//! rejected, while DMs and replies (resolvable parent) are exempt, an
//! unresolvable reply parent keeps the existing 404 path, and a PUT may not
//! strip the last attachment.
//!
//! The pure gate fn is sibling-owned:
//!
//! ```ignore
//! // TODO(AP-A2-parent): when src/application/aperture.rs lands, flip this
//! // single import on and add direct truth-table unit tests beside the HTTP
//! // matrix below:
//! // use birchtree::application::aperture::check_aperture_write;
//! ```
//!
//! Signature (pinned): `check_aperture_write(aperture, direct, reply,
//! has_media)`.
//!
//! Wiring is parent-owned (`create_status` / `update_status` call sites), so
//! this file asserts CURRENT (flag-off) behavior in all passing tests, plus
//! `//! parent will enable once the gate is wired. The ignored tests are written
//! against the documented contract; they FAIL today (flag on changes
//! nothing yet) and must go green when the parent un-ignores them.
//!
//! No gate logic is replicated here: every assertion is an HTTP
//! status/body through the real router.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;

use common::{TestServer, form_request, json, send};

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses \
                          write:statuses write:media";

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
        "client_name=Test+App&redirect_uris={}&scopes={}",
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
    let app = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.social")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn setup(aperture_mode: bool, username: &str) -> (TestServer, String) {
    let srv = TestServer::with_config(true, |cfg| {
        cfg.aperture_mode = aperture_mode;
    });
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, username).await;
    (srv, token)
}

// --- media upload helpers (for the with-media / edit-strip cases) ---

const BOUNDARY: &str = "birchtree-aperture-test-boundary";

struct Part {
    name: &'static str,
    filename: Option<String>,
    mime: Option<&'static str>,
    value: Vec<u8>,
}

fn file(name: &'static str, filename: &str, mime: &'static str, bytes: Vec<u8>) -> Part {
    Part { name, filename: Some(filename.to_string()), mime: Some(mime), value: bytes }
}

fn multipart_request(method: &str, uri: &str, token: Option<&str>, parts: &[Part]) -> Request<Body> {
    let mut body: Vec<u8> = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match &part.filename {
            Some(filename) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\nContent-Type: {}\r\n\r\n",
                    part.name,
                    part.mime.unwrap_or("application/octet-stream")
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", part.name).as_bytes(),
            ),
        }
        body.extend_from_slice(&part.value);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    let mut builder = Request::builder().method(method).uri(uri).header(
        "content-type",
        format!("multipart/form-data; boundary={BOUNDARY}"),
    );
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body)).expect("multipart request")
}

fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 2, |x, y| {
        image::Rgba([(x * 40 + 10) as u8, (y * 90 + 20) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png).expect("encode png fixture");
    cursor.into_inner()
}

async fn upload_image(srv: &TestServer, token: &str) -> Value {
    let parts = vec![file("file", "pixel.png", "image/png", png_bytes())];
    let (status, _h, bytes) =
        send(&srv.router, multipart_request("POST", "/api/v1/media", Some(token), &parts)).await;
    assert_eq!(status, StatusCode::OK, "upload: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn post_body(
    srv: &TestServer,
    token: &str,
    body: &str,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let (status, headers, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/statuses", body, Some(token))).await;
    (status, headers, json(&bytes))
}

// ---------------------------------------------------------------------------
// Flag OFF: current behavior. The gate is not wired, so every well-formed
// write succeeds regardless of attachments. These all PASS today.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flag_off_top_level_text_only_posts() {
    let (srv, token) = setup(false, "apofftop").await;
    let (status, _, body) = post_body(&srv, &token, "status=hello+world").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["id"].as_str().is_some(), "{body}");
}

#[tokio::test]
async fn flag_off_reply_text_only_posts() {
    let (srv, token) = setup(false, "apoffreply").await;
    let (_, _, parent) = post_body(&srv, &token, "status=parent").await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let (status, _, body) =
        post_body(&srv, &token, &format!("status=reply&in_reply_to_id={}", enc(&parent_id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["in_reply_to_id"], serde_json::json!(parent_id), "{body}");
}

#[tokio::test]
async fn flag_off_dm_text_only_posts() {
    let (srv, token) = setup(false, "apoffdm").await;
    let (status, _, body) =
        post_body(&srv, &token, "status=secret&visibility=direct").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["visibility"], serde_json::json!("direct"), "{body}");
}

#[tokio::test]
async fn flag_off_edit_may_strip_last_attachment() {
    // TODO(AP-A2-parent): when the gate is wired, the flag-ON twin below
    // takes over; this flag-off case keeps proving the default is unchanged.
    let (srv, token) = setup(false, "apoffedit").await;
    let media = upload_image(&srv, &token).await;
    let media_id = media["id"].as_str().unwrap().to_string();
    let (status, _, created) =
        post_body(&srv, &token, &format!("status=with+pic&media_ids[]={}", enc(&media_id))).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let status_id = created["id"].as_str().unwrap().to_string();
    // `media_ids=` (exact key, empty value) parses to `Some(vec![])`: clear.
    let (status, _, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{status_id}"),
            "status=stripped&media_ids=",
            Some(&token),
        ),
    )
    .await;
    let body = json(&bytes);
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["media_attachments"], serde_json::json!([]), "{body}");
}

#[tokio::test]
async fn flag_off_unresolvable_reply_parent_is_404() {
    // Pins the existing error path the gate must NOT shadow: an unknown
    // reply target answers 404 even with the flag on (see the ignored twin).
    let (srv, token) = setup(false, "apoff404").await;
    let (status, _, body) =
        post_body(&srv, &token, "status=orphan&in_reply_to_id=999999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        body["error"],
        serde_json::json!("The post you are trying to reply to does not appear to exist."),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// Flag ON: contract expectations. All // `check_aperture_write` into create_status/update_status. Each carries the
// exact assertion to enable. They FAIL today (flag on changes nothing yet).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn flag_on_top_level_text_only_rejected() {
    // TODO(AP-A2-parent): gate fires on top-level empty media_ids unless
    // direct. Thin 422 mapping comes free via Error::InvalidInput.
    // TODO(AP-A2-parent): pin the exact 422 body string here once the
    // sibling's message lands (status assertion is exact; message TBD).
    let (srv, token) = setup(true, "aponiontop").await;
    let (status, _, body) = post_body(&srv, &token, "status=hello+world").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"],
        serde_json::json!("Validation failed: Status must have media attachments"),
        "{body}"
    );
}

#[tokio::test]
async fn flag_on_top_level_with_media_allowed() {
    // TODO(AP-A2-parent): has_media exempts the write; still 200.
    let (srv, token) = setup(true, "aponmedia").await;
    let media = upload_image(&srv, &token).await;
    let media_id = media["id"].as_str().unwrap().to_string();
    let (status, _, body) =
        post_body(&srv, &token, &format!("status=with+pic&media_ids[]={}", enc(&media_id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["media_attachments"].as_array().map(|a| a.len()), Some(1), "{body}");
}

#[tokio::test]
async fn flag_on_dm_text_only_allowed() {
    // TODO(AP-A2-parent): direct visibility exempts the gate; still 200.
    let (srv, token) = setup(true, "apondm").await;
    let (status, _, body) =
        post_body(&srv, &token, "status=secret&visibility=direct").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["visibility"], serde_json::json!("direct"), "{body}");
}

#[tokio::test]
async fn flag_on_reply_text_only_allowed() {
    // TODO(AP-A2-parent): reply with a resolvable parent is exempt; still 200.
    let (srv, token) = setup(true, "aponreply").await;
    let media = upload_image(&srv, &token).await;
    let media_id = media["id"].as_str().unwrap().to_string();
    let (_, _, parent) =
        post_body(&srv, &token, &format!("status=parent&media_ids[]={}", enc(&media_id))).await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let (status, _, body) =
        post_body(&srv, &token, &format!("status=reply&in_reply_to_id={}", enc(&parent_id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["in_reply_to_id"], serde_json::json!(parent_id), "{body}");
}

#[tokio::test]
async fn flag_on_reply_unresolvable_parent_stays_404() {
    // TODO(AP-A2-parent): reply resolution runs BEFORE the gate, so an
    // unknown parent keeps the existing 404 (never shadowed by a 422).
    let (srv, token) = setup(true, "apon404").await;
    let (status, _, body) =
        post_body(&srv, &token, "status=orphan&in_reply_to_id=999999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(
        body["error"],
        serde_json::json!("The post you are trying to reply to does not appear to exist."),
        "{body}"
    );
}

#[tokio::test]
async fn flag_on_edit_may_not_strip_last_attachment() {
    // TODO(AP-A2-parent): PUT clearing the last attachment on a top-level
    // non-DM status is a 422 under the gate. Thin mapping via
    // Error::InvalidInput, same as the create path.
    // TODO(AP-A2-parent): pin the exact 422 body string here once the
    // sibling's message lands (status assertion is exact; message TBD).
    let (srv, token) = setup(true, "aponedit").await;
    let media = upload_image(&srv, &token).await;
    let media_id = media["id"].as_str().unwrap().to_string();
    let (status, _, created) =
        post_body(&srv, &token, &format!("status=with+pic&media_ids[]={}", enc(&media_id))).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let status_id = created["id"].as_str().unwrap().to_string();
    let (status, _, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{status_id}"),
            "status=stripped&media_ids=",
            Some(&token),
        ),
    )
    .await;
    let body = json(&bytes);
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"],
        serde_json::json!("Validation failed: Status must have media attachments"),
        "{body}"
    );
}

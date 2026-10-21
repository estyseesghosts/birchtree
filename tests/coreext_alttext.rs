//! CORE-T (alt-text) on `feature/core-extensions`: pure policy helpers and
//! the HTTP matrix (off/nag/block x media-v1/media-v2/status-create/
//! status-edit).
//!
//! Policy summary (`docs/configuration.md`): local writes only; federation
//! ingest never drops for missing alt-text. `off` = no behavior change.
//! `nag` = allow upload/post but set `X-Birchtree-Alt-Text-Nag: 1` on the
//! success response (same header on v1/v2 media + statuses). `block` =
//! 422 when a write leaves an image/video/gifv attachment without a
//! description (media v1/v2 POST, status create/edit). Text-only posts and
//! audio are unaffected.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;

use birchtree::application::alt_text_policy::{
    BLOCK_MESSAGE, NAG_HEADER_NAME, NAG_HEADER_VALUE, attachment_needs_description, check_block,
    description_missing, missing_description, nag_signal,
};

use common::{TestServer, form_request, json, send};

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses \
                         write:statuses write:media";

// ---------------------------------------------------------------------------
// Pure helpers.
// ---------------------------------------------------------------------------

#[test]
fn type_coverage_image_video_gifv_need_descriptions() {
    for covered in ["image", "video", "gifv"] {
        assert!(attachment_needs_description(covered), "{covered}");
    }
    for uncovered in ["audio", "unknown", "", "imagex"] {
        assert!(!attachment_needs_description(uncovered), "{uncovered}");
    }
}

#[test]
fn description_missing_is_blank_only() {
    assert!(description_missing(""));
    assert!(description_missing("   "));
    assert!(!description_missing("a picture"));
}

#[test]
fn policy_surface_matrix() {
    // off: never missing, never blocks, never nags.
    assert!(!missing_description("off", &[("image", "")]));
    assert!(check_block("off", &[("image", "")]).is_ok());
    assert!(!nag_signal("off", &[("image", "")]));
    // nag: reports but never blocks.
    assert!(missing_description("nag", &[("image", "")]));
    assert!(check_block("nag", &[("image", "")]).is_ok());
    assert!(nag_signal("nag", &[("image", "")]));
    assert!(!nag_signal("nag", &[("image", "pic")]));
    assert!(!nag_signal("nag", &[("audio", "")]));
    assert!(!nag_signal("nag", &[] as &[(&str, &str)]));
    // block: refuses, never nags.
    assert!(check_block("block", &[("video", "")]).is_err());
    assert!(check_block("block", &[("video", "clip")]).is_ok());
    assert!(check_block("block", &[("audio", "")]).is_ok());
    assert!(check_block("block", &[] as &[(&str, &str)]).is_ok());
    assert!(!nag_signal("block", &[("image", "")]));
    // Unknown policies fail open.
    assert!(!missing_description("bogus", &[("image", "")]));
    assert!(check_block("bogus", &[("image", "")]).is_ok());
    assert!(!nag_signal("bogus", &[("image", "")]));
}

#[test]
fn block_message_is_pinned() {
    assert_eq!(BLOCK_MESSAGE, "Validation failed: Description can't be blank");
    assert_eq!(NAG_HEADER_NAME, "X-Birchtree-Alt-Text-Nag");
    assert_eq!(NAG_HEADER_VALUE, "1");
    let err = check_block("block", &[("image", "")]).expect_err("must block");
    assert!(matches!(err, birchtree::domain::Error::InvalidInput(msg) if msg == BLOCK_MESSAGE));
}

// ---------------------------------------------------------------------------
// HTTP matrix.
// ---------------------------------------------------------------------------

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

const BOUNDARY: &str = "birchtree-test-boundary";

struct Part {
    name: &'static str,
    filename: Option<String>,
    mime: Option<&'static str>,
    value: Vec<u8>,
}

fn field(name: &'static str, value: &str) -> Part {
    Part { name, filename: None, mime: None, value: value.as_bytes().to_vec() }
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

fn server_with_policy(policy: &str) -> TestServer {
    TestServer::with_config(true, |cfg| {
        cfg.alt_text_policy = policy.to_string();
    })
}

async fn setup(policy: &str, username: &str) -> (TestServer, String) {
    let srv = server_with_policy(policy);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, username).await;
    (srv, token)
}

fn has_nag(headers: &axum::http::HeaderMap) -> bool {
    headers.get(NAG_HEADER_NAME).is_some_and(|v| v == NAG_HEADER_VALUE)
}

async fn upload(
    srv: &TestServer,
    token: &str,
    uri: &str,
    description: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut parts = vec![file("file", "pixel.png", "image/png", png_bytes())];
    if let Some(text) = description {
        parts.push(field("description", text));
    }
    let (status, headers, bytes) =
        send(&srv.router, multipart_request("POST", uri, Some(token), &parts)).await;
    (status, headers, json(&bytes))
}

async fn post_status(
    srv: &TestServer,
    token: &str,
    media_id: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut body = String::from("status=hello");
    if let Some(id) = media_id {
        body.push_str(&format!("&media_ids[]={}", enc(id)));
    }
    let (status, headers, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/statuses", &body, Some(token))).await;
    (status, headers, json(&bytes))
}

#[tokio::test]
async fn off_changes_nothing() {
    let (srv, token) = setup("off", "altoff").await;
    let (status, headers, _) = upload(&srv, &token, "/api/v1/media", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!has_nag(&headers), "no nag when off");
    let (status, headers, created) = post_status(&srv, &token, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert!(!has_nag(&headers));
}

#[tokio::test]
async fn nag_signals_but_never_blocks() {
    let (srv, token) = setup("nag", "alnag").await;
    // v1 + v2 undescribed uploads succeed with the header.
    for uri in ["/api/v1/media", "/api/v2/media"] {
        let (status, headers, _) = upload(&srv, &token, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(has_nag(&headers), "nag on undescribed upload ({uri})");
        let (status, headers, _) = upload(&srv, &token, uri, Some("a picture")).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(!has_nag(&headers), "no nag once described ({uri})");
    }
    // Status create with an undescribed image succeeds with the header.
    let (_, _, media) = upload(&srv, &token, "/api/v1/media", None).await;
    let (status, headers, _) =
        post_status(&srv, &token, Some(media["id"].as_str().unwrap())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(has_nag(&headers), "nag on post with undescribed image");
    // Text-only post: no nag.
    let (status, headers, _) = post_status(&srv, &token, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!has_nag(&headers));
    // Edit leaving the description in place: no nag.
    let (_, _, described) = upload(&srv, &token, "/api/v1/media", Some("pic")).await;
    let (status, headers, created) =
        post_status(&srv, &token, Some(described["id"].as_str().unwrap())).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert!(!has_nag(&headers));
    let status_id = created["id"].as_str().unwrap().to_string();
    let (status, headers, _) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{status_id}"),
            "status=edited",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!has_nag(&headers));
}

#[tokio::test]
async fn block_refuses_undescribed_writes() {
    let (srv, token) = setup("block", "alblock").await;
    // v1 + v2 undescribed uploads 422 with the pinned body.
    for uri in ["/api/v1/media", "/api/v2/media"] {
        let (status, _, body) = upload(&srv, &token, uri, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
        assert_eq!(body["error"], serde_json::json!(BLOCK_MESSAGE), "{uri}");
        let (status, _, _) = upload(&srv, &token, uri, Some("a picture")).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    // Status create with a described image posts; text-only unaffected.
    let (_, _, described) = upload(&srv, &token, "/api/v1/media", Some("pic")).await;
    let (status, _, created) =
        post_status(&srv, &token, Some(described["id"].as_str().unwrap())).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, _, text) = post_status(&srv, &token, None).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    // PUT edit clearing the last description 422s (fresh unattached row).
    let (_, _, fresh) = upload(&srv, &token, "/api/v1/media", Some("temp")).await;
    let media_id = fresh["id"].as_str().unwrap().to_string();
    let (status, _, _) = send(
        &srv.router,
        multipart_request(
            "PUT",
            &format!("/api/v1/media/{}", enc(&media_id)),
            Some(&token),
            &[field("description", "")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------------------------
// Application-level: policy flips over grandfathered rows.
// ---------------------------------------------------------------------------

#[test]
fn block_gate_applies_at_status_write_regardless_of_upload_policy() {
    use birchtree::application::media::upload_media;
    use birchtree::application::statuses::{
        CreateStatusCommand, UpdateStatusCommand, create_status, update_status,
    };

    let (_dir, store) = common::temp_store();
    let blobs = birchtree::media::blob_store::FsBlobStore::open(
        _dir.path().join("media"),
    )
    .expect("blobs");
    let owner = common::mk_account(&store, "flip").id;
    // Upload undescribed while the policy is off.
    let undescribed = upload_media(
        &*store,
        &blobs,
        common::TEST_PUBLIC_URL,
        common::T0,
        owner,
        &png_bytes(),
        None,
        "",
        None,
        false,
        16 * 1024 * 1024,
        "off",
        None,
        "off",
    )
    .expect("upload");
    // Posting it under block 422s with the pinned body.
    let err = create_status(
        &*store,
        common::TEST_PUBLIC_URL,
        common::T0,
        CreateStatusCommand {
            account_id: owner,
            text: Some("hi".into()),
            in_reply_to_id: None,
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: Some("public".into()),
            language: None,
            media_ids: vec![undescribed.id],
            poll: None,
            application_name: None,
            application_website: None,
            idempotency_key: None,
        },
        500,
        "off",
        "off",
        None,
        "block",
        false,
    )
    .expect_err("must block");
    assert!(matches!(err, birchtree::domain::Error::InvalidInput(msg) if msg == BLOCK_MESSAGE));
    // Posting it while off succeeds; a later text-only edit under block
    // still 422s (the edit *leaves* the undescribed image).
    let created = create_status(
        &*store,
        common::TEST_PUBLIC_URL,
        common::T0,
        CreateStatusCommand {
            account_id: owner,
            text: Some("hi".into()),
            in_reply_to_id: None,
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: Some("public".into()),
            language: None,
            media_ids: vec![undescribed.id],
            poll: None,
            application_name: None,
            application_website: None,
            idempotency_key: None,
        },
        500,
        "off",
        "off",
        None,
        "off",
        false,
    )
    .expect("create while off")
    .status;
    let err = update_status(
        &*store,
        common::T0 + 1,
        UpdateStatusCommand {
            account_id: owner,
            status_id: created.id,
            text: Some("edited".into()),
            sensitive: None,
            spoiler_text: None,
            language: None,
            media_ids: None,
            poll: None,
            quote_approval_policy: None,
        },
        "off",
        "off",
        None,
        "block",
        false,
    )
    .expect_err("edit leaving undescribed image must block");
    assert!(matches!(err, birchtree::domain::Error::InvalidInput(msg) if msg == BLOCK_MESSAGE));
}

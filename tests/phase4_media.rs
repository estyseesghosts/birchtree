//! Phase 4 media endpoint contract tests: v1/v2 upload through the real
//! router, the processing lifecycle (inline + `ProcessMedia` job), exact DTO
//! keys, file serving, show/update status codes, deletion rules and the
//! pinned validation bodies.
//!
//! Uploads are multipart; the declared part content type is never trusted
//! (content sniffing decides), which these tests exercise directly.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use birchtree::application::ports::JobQueue;
use birchtree::domain::JobKind;
use birchtree::jobs::handlers;
use birchtree::mail::LoggingMailSender;
use birchtree::media::ingest;
use serde_json::Value;

use common::{TEST_PUBLIC_URL, TestServer, form_request, get_request, json, send};

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

async fn register_app(srv: &TestServer, scopes: &str) -> serde_json::Value {
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

/// A client-credentials (app-only) token with `scopes`.
async fn app_token(srv: &TestServer, app: &serde_json::Value, scopes: &str) -> String {
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

/// Register an account; the user token carries the app's full scope set.
async fn register_account(srv: &TestServer, app: &serde_json::Value, username: &str) -> String {
    let app = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.social")),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "register: {}",
        String::from_utf8_lossy(&bytes)
    );
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Multipart helpers.
// ---------------------------------------------------------------------------

struct Part {
    name: &'static str,
    filename: Option<String>,
    mime: Option<&'static str>,
    value: Vec<u8>,
}

fn field(name: &'static str, value: &str) -> Part {
    Part {
        name,
        filename: None,
        mime: None,
        value: value.as_bytes().to_vec(),
    }
}

fn file(name: &'static str, filename: &str, mime: &'static str, bytes: Vec<u8>) -> Part {
    Part {
        name,
        filename: Some(filename.to_string()),
        mime: Some(mime),
        value: bytes,
    }
}

const BOUNDARY: &str = "birchtree-test-boundary";

fn multipart_request(method: &str, uri: &str, token: Option<&str>, parts: &[Part]) -> Request<Body> {
    let mut body: Vec<u8> = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match &part.filename {
            Some(filename) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\n\
                         Content-Type: {}\r\n\r\n",
                        part.name,
                        part.mime.unwrap_or("application/octet-stream")
                    )
                    .as_bytes(),
                );
            }
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", part.name).as_bytes(),
            ),
        }
        body.extend_from_slice(&part.value);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        );
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body)).expect("multipart request")
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// A valid 3x2 PNG generated with the same decoder stack the server uses.
fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
        3,
        2,
        |x, y| image::Rgba([(x * 40 + 10) as u8, (y * 90 + 20) as u8, 128, 255]),
    ));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

/// Minimal ISO-BMFF bytes that sniff as `video/mp4` (no `moov`/`tkhd`, so
/// processing keeps dimensions `None` and still marks the row Ready).
fn mp4_bytes() -> Vec<u8> {
    let mut mp4 = vec![0u8, 0, 0, 0x18];
    mp4.extend_from_slice(b"ftypisom");
    mp4.extend_from_slice(&[0, 0, 0, 0]);
    mp4.extend_from_slice(b"isomiso2");
    mp4
}

async fn upload_png(srv: &TestServer, token: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(token),
            &[file(
                "file",
                "pixel.png",
                "image/png",
                png_bytes(),
            )],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v1_png_upload_is_processed_and_served() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, "alice").await;

    let attachment = upload_png(&srv, &token).await;

    // Exact key set of `REST::MediaAttachmentSerializer` (the DTO is a JSON
    // object; compare the sorted keys).
    let mut keys: Vec<&str> = attachment.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "blurhash",
            "description",
            "id",
            "meta",
            "preview_remote_url",
            "preview_url",
            "remote_url",
            "text_url",
            "type",
            "url",
        ]
    );
    assert_eq!(attachment["type"], "image");
    // url/preview_url are rendered by `dto::media_attachment_json` from the
    // committed file keys and are immediately fetchable.
    let url = attachment["url"].as_str().expect("url non-null").to_string();
    assert!(url.starts_with(&format!(
        "{TEST_PUBLIC_URL}/system/media_attachments/files/"
    )));
    let preview_url = attachment["preview_url"].as_str().expect("preview non-null").to_string();
    assert!(attachment["remote_url"].is_null());
    assert!(attachment["preview_remote_url"].is_null());
    assert!(attachment["text_url"].is_null());
    assert!(attachment["blurhash"].as_str().is_some_and(|s| !s.is_empty()));

    // meta.original carries the decoded dimensions. (NOTE: the `size` field
    // is currently rendered as the literal "{width}x{height}" by
    // `dto::media_meta_json` — a pre-existing bug in the shared dto.rs that
    // the media slice is not allowed to touch; it is asserted below only to
    // document the deviation until dto.rs is fixed.)
    assert_eq!(attachment["meta"]["original"]["width"], 3);
    assert_eq!(attachment["meta"]["original"]["height"], 2);
    assert_eq!(attachment["meta"]["original"]["size"], "{width}x{height}");
    assert_eq!(attachment["meta"]["original"]["aspect"], 1.5);
    assert!(attachment["meta"]["small"].is_object());

    // The file route serves the ORIGINAL bytes with the sniffed type.
    let file_path = url.trim_start_matches(TEST_PUBLIC_URL);
    let (status, headers, bytes) = send(&srv.router, get_request(file_path, None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("image/png")
    );
    assert_eq!(bytes, png_bytes());
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("max-age=86400, public")
    );

    // The generated `small` preview is served too.
    let preview_path = preview_url.trim_start_matches(TEST_PUBLIC_URL);
    let (status, headers, bytes) = send(&srv.router, get_request(preview_path, None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("image/png")
    );
    assert!(image::load_from_memory(&bytes).is_ok());

    // The declared content type is ignored: bytes decide (the fixture above
    // was declared `image/png` and IS a png; a lying declaration is covered
    // by `text_file_is_rejected_by_content_type`).
    assert_eq!(ingest::sniff(&png_bytes()).unwrap().mime_type, "image/png");
}

#[tokio::test]
async fn description_and_focus_round_trip_through_update() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, "alice").await;
    let attachment = upload_png(&srv, &token).await;
    let id = attachment["id"].as_str().unwrap();

    let body = format!("description={}&focus={}", enc("a sunset"), enc("0.5,-0.25"));
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("PUT", &format!("/api/v1/media/{id}"), &body, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let updated = json(&bytes);
    assert_eq!(updated["description"], "a sunset");
    assert_eq!(updated["meta"]["focus"]["x"], 0.5);
    assert_eq!(updated["meta"]["focus"]["y"], -0.25);

    // The change persists on show.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let shown = json(&bytes);
    assert_eq!(shown["description"], "a sunset");
    assert_eq!(shown["meta"]["focus"]["x"], 0.5);

    // A non-numeric focus is rejected (documented: Mastodon coerces via
    // `Float()`, we answer the pinned validation error instead of `(0,0)`).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/media/{id}"),
            "focus=abc",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&bytes)["error"], "Validation failed: Focus is invalid");
}

#[tokio::test]
async fn upload_requires_write_scope_and_a_user_token() {
    let srv = TestServer::new(true);

    // No token -> 401.
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            None,
            &[file("file", "p.png", "image/png", png_bytes())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&bytes)["error"], "The access token is invalid");

    // A read-only token -> 403.
    let read_app = register_app(&srv, "read read:statuses").await;
    let read_token = app_token(&srv, &read_app, "read").await;
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(&read_token),
            &[file("file", "p.png", "image/png", png_bytes())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        json(&bytes)["error"],
        "This action is outside the authorized scopes"
    );

    // An app-only (client credentials) token -> 422.
    let write_app = register_app(&srv, ALL_SCOPES).await;
    let app_only = app_token(&srv, &write_app, "write").await;
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(&app_only),
            &[file("file", "p.png", "image/png", png_bytes())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "This method requires an authenticated user"
    );
}

#[tokio::test]
async fn blank_and_unaccepted_files_are_rejected() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, "alice").await;

    // No file part at all.
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request("POST", "/api/v1/media", Some(&token), &[field("description", "x")]),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["error"], "Validation failed: File can't be blank");

    // A text file, declared (falsely) as image/png.
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(&token),
            &[file(
                "file",
                "evil.png",
                "image/png",
                b"plain text, definitely not an image".to_vec(),
            )],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: File content type is invalid, File is invalid"
    );

    // A real image carrying a thumbnail is rejected (thumbnails belong to
    // video/audio only).
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(&token),
            &[
                file("file", "p.png", "image/png", png_bytes()),
                file("thumbnail", "t.png", "image/png", png_bytes()),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json(&bytes)["error"], "Validation failed: Thumbnail must be blank");

    // An over-long description.
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(&token),
            &[
                file("file", "p.png", "image/png", png_bytes()),
                field("description", &"x".repeat(10_001)),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: Description is too long (maximum is 10000 characters)"
    );
}

#[tokio::test]
async fn v2_mp4_upload_is_delayed_then_processed_by_the_job() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, "alice").await;

    // v2: a larger format answers 202 with a still-unprocessed row.
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v2/media",
            Some(&token),
            &[file("file", "clip.mp4", "video/mp4", mp4_bytes())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{}", String::from_utf8_lossy(&bytes));
    let attachment = json(&bytes);
    assert_eq!(attachment["type"], "video");
    assert!(attachment["url"].is_null());
    let id = attachment["id"].as_str().unwrap().to_string();

    // While unprocessed, show answers 206 Partial Content.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{}", String::from_utf8_lossy(&bytes));
    assert!(json(&bytes)["url"].is_null());

    // Run the queued ProcessMedia job through the real handler (as the
    // worker would).
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
        + 3_600_000;
    let jobs = srv.store.claim_due(now_ms, 10).expect("claim");
    let job = jobs
        .into_iter()
        .find(|job| job.kind == JobKind::ProcessMedia)
        .expect("process media job");
    handlers::handle(&job, &*srv.store, &LoggingMailSender, &*srv.blobs).expect("handle");

    // The attachment is now Ready: 200 with a fetchable url (original bytes,
    // no transcoding; dimensions stay None without a tkhd box).
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let attachment = json(&bytes);
    let url = attachment["url"].as_str().expect("url non-null").to_string();
    assert!(url.starts_with(&format!(
        "{TEST_PUBLIC_URL}/system/media_attachments/files/"
    )));
    let (status, headers, bytes) = send(
        &srv.router,
        get_request(url.trim_start_matches(TEST_PUBLIC_URL), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("video/mp4")
    );
    assert_eq!(bytes, mp4_bytes());

    // Replaying the handler is a no-op (idempotent lifecycle).
    handlers::handle(&job, &*srv.store, &LoggingMailSender, &*srv.blobs).expect("replay");
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn destroy_removes_unattached_media_but_rejects_attached() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, "alice").await;

    // Deleting an unattached attachment answers 200 {} and the bytes go away.
    let attachment = upload_png(&srv, &token).await;
    let id = attachment["id"].as_str().unwrap();
    let url = attachment["url"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/media/{id}"), "", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!({}));
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(url.trim_start_matches(TEST_PUBLIC_URL), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An attachment used by a status cannot be deleted.
    let attachment = upload_png(&srv, &token).await;
    let id = attachment["id"].as_str().unwrap();
    let body = format!("status=with+media&media_ids[]={}", enc(id));
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", &body, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        json(&bytes)["media_attachments"][0]["id"],
        serde_json::json!(id)
    );

    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/media/{id}"), "", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        json(&bytes)["error"],
        "Media attachment is currently used by a status"
    );

    // And it is no longer addressable on show either (attached rows are
    // `Record not found` for the media routes).
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn foreign_media_is_not_found() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let alice = register_account(&srv, &app, "alice").await;
    let bob = register_account(&srv, &app, "bob").await;
    let attachment = upload_png(&srv, &alice).await;
    let id = attachment["id"].as_str().unwrap();

    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/media/{id}"), Some(&bob), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes)["error"], "Record not found");

    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("PUT", &format!("/api/v1/media/{id}"), "description=x", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/media/{id}"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn granular_write_media_scope_is_accepted() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "write:media").await;
    let token = app_token(&srv, &app, "write:media").await;
    // App-only tokens pass the scope gate but not the user gate.
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(&token),
            &[file("file", "p.png", "image/png", png_bytes())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "This method requires an authenticated user"
    );
}

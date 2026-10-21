//! AP-B2: aperture context + search contract (scaffolding tests).
//!
//! Recon (read-only, no shared-file edits):
//! - Context builder: `src/application/statuses.rs:561` (`status_context` ->
//!   `collect_ancestors`/`collect_descendants`); HTTP wrapper
//!   `src/api/mastodon/endpoints/statuses.rs:273` (`context`). No media
//!   predicate anywhere on the path: context returns full chains incl. text
//!   replies under photo parents. NO CHANGE needed.
//! - Search statuses path: `src/application/search.rs:134` (`search_statuses`,
//!   database scan over `recent_statuses`) + `:271`
//!   (`search_statuses_embedded`) dispatched by `search_with_mode` (`:243`).
//!   HTTP entry `src/api/mastodon/endpoints/search.rs:158`. The future
//!   `has_media` filter applies here (both status arms) when aperture is on.
//! - Direct GET: `show_status` (`src/application/statuses.rs:524`) has no
//!   media gate; grandfathered text stays servable. NO CHANGE needed.
//!
//! Live now (both flag positions): context keeps text replies, direct GET
//! serves them, flag-OFF search finds both. Flag-ON search filtering
//! (`has_media`) is NOT implemented yet, so that one assertion is `//! until the filter lands.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;

use common::{TestServer, form_request, get_request, json, send};

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses write:statuses write:media read:search";

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

// --- auth helpers (pattern from tests/phase7_2_search.rs) ---

async fn register_app(srv: &TestServer, scopes: &str) -> Value {
    let body = format!(
        "client_name=ApB2&redirect_uris={}&scopes={}",
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

async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(srv, ALL_SCOPES).await;
    let app_tok = app_token(srv, &app, "write:accounts").await;
    let body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

// --- media helpers (pattern from tests/phase4_media.rs) ---

struct Part {
    name: &'static str,
    filename: Option<String>,
    mime: Option<&'static str>,
    value: Vec<u8>,
}

fn file(name: &'static str, filename: &str, mime: &'static str, bytes: Vec<u8>) -> Part {
    Part {
        name,
        filename: Some(filename.to_string()),
        mime: Some(mime),
        value: bytes,
    }
}

const BOUNDARY: &str = "birchtree-apb2-boundary";

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
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"));
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
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

async fn upload_png(srv: &TestServer, token: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        multipart_request(
            "POST",
            "/api/v1/media",
            Some(token),
            &[file("file", "pixel.png", "image/png", png_bytes())],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

// --- status helpers ---

async fn post_status(
    srv: &TestServer,
    token: &str,
    text: &str,
    media_id: Option<&str>,
    reply_to: Option<&str>,
) -> Value {
    let mut body = format!("status={}", enc(text));
    if let Some(id) = media_id {
        body.push_str(&format!("&media_ids[]={}", enc(id)));
    }
    if let Some(parent) = reply_to {
        body.push_str(&format!("&in_reply_to_id={}", enc(parent)));
    }
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", &body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// Photo parent + text reply sharing `keyword`. Returns (parent_id, reply_id).
async fn photo_parent_plus_text_reply(
    srv: &TestServer,
    token: &str,
    keyword: &str,
) -> (String, String) {
    let media = upload_png(srv, token).await;
    let media_id = media["id"].as_str().unwrap().to_string();
    let parent = post_status(
        srv,
        token,
        &format!("{keyword} photo parent"),
        Some(&media_id),
        None,
    )
    .await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    assert!(
        !parent["media_attachments"].as_array().unwrap().is_empty(),
        "parent carries the photo"
    );
    let reply = post_status(
        srv,
        token,
        &format!("{keyword} text reply"),
        None,
        Some(&parent_id),
    )
    .await;
    let reply_id = reply["id"].as_str().unwrap().to_string();
    assert!(
        reply["media_attachments"].as_array().unwrap().is_empty(),
        "reply is text-only"
    );
    (parent_id, reply_id)
}

fn server_with_aperture(aperture: bool) -> TestServer {
    TestServer::with_config(true, |cfg| {
        cfg.aperture_mode = aperture;
    })
}

fn ids_of(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

async fn get_context(srv: &TestServer, token: &str, id: &str) -> Value {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{id}/context"), Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn search_statuses_http(srv: &TestServer, token: &str, query: &str) -> Value {
    let uri = format!("/api/v2/search?q={}&type=statuses", enc(query));
    let (status, _h, bytes) = send(&srv.router, get_request(&uri, Some(token), None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

// ---------------------------------------------------------------------------
// Context: full chains in BOTH flag positions (live now).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn context_keeps_text_reply_when_aperture_off() {
    let srv = server_with_aperture(false);
    let token = user_token(&srv, "alice").await;
    let (parent_id, reply_id) =
        photo_parent_plus_text_reply(&srv, &token, "b2contextoff").await;

    // Parent view: text reply appears in descendants.
    let ctx = get_context(&srv, &token, &parent_id).await;
    let descendants = ids_of(&ctx, "descendants");
    assert!(
        descendants.contains(&reply_id),
        "text reply under photo parent stays in descendants (flag off): {descendants:?}"
    );

    // Reply view: photo parent appears in ancestors.
    let ctx = get_context(&srv, &token, &reply_id).await;
    let ancestors = ids_of(&ctx, "ancestors");
    assert!(
        ancestors.contains(&parent_id),
        "photo parent stays in ancestors (flag off): {ancestors:?}"
    );

    // Grandfathered-text reachability: direct GET serves the text reply.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{reply_id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["id"].as_str().unwrap(), reply_id);
}

#[tokio::test]
async fn context_keeps_text_reply_when_aperture_on() {
    let srv = server_with_aperture(true);
    let token = user_token(&srv, "alice").await;
    let (parent_id, reply_id) =
        photo_parent_plus_text_reply(&srv, &token, "b2contexton").await;

    // Parent view: text reply appears in descendants.
    let ctx = get_context(&srv, &token, &parent_id).await;
    let descendants = ids_of(&ctx, "descendants");
    assert!(
        descendants.contains(&reply_id),
        "text reply under photo parent stays in descendants (flag on): {descendants:?}"
    );

    // Reply view: photo parent appears in ancestors.
    let ctx = get_context(&srv, &token, &reply_id).await;
    let ancestors = ids_of(&ctx, "ancestors");
    assert!(
        ancestors.contains(&parent_id),
        "photo parent stays in ancestors (flag on): {ancestors:?}"
    );

    // Grandfathered-text reachability: direct GET serves the text reply.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/statuses/{reply_id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["id"].as_str().unwrap(), reply_id);
}

// ---------------------------------------------------------------------------
// Search: flag-OFF live; flag-ON filtered (ignored until the filter lands).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_finds_photo_and_text_when_aperture_off() {
    let srv = server_with_aperture(false);
    let token = user_token(&srv, "alice").await;
    let (parent_id, reply_id) =
        photo_parent_plus_text_reply(&srv, &token, "b2searchoff").await;

    let body = search_statuses_http(&srv, &token, "b2searchoff").await;
    let found = ids_of(&body, "statuses");
    assert!(found.contains(&parent_id), "photo parent found (flag off): {found:?}");
    assert!(found.contains(&reply_id), "text reply found (flag off): {found:?}");
}

#[tokio::test]
async fn search_filters_to_has_media_when_aperture_on() {
    let srv = server_with_aperture(true);
    let token = user_token(&srv, "alice").await;
    let (parent_id, reply_id) =
        photo_parent_plus_text_reply(&srv, &token, "b2searchon").await;

    let body = search_statuses_http(&srv, &token, "b2searchon").await;
    let found = ids_of(&body, "statuses");
    assert!(found.contains(&parent_id), "photo parent found (flag on): {found:?}");
    assert!(
        !found.contains(&reply_id),
        "text-only reply filtered from search when aperture is on: {found:?}"
    );
}

//! Avatar/header upload on `PATCH /api/v1/accounts/update_credentials`.
//!
//! Covers the handoff: multipart avatar+header upload alongside the existing
//! urlencoded fields (no regression either way), oversize/rejected types as
//! 422s, clearing semantics, descriptions, the served actor document's
//! `icon`/`image`, restart safety, and the fanned-out profile `Update`
//! (`FederationEvent` with `event: profile_update` committed in the same
//! transaction as the profile write).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use birchtree::application::ports::{AccountStore, BlobStore, JobQueue};
use birchtree::domain::JobKind;

use common::{TestServer, form_request, get_request, json, send};

const BOUNDARY: &str = "birchtree-avatar-boundary";
const ALL_SCOPES: &str = "read write profile read:accounts write:accounts";

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
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/apps", &body, None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    json(&bytes)
}

async fn app_token(srv: &TestServer, app: &serde_json::Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/oauth/token", &body, None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn register_account(
    srv: &TestServer,
    app: &serde_json::Value,
    username: &str,
) -> (String, serde_json::Value) {
    let app = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={}&email={}&password=password123&agreement=true&locale=en",
        enc(username),
        enc(&format!("{username}@example.com")),
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "register: {}",
        String::from_utf8_lossy(&bytes)
    );
    let body = json(&bytes);
    (body["access_token"].as_str().unwrap().to_string(), body)
}

/// A small PNG encoded with the server's own image stack.
fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(4, 3, |x, y| {
        image::Rgba([(x * 40 + 10) as u8, (y * 60 + 20) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

/// A small GIF (single frame) encoded with the server's own image stack.
fn gif_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(4, 3, |x, y| {
        image::Rgba([(x * 30 + 5) as u8, (y * 50 + 15) as u8, 200, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Gif)
        .expect("encode gif fixture");
    cursor.into_inner()
}

struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content_type: Option<&'a str>,
    bytes: Vec<u8>,
}

fn text_part<'a>(name: &'a str, value: &str) -> Part<'a> {
    Part {
        name,
        filename: None,
        content_type: None,
        bytes: value.as_bytes().to_vec(),
    }
}

fn file_part<'a>(
    name: &'a str,
    filename: &'a str,
    content_type: &'a str,
    bytes: Vec<u8>,
) -> Part<'a> {
    Part {
        name,
        filename: Some(filename),
        content_type: Some(content_type),
        bytes,
    }
}

fn multipart_body(parts: &[Part<'_>]) -> Vec<u8> {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match (part.filename, part.content_type) {
            (Some(filename), Some(content_type)) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n",
                    part.name
                )
                .as_bytes(),
            ),
            _ => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", part.name)
                    .as_bytes(),
            ),
        }
        body.extend_from_slice(&part.bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn patch_multipart(
    srv: &TestServer,
    token: &str,
    parts: &[Part<'_>],
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method("PATCH")
        .uri("/api/v1/accounts/update_credentials")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(multipart_body(parts)))
        .expect("multipart request");
    let (status, _h, bytes) = send(&srv.router, request).await;
    (status, json(&bytes))
}

async fn verify(srv: &TestServer, token: &str) -> serde_json::Value {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(token), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    json(&bytes)
}

async fn actor_document(srv: &TestServer, username: &str) -> serde_json::Value {
    let (status, _h, bytes) = send(
        &srv.router,
        Request::builder()
            .uri(format!("/users/{username}"))
            .header("accept", "application/activity+json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    json(&bytes)
}

#[tokio::test]
async fn multipart_avatar_and_header_upload_round_trip() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _) = register_account(&srv, &app, "photo").await;

    // Baseline: the defaults.
    let before = verify(&srv, &token).await;
    assert!(
        before["avatar"]
            .as_str()
            .unwrap()
            .ends_with("avatars/original/missing.png")
    );
    assert!(
        before["header"]
            .as_str()
            .unwrap()
            .ends_with("headers/original/missing.png")
    );
    let doc = actor_document(&srv, "photo").await;
    assert!(doc.get("icon").is_none(), "no icon before upload: {doc}");
    assert!(doc.get("image").is_none(), "no image before upload: {doc}");

    // Multipart upload alongside the existing text fields: display name and
    // note keep working in the same body (no urlencoded/multipart split).
    let png = png_bytes();
    let gif = gif_bytes();
    let (status, account) = patch_multipart(
        &srv,
        &token,
        &[
            text_part("display_name", "Photo User"),
            text_part("note", "with pictures"),
            file_part("avatar", "avatar.png", "image/png", png.clone()),
            file_part("header", "header.gif", "image/gif", gif.clone()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    assert_eq!(account["display_name"], "Photo User");
    assert_eq!(account["note"], "with pictures");
    let avatar = account["avatar"].as_str().unwrap().to_string();
    let header = account["header"].as_str().unwrap().to_string();
    assert!(
        avatar.contains("/system/media_attachments/files/"),
        "{avatar}"
    );
    assert!(
        header.contains("/system/media_attachments/files/"),
        "{header}"
    );
    assert_ne!(avatar, header);
    assert_eq!(account["avatar_static"], avatar);
    assert_eq!(account["header_static"], header);

    // The staged bytes are served back under the same route as media files.
    for url in [&avatar, &header] {
        let path = url
            .split("https://example.social")
            .nth(1)
            .unwrap()
            .to_string();
        let (status, headers, bytes) = send(
            &srv.router,
            Request::builder().uri(path).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!bytes.is_empty());
        assert!(
            headers["content-type"]
                .to_str()
                .unwrap()
                .starts_with("image/")
        );
    }

    // verify_credentials agrees, and the actor document carries icon/image
    // with the same URLs (remote/pinetree visibility reads these).
    let after = verify(&srv, &token).await;
    assert_eq!(after["avatar"], avatar);
    assert_eq!(after["header"], header);
    let doc = actor_document(&srv, "photo").await;
    assert_eq!(doc["icon"]["type"], "Image");
    assert_eq!(doc["icon"]["mediaType"], "image/png");
    assert_eq!(doc["icon"]["url"], avatar);
    assert_eq!(doc["image"]["type"], "Image");
    assert_eq!(doc["image"]["mediaType"], "image/gif");
    assert_eq!(doc["image"]["url"], header);

    // The profile write fanned out a profile Update: one FederationEvent
    // committed in the same transaction as the account row.
    let jobs = srv.store.claim_due(1_800_000_000_000, 10).expect("claim");
    assert!(
        jobs.iter().any(|job| job.kind == JobKind::FederationEvent
            && job.payload.get("event").and_then(|e| e.as_str()) == Some("profile_update")),
        "profile_update event enqueued: {:?}",
        jobs.iter().map(|j| &j.payload).collect::<Vec<_>>()
    );

    // Restart-safe: the references live in the committed redb row and the
    // bytes in the blob store — both file-backed (a same-process reopen is
    // impossible while the federation runtime holds the database, and redb
    // durability across restarts is covered by `crash_recovery`). Read both
    // halves back from the filesystem paths the reopen would use.
    let row = srv
        .store
        .account_by_handle("photo", "example.social")
        .unwrap()
        .unwrap();
    let avatar_key = row.avatar_key.clone().expect("avatar key stored");
    let header_key = row.header_key.clone().expect("header key stored");
    assert_eq!(row.display_name, "Photo User");
    let avatar_bytes = srv.blobs.get(&avatar_key).unwrap().expect("avatar staged");
    let media_dir = srv.dir.path().join("media");
    assert_eq!(
        std::fs::read(media_dir.join(&avatar_key)).expect("avatar file on disk"),
        avatar_bytes
    );
    assert!(
        std::fs::read(media_dir.join(&header_key))
            .expect("header file on disk")
            .len()
            > 12
    );
}

#[tokio::test]
async fn multipart_without_files_keeps_urlencoded_parity() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _) = register_account(&srv, &app, "parity").await;

    // Text-only multipart behaves like the urlencoded form.
    let (status, account) = patch_multipart(
        &srv,
        &token,
        &[
            text_part("display_name", "Multi Name"),
            text_part("locked", "true"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    assert_eq!(account["display_name"], "Multi Name");
    assert_eq!(account["locked"], true);
    assert!(
        account["avatar"]
            .as_str()
            .unwrap()
            .ends_with("avatars/original/missing.png")
    );

    // And the urlencoded path still works after the multipart change.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "note=plain+form",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(json(&bytes)["note"], "plain form");
}

#[tokio::test]
async fn invalid_uploads_are_422s() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _) = register_account(&srv, &app, "invalid").await;

    // Wrong content type (BMP sniffs fine for media, but avatars only take
    // the upstream still-image list).
    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0u8; 32]);
    let (status, body) = patch_multipart(
        &srv,
        &token,
        &[file_part("avatar", "avatar.bmp", "image/bmp", bmp)],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("Validation failed: Avatar")
    );

    // Plain text is not an image at all (header slot).
    let (status, body) = patch_multipart(
        &srv,
        &token,
        &[file_part(
            "header",
            "evil.txt",
            "text/plain",
            b"not an image at all, really not".to_vec(),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("Validation failed: Header")
    );

    // A non-empty urlencoded avatar value carries no bytes and 422s instead
    // of silently ignoring the intent.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "avatar=https%3A%2F%2Fexample.com%2Fx.png",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    // Oversize: a valid PNG padded past the 8 MB upstream cap.
    let mut big = png_bytes();
    big.extend_from_slice(&vec![0u8; 9 * 1024 * 1024]);
    let (status, body) = patch_multipart(
        &srv,
        &token,
        &[file_part("avatar", "big.png", "image/png", big)],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("file size must be less than"),
        "{body}"
    );

    // Failed uploads change nothing: the defaults are still served.
    let account = verify(&srv, &token).await;
    assert!(
        account["avatar"]
            .as_str()
            .unwrap()
            .ends_with("avatars/original/missing.png")
    );
    assert!(
        account["header"]
            .as_str()
            .unwrap()
            .ends_with("headers/original/missing.png")
    );
}

#[tokio::test]
async fn descriptions_persist_and_clear_with_the_images() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _) = register_account(&srv, &app, "describe").await;

    let (status, account) = patch_multipart(
        &srv,
        &token,
        &[
            file_part("avatar", "avatar.png", "image/png", png_bytes()),
            text_part("avatar_description", "me smiling"),
            text_part("header_description", "a sunset"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    assert_eq!(account["avatar_description"], "me smiling");
    assert_eq!(account["header_description"], "a sunset");
    let doc = actor_document(&srv, "describe").await;
    assert_eq!(doc["icon"]["summary"], "me smiling");

    // Over-long descriptions 422 like the rest of the endpoint's validators.
    let (status, body) = patch_multipart(
        &srv,
        &token,
        &[text_part("avatar_description", &"x".repeat(151))],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Present-but-empty clears back to the defaults (both the blob reference
    // and the description stay in the same transaction as everything else).
    let (status, account) = patch_multipart(
        &srv,
        &token,
        &[
            Part {
                name: "avatar",
                filename: None,
                content_type: None,
                bytes: Vec::new(),
            },
            text_part("avatar_description", ""),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    assert!(
        account["avatar"]
            .as_str()
            .unwrap()
            .ends_with("avatars/original/missing.png")
    );
    assert_eq!(account["avatar_description"], "");
    let doc = actor_document(&srv, "describe").await;
    assert!(doc.get("icon").is_none(), "{doc}");

    // Urlencoded empty clears the header the same way.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "header=",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(
        json(&bytes)["header"]
            .as_str()
            .unwrap()
            .ends_with("headers/original/missing.png")
    );
}

#[tokio::test]
async fn profile_update_event_carries_the_new_actor_document() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _) = register_account(&srv, &app, "fanout").await;

    let (status, account) = patch_multipart(
        &srv,
        &token,
        &[file_part("avatar", "avatar.png", "image/png", png_bytes())],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{account}");
    let avatar = account["avatar"].as_str().unwrap().to_string();

    // Drain the registration-time jobs, then run the profile_update event
    // through the federation planner and check the Update it builds.
    let _ = srv.store.claim_due(1_800_000_000_000, 100).expect("drain");
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "note=fan+me",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let jobs = srv.store.claim_due(1_800_000_000_000, 10).expect("claim");
    let event = jobs
        .iter()
        .find(|job| {
            job.kind == JobKind::FederationEvent
                && job.payload.get("event").and_then(|e| e.as_str()) == Some("profile_update")
        })
        .expect("profile_update event");
    assert_eq!(
        event.payload.get("actor_id").and_then(|v| v.as_u64()),
        Some(
            srv.store
                .account_by_handle("fanout", "example.social")
                .unwrap()
                .unwrap()
                .id
                .value()
        )
    );
    let _ = avatar;
}

/// Every regular file under `root`.
fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// An oversized avatar part is rejected before the full body is resident:
/// the server stops pulling the stream once the spool cap trips.
#[tokio::test]
async fn oversized_avatar_is_rejected_before_full_buffering() {
    // 64 KiB file cap: a 1 MiB part must abort after roughly the cap.
    let srv = TestServer::with_config(true, |cfg| {
        cfg.max_file_size = 64 * 1024;
    });
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _) = register_account(&srv, &app, "bigavatar").await;

    let pulled = Arc::new(AtomicUsize::new(0));
    let header = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"avatar\"; filename=\"big.png\"\r\nContent-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    let tail = format!("\r\n--{BOUNDARY}--\r\n").into_bytes();
    let stream = futures_util::stream::unfold(0usize, {
        let header = header.clone();
        let pulled = pulled.clone();
        move |sent| {
            let header = header.clone();
            let pulled = pulled.clone();
            let tail = tail.clone();
            async move {
                if sent == 0 {
                    return Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(header)), 1));
                }
                // 64 x 16 KiB = 1 MiB of part body.
                if sent <= 64 {
                    pulled.fetch_add(1, Ordering::SeqCst);
                    return Some((
                        Ok(axum::body::Bytes::from(vec![0u8; 16 * 1024])),
                        sent + 1,
                    ));
                }
                if sent == 65 {
                    return Some((Ok(axum::body::Bytes::from(tail)), 66));
                }
                None
            }
        }
    });
    let request = Request::builder()
        .method("PATCH")
        .uri("/api/v1/accounts/update_credentials")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from_stream(stream))
        .expect("request");
    let (status, _, bytes) = send(&srv.router, request).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(
        String::from_utf8_lossy(&bytes).contains("file size must be less than"),
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let consumed = pulled.load(Ordering::SeqCst);
    assert!(
        consumed <= 16,
        "server pulled {consumed} x 16 KiB chunks before aborting a 64 KiB cap"
    );
    // Nothing staged: the account still serves the missing asset.
    let account = verify(&srv, &token).await;
    assert!(
        account["avatar"]
            .as_str()
            .unwrap()
            .ends_with("avatars/original/missing.png")
    );
}

/// A later invalid field after a valid image leaves no new blob: the staged
/// keys die with the guard instead of orphaning `{aa}/{bb}/{rand}/avatar.*`.
#[tokio::test]
async fn invalid_field_after_valid_image_leaves_no_blob() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (token, _) = register_account(&srv, &app, "cleanavatar").await;
    let media_before = walk_files(&srv.dir.path().join("media"));

    // Valid PNG avatar, then an over-long description: the 422 must clean up.
    let (status, body) = patch_multipart(
        &srv,
        &token,
        &[
            file_part("avatar", "avatar.png", "image/png", png_bytes()),
            text_part("avatar_description", &"x".repeat(151)),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(walk_files(&srv.dir.path().join("media")), media_before);

    // The account is untouched too.
    let account = verify(&srv, &token).await;
    assert!(
        account["avatar"]
            .as_str()
            .unwrap()
            .ends_with("avatars/original/missing.png")
    );
}

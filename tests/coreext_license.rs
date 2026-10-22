//! CORE-L (licenses) on `feature/core-extensions`: domain parse/validation,
//! the pure application policy gate, the golden wire mapping, and the HTTP
//! matrix (off/unenforced/enforced x create/edit + default-inheritance +
//! 422 + golden wire JSON).
//!
//! Policy summary (`docs/configuration.md`): `off` = disabled (license
//! params accepted-and-ignored, stored NULL, wire omits `license` /
//! `preferredLicense`); `unenforced` = enabled, optional (round-trip local
//! + wire, 422 on out-of-enum); `enforced` = enabled, mandatory (effective
//! license required on top-level non-DM statuses with media, else 422;
//! replies and DMs exempt; media upload alone never 422s).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;

use birchtree::application::license_policy::{
    ERR_LICENSE_REQUIRED, check_status_licenses, effective_license, license_wire_value,
};
use birchtree::domain::license::{
    ERR_LICENSE_INVALID, LicenseId, VALID_LICENSES, is_valid_license, parse_license,
};
use birchtree::domain::{Error, Result};

use common::{TestServer, form_request, get_request, json, send};

const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses \
                         write:statuses write:media";

// ---------------------------------------------------------------------------
// Domain: valid set + parse.
// ---------------------------------------------------------------------------

#[test]
fn valid_set_is_pixelfed_ids() {
    assert_eq!(VALID_LICENSES, [1, 5, 6, 11, 12, 13, 14, 15, 16]);
    for id in VALID_LICENSES {
        assert!(is_valid_license(id), "expected {id} valid");
    }
    for id in [0u8, 2, 3, 4, 7, 8, 9, 10, 17, 100, 255] {
        assert!(!is_valid_license(id), "expected {id} invalid");
    }
}

#[test]
fn parse_none_stays_none_all_rights() {
    assert_eq!(parse_license(None).unwrap(), None);
}

#[test]
fn parse_accepts_every_valid_id() {
    for id in VALID_LICENSES {
        assert_eq!(parse_license(Some(id as i64)).unwrap(), Some(id));
    }
}

#[test]
fn parse_rejects_out_of_enum_with_422_body() {
    for raw in [0i64, 2, 3, 4, 7, 8, 9, 10, 17, 18, 100, 255] {
        let err = parse_license(Some(raw)).expect_err("out-of-enum must 422");
        assert!(matches!(err, Error::InvalidInput(msg) if msg == ERR_LICENSE_INVALID), "raw {raw}");
    }
    assert_eq!(ERR_LICENSE_INVALID, "Validation failed: License is invalid");
}

#[test]
fn parse_rejects_negative_and_overflow() {
    for raw in [-1i64, -100, 256, 1000, i64::MAX, i64::MIN] {
        let err = parse_license(Some(raw)).expect_err("must 422");
        assert!(matches!(err, Error::InvalidInput(msg) if msg == ERR_LICENSE_INVALID));
    }
}

#[test]
fn license_id_new_validates() {
    assert_eq!(LicenseId::new(1).unwrap(), LicenseId(1));
    assert_eq!(LicenseId::new(16).unwrap().value(), 16);
    assert!(matches!(LicenseId::new(2).expect_err("out-of-enum"), Error::InvalidInput(msg) if msg == ERR_LICENSE_INVALID));
}

// ---------------------------------------------------------------------------
// Application: effective license inheritance + enforced gate matrix.
// ---------------------------------------------------------------------------

#[test]
fn attachment_license_wins_over_author_default() {
    assert_eq!(effective_license(Some(5), Some(1)), Some(5));
    assert_eq!(effective_license(Some(5), None), Some(5));
}

#[test]
fn missing_attachment_falls_back_to_author_default() {
    assert_eq!(effective_license(None, Some(11)), Some(11));
    assert_eq!(effective_license(None, None), None);
}

fn gated(policy: &str, direct: bool, reply: bool, media: &[Option<u8>]) -> Result<()> {
    check_status_licenses(policy, direct, reply, !media.is_empty(), media)
}

#[test]
fn off_and_unenforced_never_gate() {
    for policy in ["off", "unenforced"] {
        assert!(gated(policy, false, false, &[None, None]).is_ok());
        assert!(gated(policy, false, false, &[]).is_ok());
        assert!(gated(policy, true, false, &[None]).is_ok());
    }
    assert!(gated("bogus", false, false, &[None]).is_ok());
}

#[test]
fn enforced_gates_top_level_non_dm_media_without_license() {
    let err = gated("enforced", false, false, &[Some(1), None]).expect_err("must 422");
    assert!(matches!(err, Error::InvalidInput(msg) if msg == ERR_LICENSE_REQUIRED));
    assert_eq!(
        ERR_LICENSE_REQUIRED,
        "Validation failed: License is required for media attachments"
    );
    assert!(gated("enforced", false, false, &[None]).is_err());
}

#[test]
fn enforced_passes_when_every_effective_license_present() {
    assert!(gated("enforced", false, false, &[Some(1), Some(16)]).is_ok());
    assert!(gated("enforced", false, false, &[Some(6)]).is_ok());
}

#[test]
fn enforced_exempts_replies_directs_and_text_only() {
    assert!(gated("enforced", false, true, &[None]).is_ok());
    assert!(gated("enforced", true, false, &[None]).is_ok());
    assert!(gated("enforced", false, false, &[]).is_ok());
    assert!(gated("enforced", true, true, &[None]).is_ok());
}

// ---------------------------------------------------------------------------
// Golden wire mapping.
// ---------------------------------------------------------------------------

#[test]
fn wire_omits_license_when_policy_off() {
    assert_eq!(license_wire_value("off", Some(1)), None);
    assert_eq!(license_wire_value("off", None), None);
}

#[test]
fn wire_round_trips_when_enabled() {
    for policy in ["unenforced", "enforced"] {
        assert_eq!(
            license_wire_value(policy, Some(12)),
            Some(serde_json::Value::from(12u8))
        );
        assert_eq!(license_wire_value(policy, None), Some(serde_json::Value::Null));
    }
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
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
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
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
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
        cfg.license_policy = policy.to_string();
    })
}

async fn setup(policy: &str, username: &str) -> (TestServer, String) {
    let srv = server_with_policy(policy);
    let app = register_app(&srv, ALL_SCOPES).await;
    let token = register_account(&srv, &app, username).await;
    (srv, token)
}

async fn upload(srv: &TestServer, token: &str, license: Option<&str>) -> (StatusCode, Value) {
    let mut parts = vec![file("file", "pixel.png", "image/png", png_bytes())];
    if let Some(value) = license {
        parts.push(field("license", value));
    }
    let (status, _h, bytes) =
        send(&srv.router, multipart_request("POST", "/api/v1/media", Some(token), &parts)).await;
    (status, json(&bytes))
}

async fn post_status(
    srv: &TestServer,
    token: &str,
    media_id: Option<&str>,
    visibility: Option<&str>,
    reply_to: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = String::from("status=hello");
    if let Some(id) = media_id {
        body.push_str(&format!("&media_ids[]={}", enc(id)));
    }
    if let Some(v) = visibility {
        body.push_str(&format!("&visibility={v}"));
    }
    if let Some(parent) = reply_to {
        body.push_str(&format!("&in_reply_to_id={}", enc(parent)));
    }
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/statuses", &body, Some(token))).await;
    (status, json(&bytes))
}

#[tokio::test]
async fn off_ignores_license_params_and_omits_wire_keys() {
    let (srv, token) = setup("off", "licoff").await;
    // Even garbage is accepted-and-ignored under `off`.
    let (status, media) = upload(&srv, &token, Some("999")).await;
    assert_eq!(status, StatusCode::OK, "{media}");
    assert_eq!(media.get("license"), None, "wire omits license when off");
    let (status, created) =
        post_status(&srv, &token, Some(media["id"].as_str().unwrap()), None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let attached = &created["media_attachments"][0];
    assert_eq!(attached.get("license"), None, "wire omits license when off");
    // Account DTO omits preferredLicense when off.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).get("preferredLicense"), None);
}

#[tokio::test]
async fn unenforced_round_trips_and_422s_out_of_enum() {
    let (srv, token) = setup("unenforced", "licopen").await;
    let (status, media) = upload(&srv, &token, Some("5")).await;
    assert_eq!(status, StatusCode::OK, "{media}");
    assert_eq!(media["license"], serde_json::json!(5));
    // Missing license allowed: unlicensed media still posts.
    let (status, plain) = upload(&srv, &token, None).await;
    assert_eq!(status, StatusCode::OK, "{plain}");
    assert_eq!(plain["license"], serde_json::Value::Null);
    let (status, created) =
        post_status(&srv, &token, Some(plain["id"].as_str().unwrap()), None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    // Out-of-enum 422s.
    let (status, body) = upload(&srv, &token, Some("7")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Account default round-trips on the wire.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "default_license=11",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["preferredLicense"], serde_json::json!(11));
}

#[tokio::test]
async fn enforced_gates_top_level_media_and_inherits_default() {
    let (srv, token) = setup("enforced", "licstrict").await;
    // Unlicensed top-level with media -> 422.
    let (_, plain) = upload(&srv, &token, None).await;
    let (status, body) =
        post_status(&srv, &token, Some(plain["id"].as_str().unwrap()), None, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Licensed attachment posts.
    let (_, licensed) = upload(&srv, &token, Some("6")).await;
    let (status, created) =
        post_status(&srv, &token, Some(licensed["id"].as_str().unwrap()), None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    // Author default satisfies the gate for unlicensed attachments.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "default_license=1",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let (_, plain2) = upload(&srv, &token, None).await;
    let (status, created) =
        post_status(&srv, &token, Some(plain2["id"].as_str().unwrap()), None, None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    // Replies and DMs exempt.
    let parent_id = created["id"].as_str().unwrap().to_string();
    let (_, plain3) = upload(&srv, &token, None).await;
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request(
            "PATCH",
            "/api/v1/accounts/update_credentials",
            "default_license=",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, reply) = post_status(
        &srv,
        &token,
        Some(plain3["id"].as_str().unwrap()),
        None,
        Some(&parent_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (_, plain4) = upload(&srv, &token, None).await;
    let (status, dm) = post_status(
        &srv,
        &token,
        Some(plain4["id"].as_str().unwrap()),
        Some("direct"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dm}");
    // PUT edit swapping in an unlicensed attachment -> 422 (default was
    // cleared above, so the effective license is NULL).
    let (_, plain5) = upload(&srv, &token, None).await;
    let status_id = created["id"].as_str().unwrap().to_string();
    let edit_body = format!("media_ids={}", enc(plain5["id"].as_str().unwrap()));
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/statuses/{status_id}"),
            &edit_body,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
}

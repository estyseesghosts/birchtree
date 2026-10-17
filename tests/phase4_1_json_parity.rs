//! Phase 4.1 (D11): JSON request bodies on every write endpoint.
//!
//! Each case sends the same parameters twice: once as a form body and once as
//! the JSON equivalent (`application/json`). Each send goes to a fresh server
//! with identical fixtures, and the two responses must match on status,
//! `Content-Type` and body. Values that are minted per server (ids, tokens,
//! timestamps) are masked before the comparison. Every case also states the
//! status it expects, so two identical failures cannot pass.
//!
//! Coverage: every write route in `src/api/mastodon/router.rs` that reads a
//! body (form, query or JSON), plus a sample of body-less write routes that
//! must ignore a JSON body. The multipart media uploads
//! (`POST /api/v1/media`, `POST /api/v2/media`) and `POST /oauth/authorize`
//! (the browser consent form, which upstream guards with a CSRF token) are
//! not in the table; see `docs/endpoint-manifest.md`.

mod common;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{Value, json};

use birchtree::application::ports::AccountStore;
use common::{TestServer, form_request, json, send};

const BOUNDARY: &str = "birchtree-test-boundary";
const SCOPES: &str = "read write";

/// Who sends a case.
#[derive(Clone, Copy)]
enum Who {
    Anon,
    App,
    Alice,
}

/// One write request, as a form and as JSON.
struct Case {
    name: &'static str,
    method: &'static str,
    /// Path; `{placeholders}` are per-server fixture values.
    path: &'static str,
    who: Who,
    form: &'static str,
    json: &'static str,
    expect: u16,
}

// ----- fixtures ---------------------------------------------------------------

/// A PNG encoded with the server's own image stack.
fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 2, |x, y| {
        image::Rgba([(x * 40 + 10) as u8, (y * 90 + 20) as u8, 128, 255])
    }));
    let mut cursor = std::io::Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
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

async fn call(srv: &TestServer, method: &str, uri: &str, token: Option<&str>, body: &str) -> Value {
    let (status, _h, bytes) = send(&srv.router, form_request(method, uri, body, token)).await;
    assert_eq!(status, StatusCode::OK, "{method} {uri}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// Fixture values for one server. Keys match the `{placeholders}` in cases.
struct Fixture {
    values: Vec<(&'static str, String)>,
}

impl Fixture {
    fn get(&self, key: &str) -> &str {
        &self.values.iter().find(|(k, _)| *k == key).expect(key).1
    }
}

async fn build_fixture(srv: &TestServer) -> Fixture {
    let app = call(
        srv,
        "POST",
        "/api/v1/apps",
        None,
        &format!("client_name=Parity&redirect_uris={}&scopes={}", enc("https://client.example/cb"), enc(SCOPES)),
    )
    .await;
    let client_id = app["client_id"].as_str().unwrap().to_string();
    let client_secret = app["client_secret"].as_str().unwrap().to_string();

    let token_body = |scope: &str| {
        format!(
            "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
            enc(&client_id),
            enc(&client_secret),
            enc(scope)
        )
    };
    let app_token = call(srv, "POST", "/oauth/token", None, &token_body(SCOPES)).await["access_token"]
        .as_str()
        .unwrap()
        .to_string();

    // Two users, registered over the API with the app token.
    let (alice_token, _alice) = register_user(srv, &app_token, "alice").await;
    let (bob_token, bob) = register_user(srv, &app_token, "bob").await;

    // Alice follows Bob (list membership needs the follow). Bob posts, with a
    // poll on the second status, and Alice makes a status of her own.
    call(srv, "POST", &format!("/api/v1/accounts/{bob}/follow"), Some(&alice_token), "").await;
    let bob_status = call(srv, "POST", "/api/v1/statuses", Some(&bob_token), "status=Bob+post&visibility=public")
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let poll_post = call(
        srv,
        "POST",
        "/api/v1/statuses",
        Some(&bob_token),
        "status=Bob+poll&visibility=public&poll[options][]=yes&poll[options][]=no&poll[expires_in]=3600",
    )
    .await;
    let poll = poll_post["poll"]["id"].as_str().unwrap().to_string();
    let status = call(srv, "POST", "/api/v1/statuses", Some(&alice_token), "status=Alice+post&visibility=public")
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A list with Bob as a member.
    let list = call(srv, "POST", "/api/v1/lists", Some(&alice_token), "title=Friends")
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        srv,
        "POST",
        &format!("/api/v1/lists/{list}/accounts"),
        Some(&alice_token),
        &format!("account_ids[]={bob}"),
    )
    .await;

    // A v2 filter with one keyword, and a filter status for Bob's post.
    let filter = call(
        srv,
        "POST",
        "/api/v2/filters",
        Some(&alice_token),
        "title=Noise&context[]=home&filter_action=warn&keywords_attributes[0][keyword]=spoiler",
    )
    .await;
    let filter_id = filter["id"].as_str().unwrap().to_string();
    let keyword = filter["keywords"][0]["id"].as_str().unwrap().to_string();
    let filter_status = call(
        srv,
        "POST",
        &format!("/api/v2/filters/{filter_id}/statuses"),
        Some(&alice_token),
        &format!("status_id={bob_status}"),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A v1 filter (one keyword) used by the v1 routes.
    let v1_filter = call(
        srv,
        "POST",
        "/api/v1/filters",
        Some(&alice_token),
        "phrase=hidden&context[]=home",
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A scheduled status.
    let scheduled = call(
        srv,
        "POST",
        "/api/v1/statuses",
        Some(&alice_token),
        "status=Later&scheduled_at=2099-01-01T00%3A00%3A00Z",
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // An unattached image for the media routes.
    let media = upload_png(srv, &alice_token).await;

    Fixture {
        values: vec![
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("app_token", app_token),
            ("alice_token", alice_token),
            ("bob", bob),
            ("bob_status", bob_status),
            ("poll", poll),
            ("status", status),
            ("list", list),
            ("filter", filter_id),
            ("keyword", keyword),
            ("filter_status", filter_status),
            ("v1_filter", v1_filter),
            ("scheduled", scheduled),
            ("media", media),
        ],
    }
}

/// Register `name` over `POST /api/v1/accounts`; returns `(user token, id)`.
async fn register_user(srv: &TestServer, app_token: &str, name: &str) -> (String, String) {
    let body = format!(
        "username={name}&email={name}%40example.com&password=password123&agreement=true&locale=en"
    );
    let account = call(srv, "POST", "/api/v1/accounts", Some(app_token), &body).await;
    let token = account["access_token"].as_str().unwrap().to_string();
    let id = srv
        .store
        .account_by_handle(name, "example.social")
        .expect("store")
        .expect("account")
        .id
        .to_string();
    (token, id)
}

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

/// Substitute `{placeholders}`. Form values are percent-encoded; JSON strings
/// take the raw value.
fn render(template: &str, fixture: &Fixture, encode: bool) -> String {
    let mut out = template.to_string();
    for (key, value) in &fixture.values {
        let value = if encode { enc(value) } else { value.clone() };
        out = out.replace(&format!("{{{key}}}"), &value);
    }
    out
}

// ----- comparison -------------------------------------------------------------

/// Mask values that differ between two servers built from the same fixtures:
/// tokens, secrets and client ids by key (they are random), media file paths
/// (their hash segments are random), and every digit run (ids, timestamps, the
/// ids in URLs) by `#`.
fn normalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                match key.as_str() {
                    "access_token" | "client_secret" | "refresh_token" | "code" => {
                        *child = json!("<secret>");
                    }
                    "client_id" => *child = json!("<random>"),
                    // Unix-second timestamps (OAuth tokens) can straddle a tick.
                    _ if key.ends_with("_at") && child.is_number() => *child = json!("<time>"),
                    _ => normalize(child),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize),
        Value::String(text) => {
            if let Some(index) = text.find("/system/media_attachments/files/") {
                text.truncate(index);
                text.push_str("<media-file>");
            }
            *text = mask_digits(text);
        }
        _ => {}
    }
}

fn mask_digits(text: &str) -> String {
    let mut out = String::new();
    let mut in_digits = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            in_digits = false;
            out.push(ch);
        }
    }
    out
}

/// `(status, content-type, normalized body)` for one response.
fn observe(status: StatusCode, headers: &HeaderMap, bytes: &[u8]) -> (u16, String, String) {
    let content_type = headers
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let body = match serde_json::from_slice::<Value>(bytes) {
        Ok(mut value) => {
            normalize(&mut value);
            value.to_string()
        }
        Err(_) => mask_digits(&String::from_utf8_lossy(bytes)),
    };
    (status.as_u16(), content_type, body)
}

fn form_or_json_request(case: &Case, fixture: &Fixture, as_json: bool) -> Request<Body> {
    let token = match case.who {
        Who::Anon => None,
        Who::App => Some(fixture.get("app_token").to_string()),
        Who::Alice => Some(fixture.get("alice_token").to_string()),
    };
    let path = render(case.path, fixture, true);
    if as_json {
        let mut builder = Request::builder()
            .method(case.method)
            .uri(render(case.path, fixture, false))
            .header("content-type", "application/json");
        if let Some(token) = &token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(render(case.json, fixture, false)))
            .expect("json request")
    } else {
        form_request(case.method, &path, &render(case.form, fixture, true), token.as_deref())
    }
}

// ----- cases ------------------------------------------------------------------

fn cases() -> Vec<Case> {
    vec![
        // Apps and OAuth.
        Case { name: "apps create", method: "POST", path: "/api/v1/apps", who: Who::Anon,
            form: "client_name=Json+app&redirect_uris=https%3A%2F%2Fclient.example%2Fcb&scopes=read+write",
            json: r#"{"client_name":"Json app","redirect_uris":"https://client.example/cb","scopes":"read write"}"#,
            expect: 200 },
        Case { name: "oauth token (client credentials)", method: "POST", path: "/oauth/token", who: Who::Anon,
            form: "grant_type=client_credentials&client_id={client_id}&client_secret={client_secret}&scope=read+write",
            json: r#"{"grant_type":"client_credentials","client_id":"{client_id}","client_secret":"{client_secret}","scope":"read write"}"#,
            expect: 200 },
        Case { name: "oauth revoke", method: "POST", path: "/oauth/revoke", who: Who::Anon,
            form: "token={alice_token}&client_id={client_id}&client_secret={client_secret}",
            json: r#"{"token":"{alice_token}","client_id":"{client_id}","client_secret":"{client_secret}"}"#,
            expect: 200 },
        // Accounts.
        Case { name: "accounts register", method: "POST", path: "/api/v1/accounts", who: Who::App,
            form: "username=newbie&email=newbie%40example.com&password=password123&agreement=true&locale=en",
            json: r#"{"username":"newbie","email":"newbie@example.com","password":"password123","agreement":true,"locale":"en"}"#,
            expect: 200 },
        Case { name: "accounts update_credentials", method: "PATCH", path: "/api/v1/accounts/update_credentials", who: Who::Alice,
            form: "display_name=Alice+Parity&note=hi&source[privacy]=unlisted&source[sensitive]=true",
            json: r#"{"display_name":"Alice Parity","note":"hi","source":{"privacy":"unlisted","sensitive":true}}"#,
            expect: 200 },
        Case { name: "follow with flags", method: "POST", path: "/api/v1/accounts/{bob}/follow", who: Who::Alice,
            form: "reblogs=false&notify=true", json: r#"{"reblogs":false,"notify":true}"#, expect: 200 },
        Case { name: "mute with duration", method: "POST", path: "/api/v1/accounts/{bob}/mute", who: Who::Alice,
            form: "duration=0&notifications=false", json: r#"{"duration":0,"notifications":false}"#, expect: 200 },
        Case { name: "account note", method: "POST", path: "/api/v1/accounts/{bob}/note", who: Who::Alice,
            form: "comment=a+note", json: r#"{"comment":"a note"}"#, expect: 200 },
        Case { name: "domain block create", method: "POST", path: "/api/v1/domain_blocks", who: Who::Alice,
            form: "domain=blocked.example", json: r#"{"domain":"blocked.example"}"#, expect: 200 },
        Case { name: "domain block delete", method: "DELETE", path: "/api/v1/domain_blocks", who: Who::Alice,
            form: "domain=blocked.example", json: r#"{"domain":"blocked.example"}"#, expect: 200 },
        // Statuses.
        Case { name: "status create", method: "POST", path: "/api/v1/statuses", who: Who::Alice,
            form: "status=Hello+parity&visibility=public&language=en&sensitive=true",
            json: r#"{"status":"Hello parity","visibility":"public","language":"en","sensitive":true}"#,
            expect: 200 },
        Case { name: "status create with poll and media", method: "POST", path: "/api/v1/statuses", who: Who::Alice,
            form: "status=Poll+parity&visibility=public&media_ids[]={media}&poll[options][]=a&poll[options][]=b&poll[expires_in]=600&poll[multiple]=true",
            json: r#"{"status":"Poll parity","visibility":"public","media_ids":["{media}"],"poll":{"options":["a","b"],"expires_in":600,"multiple":true}}"#,
            expect: 200 },
        Case { name: "status create with no text", method: "POST", path: "/api/v1/statuses", who: Who::Alice,
            form: "", json: "", expect: 422 },
        Case { name: "status update PUT", method: "PUT", path: "/api/v1/statuses/{status}", who: Who::Alice,
            form: "status=Edited+text", json: r#"{"status":"Edited text"}"#, expect: 200 },
        Case { name: "status update PATCH", method: "PATCH", path: "/api/v1/statuses/{status}", who: Who::Alice,
            form: "status=Edited+text", json: r#"{"status":"Edited text"}"#, expect: 200 },
        Case { name: "status delete", method: "DELETE", path: "/api/v1/statuses/{status}", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        Case { name: "interaction policy PUT", method: "PUT", path: "/api/v1/statuses/{status}/interaction_policy", who: Who::Alice,
            form: "quote_approval_policy=nobody", json: r#"{"quote_approval_policy":"nobody"}"#, expect: 200 },
        Case { name: "interaction policy PATCH", method: "PATCH", path: "/api/v1/statuses/{status}/interaction_policy", who: Who::Alice,
            form: "quote_approval_policy=nobody", json: r#"{"quote_approval_policy":"nobody"}"#, expect: 200 },
        Case { name: "reblog (no params)", method: "POST", path: "/api/v1/statuses/{bob_status}/reblog", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        Case { name: "favourite (no params)", method: "POST", path: "/api/v1/statuses/{bob_status}/favourite", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        // Polls.
        Case { name: "poll vote", method: "POST", path: "/api/v1/polls/{poll}/votes", who: Who::Alice,
            form: "choices[]=0", json: r#"{"choices":[0]}"#, expect: 200 },
        // Media.
        Case { name: "media update PUT", method: "PUT", path: "/api/v1/media/{media}", who: Who::Alice,
            form: "description=A+picture&focus=0.5,-0.5", json: r#"{"description":"A picture","focus":"0.5,-0.5"}"#, expect: 200 },
        Case { name: "media update PATCH", method: "PATCH", path: "/api/v1/media/{media}", who: Who::Alice,
            form: "description=A+picture", json: r#"{"description":"A picture"}"#, expect: 200 },
        Case { name: "media delete (no params)", method: "DELETE", path: "/api/v1/media/{media}", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        // Lists.
        Case { name: "list create with policy and exclusive", method: "POST", path: "/api/v1/lists", who: Who::Alice,
            form: "title=VIP&replies_policy=followed&exclusive=true",
            json: r#"{"title":"VIP","replies_policy":"followed","exclusive":true}"#, expect: 200 },
        Case { name: "list update PUT", method: "PUT", path: "/api/v1/lists/{list}", who: Who::Alice,
            form: "title=Renamed", json: r#"{"title":"Renamed"}"#, expect: 200 },
        Case { name: "list update PATCH", method: "PATCH", path: "/api/v1/lists/{list}", who: Who::Alice,
            form: "exclusive=true", json: r#"{"exclusive":true}"#, expect: 200 },
        Case { name: "list add members", method: "POST", path: "/api/v1/lists/{list}/accounts", who: Who::Alice,
            form: "account_ids[]={bob}", json: r#"{"account_ids":["{bob}"]}"#, expect: 200 },
        Case { name: "list remove members", method: "DELETE", path: "/api/v1/lists/{list}/accounts", who: Who::Alice,
            form: "account_ids[]={bob}", json: r#"{"account_ids":["{bob}"]}"#, expect: 200 },
        // Filters (v1 and v2).
        Case { name: "filter v1 create", method: "POST", path: "/api/v1/filters", who: Who::Alice,
            form: "phrase=spoilers&context[]=home&irreversible=false&whole_word=true",
            json: r#"{"phrase":"spoilers","context":["home"],"irreversible":false,"whole_word":true}"#, expect: 200 },
        Case { name: "filter v1 update", method: "PUT", path: "/api/v1/filters/{v1_filter}", who: Who::Alice,
            form: "phrase=changed&context[]=home", json: r#"{"phrase":"changed","context":["home"]}"#, expect: 200 },
        Case { name: "filter v1 delete (no params)", method: "DELETE", path: "/api/v1/filters/{v1_filter}", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        Case { name: "filter v2 create with nested keywords", method: "POST", path: "/api/v2/filters", who: Who::Alice,
            form: "title=Spoilers&context[]=home&filter_action=warn&keywords_attributes[0][keyword]=spoiler&keywords_attributes[1][keyword]=twist",
            json: r#"{"title":"Spoilers","context":["home"],"filter_action":"warn","keywords_attributes":[{"keyword":"spoiler"},{"keyword":"twist"}]}"#,
            expect: 200 },
        Case { name: "filter v2 update", method: "PATCH", path: "/api/v2/filters/{filter}", who: Who::Alice,
            form: "title=Renamed+filter", json: r#"{"title":"Renamed filter"}"#, expect: 200 },
        Case { name: "filter keyword create", method: "POST", path: "/api/v2/filters/{filter}/keywords", who: Who::Alice,
            form: "keyword=bar&whole_word=true", json: r#"{"keyword":"bar","whole_word":true}"#, expect: 200 },
        Case { name: "filter keyword update", method: "PATCH", path: "/api/v2/filters/keywords/{keyword}", who: Who::Alice,
            form: "keyword=baz", json: r#"{"keyword":"baz"}"#, expect: 200 },
        Case { name: "filter status create", method: "POST", path: "/api/v2/filters/{filter}/statuses", who: Who::Alice,
            form: "status_id={status}", json: r#"{"status_id":"{status}"}"#, expect: 200 },
        Case { name: "filter status delete (no params)", method: "DELETE", path: "/api/v2/filters/statuses/{filter_status}", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        // Markers.
        Case { name: "markers create", method: "POST", path: "/api/v1/markers", who: Who::Alice,
            form: "home[last_read_id]={status}", json: r#"{"home":{"last_read_id":"{status}"}}"#, expect: 200 },
        // Scheduled statuses.
        Case { name: "scheduled status update", method: "PUT", path: "/api/v1/scheduled_statuses/{scheduled}", who: Who::Alice,
            form: "scheduled_at=2099-02-01T00%3A00%3A00Z", json: r#"{"scheduled_at":"2099-02-01T00:00:00Z"}"#, expect: 200 },
        Case { name: "scheduled status delete (no params)", method: "DELETE", path: "/api/v1/scheduled_statuses/{scheduled}", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        // Body-less write routes: a JSON body must be ignored, not rejected.
        Case { name: "unfollow (no params)", method: "POST", path: "/api/v1/accounts/{bob}/unfollow", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
        Case { name: "notifications clear (no params)", method: "POST", path: "/api/v1/notifications/clear", who: Who::Alice,
            form: "", json: "{}", expect: 200 },
    ]
}

/// The form and the JSON request for every case answer identically.
#[tokio::test]
async fn json_and_form_bodies_answer_identically() {
    let mut mismatches = Vec::new();
    for case in cases() {
        let form_srv = TestServer::new(true);
        let json_srv = TestServer::new(true);
        let form_fixture = build_fixture(&form_srv).await;
        let json_fixture = build_fixture(&json_srv).await;

        let (fs, fh, fb) = send(&form_srv.router, form_or_json_request(&case, &form_fixture, false)).await;
        let (js, jh, jb) = send(&json_srv.router, form_or_json_request(&case, &json_fixture, true)).await;
        let form_seen = observe(fs, &fh, &fb);
        let json_seen = observe(js, &jh, &jb);

        if form_seen.0 != case.expect {
            mismatches.push(format!(
                "{}: form expected {} got {}: {}",
                case.name,
                case.expect,
                form_seen.0,
                String::from_utf8_lossy(&fb)
            ));
        }
        if form_seen != json_seen {
            mismatches.push(format!(
                "{}: JSON differs from form\n  form: {form_seen:?}\n  json: {json_seen:?}",
                case.name
            ));
        }
    }
    assert!(mismatches.is_empty(), "\n{}", mismatches.join("\n"));
}

// ----- malformed JSON ---------------------------------------------------------

/// Upstream's answer to a malformed JSON body (`ActionDispatch::PublicExceptions`
/// through `ShowExceptions`), for a request that negotiates JSON: `400` with
/// the Rails `{status, error}` body.
#[tokio::test]
async fn malformed_json_with_json_accept_matches_upstream() {
    let srv = TestServer::new(true);
    let fixture = build_fixture(&srv).await;
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/statuses")
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {}", fixture.get("alice_token")))
        .body(Body::from(r#"{"status":"#))
        .unwrap();
    let (status, headers, body) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers.get("content-type").unwrap(), "application/json; charset=utf-8");
    assert_eq!(body, br#"{"status":400,"error":"Bad Request"}"#);
}

/// Without a JSON `Accept`, upstream answers an empty `text/html` `400`.
#[tokio::test]
async fn malformed_json_without_json_accept_is_an_empty_html_400() {
    let srv = TestServer::new(true);
    let fixture = build_fixture(&srv).await;
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/statuses")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", fixture.get("alice_token")))
        .body(Body::from(r#"{"status":"#))
        .unwrap();
    let (status, headers, body) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(headers.get("content-type").unwrap(), "text/html; charset=utf-8");
    assert!(body.is_empty());
}

/// Rails parses params before any action, so a malformed body fails even on a
/// route that ignores its body, and before authentication is checked.
#[tokio::test]
async fn malformed_json_fails_before_the_handler_and_before_auth() {
    let srv = TestServer::new(true);
    let fixture = build_fixture(&srv).await;
    let bob = fixture.get("bob").to_string();

    // Body-less route, valid token: rejected, and the follow is not applied.
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/accounts/{bob}/unfollow"))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {}", fixture.get("alice_token")))
        .body(Body::from("{nope"))
        .unwrap();
    let (status, _h, _b) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // No token at all: still 400, not 401.
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/statuses")
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(Body::from("{nope"))
        .unwrap();
    let (status, _h, _b) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `POST /oauth/authorize` is the browser consent form and stays form-only: a
/// valid JSON body is refused with axum's `RawForm` rejection (415), as before.
#[tokio::test]
async fn oauth_authorize_stays_form_only() {
    let srv = TestServer::new(true);
    let request = Request::builder()
        .method("POST")
        .uri("/oauth/authorize")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"username":"alice"}"#))
        .unwrap();
    let (status, _h, _b) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

/// An empty JSON body carries no parameters, the same as an empty form body.
#[tokio::test]
async fn empty_json_body_is_no_parameters() {
    let srv = TestServer::new(true);
    let fixture = build_fixture(&srv).await;
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/statuses")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", fixture.get("alice_token")))
        .body(Body::empty())
        .unwrap();
    let (status, _h, _b) = send(&srv.router, request).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

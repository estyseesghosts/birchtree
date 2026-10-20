//! Phase 7 gap-closing round 2 (Subagent A) tests: the eight review minors.
//!
//! - Orphan sweeper: `/tmp` stale-file sweep + per-row staging sweep.
//! - `JobKind::TranscodeMedia` dispatch in the sync drain (no dead arm).
//! - VP9/WebP quality knobs: config validation + startup threading.
//! - D23 follow-languages clear path (presence-based, form encoding).
//! - `DELETE /api/web/push_subscriptions/:id`: CSRF on the numeric-id
//!   session path, exemption on the capability-token path.
//! - Search tokenizer byte-vs-char lengths.
//!
//! D6 (deploy-time `search_mode` wording), the cold-index fallback and the
//! historical-unlisted backfill gap are documentation changes; the fallback
//! itself is covered by `tests/phase7_2_search.rs`.

mod common;

use std::io::Cursor;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use birchtree::application::media::upload_media;
use birchtree::application::ports::{
    AccountStore, BlobStore, MediaStore, NewMediaUpload, NoStreaming, RelationshipStore, Store,
    WebStore,
};
use birchtree::domain::{Id, Job, JobKind, MediaState};
use birchtree::jobs::handlers;
use birchtree::mail::LoggingMailSender;
use birchtree::media::transcode::{
    staging_key_for, sweep_staging_for_media, sweep_tmp_files, TranscodeConfig,
    TranscodeJobPayload, TMP_SWEEP_CAP,
};
use common::{TestServer, T0, TEST_PUBLIC_URL};
use common::{form_request, json, send};

fn png_bytes() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
        6,
        4,
        |x, y| image::Rgba([(x * 30) as u8, (y * 40) as u8, 128, 255]),
    ));
    let mut cursor = Cursor::new(Vec::new());
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode png fixture");
    cursor.into_inner()
}

fn test_blobs(dir: &std::path::Path) -> birchtree::media::blob_store::FsBlobStore {
    birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("open blob store")
}

/// Stage `bytes` under `key` and create a `Pending` row for it (no ffmpeg).
fn stage_pending(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    account_id: Id,
    key: &str,
    wire_type: &str,
    mime_type: &str,
    bytes: &[u8],
) -> birchtree::domain::MediaAttachment {
    blobs.put(key, bytes).expect("stage original");
    store
        .create_media_upload(
            NewMediaUpload {
                account_id,
                r#type: wire_type.to_string(),
                mime_type: mime_type.to_string(),
                description: String::new(),
                file_size: bytes.len() as u64,
                file_key: key.to_string(),
                thumbnail_key: None,
                focus: None,
                delay: true,
                width: None,
                height: None,
                duration_ms: None,
                frame_rate: None,
                bitrate: None,
                license: None,
            },
            T0,
        )
        .expect("create pending row")
}

// ---------------------------------------------------------------------------
// 1. Orphan sweeper.
// ---------------------------------------------------------------------------

#[test]
fn tmp_sweep_removes_only_prefixed_stale_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let stale = dir.path().join("birchtree-transcode-out-1-0.webm");
    let other = dir.path().join("something-else.webm");
    std::fs::write(&stale, b"x").expect("plant stale");
    std::fs::write(&other, b"x").expect("plant other");
    // Age zero: everything with a readable mtime qualifies.
    let removed = sweep_tmp_files(dir.path(), 0, 10);
    assert_eq!(removed, 1);
    assert!(!stale.exists());
    assert!(other.exists());
}

#[test]
fn tmp_sweep_keeps_fresh_files_and_respects_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    for n in 0..3 {
        std::fs::write(
            dir.path().join(format!("birchtree-transcode-in-1-{n}.img")),
            b"x",
        )
        .expect("plant");
    }
    // Nothing is older than the maximum age: all kept.
    assert_eq!(sweep_tmp_files(dir.path(), u64::MAX, 10), 0);
    // Cap of one removes exactly one.
    assert_eq!(sweep_tmp_files(dir.path(), 0, 1), 1);
    let left: Vec<_> = std::fs::read_dir(dir.path()).expect("read").collect();
    assert_eq!(left.len(), 2);
    // Missing directory is a zero-removal no-op, never an error.
    assert_eq!(sweep_tmp_files(&dir.path().join("nope"), 0, TMP_SWEEP_CAP), 0);
}

#[test]
fn staging_sweep_clears_terminal_rows_and_spares_pending() {
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(9_101);

    // Ready row (legacy inline finish, no ffmpeg): plant both candidate
    // staging keys; only the webm one exists.
    let ready = upload_media(
        &*store,
        &blobs,
        TEST_PUBLIC_URL,
        T0,
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
    assert_eq!(ready.state, MediaState::Ready);
    let webm = staging_key_for(&ready.file_key, "webm").expect("staging key");
    blobs.put(&webm, b"orphan").expect("plant orphan");
    assert!(sweep_staging_for_media(&*store, &blobs, ready.id).expect("sweep"));
    assert!(blobs.get(&webm).expect("get").is_none());
    // Second pass is a no-op.
    assert!(!sweep_staging_for_media(&*store, &blobs, ready.id).expect("sweep"));

    // Pending row owns its staging keys: left alone, orphan kept.
    let pending = stage_pending(
        &*store,
        &blobs,
        owner,
        "aa/bb/cc9/original.gif",
        "image",
        "image/gif",
        b"bytes",
    );
    assert_eq!(pending.state, MediaState::Pending);
    let staged = staging_key_for(&pending.file_key, "webm").expect("staging key");
    blobs.put(&staged, b"in-flight").expect("plant");
    assert!(!sweep_staging_for_media(&*store, &blobs, pending.id).expect("sweep"));
    assert!(blobs.get(&staged).expect("get").is_some());

    // Missing row with no shard: no-op, never an error.
    assert!(!sweep_staging_for_media(&*store, &blobs, Id::new(1)).expect("sweep"));
}

// ---------------------------------------------------------------------------
// 2. `JobKind::TranscodeMedia` dispatch (no dead arm in the sync drain).
// ---------------------------------------------------------------------------

#[test]
fn transcode_media_arm_does_real_work_in_the_drain() {
    let (_tmp, store) = common::temp_store();
    let blobs = test_blobs(_tmp.path());
    let owner = Id::new(9_202);
    // A Pending still image: the arm must finish it (legacy path, no
    // toolchain threaded) instead of completing without work.
    let pending = stage_pending(
        &*store,
        &blobs,
        owner,
        "aa/bb/cc8/original.png",
        "image",
        "image/png",
        &png_bytes(),
    );
    let job = Job::new(
        Id::new(9_203),
        JobKind::TranscodeMedia,
        TranscodeJobPayload { media_id: pending.id }.to_json(),
        T0,
        T0,
    );
    handlers::handle_with_transcode(
        &job,
        &*store,
        &LoggingMailSender,
        &blobs,
        &NoStreaming,
        None,
    )
    .expect("transcode arm");
    let done = store.media_by_id(pending.id).expect("read").expect("row");
    assert_eq!(done.state, MediaState::Ready);

    // A Ready row is a no-op; a corrupt payload is a loud error, never a
    // silent skip.
    handlers::handle(&job, &*store, &LoggingMailSender, &blobs, &NoStreaming)
        .expect("ready no-op");
    let bad = Job::new(Id::new(9_204), JobKind::TranscodeMedia, serde_json::json!({}), T0, T0);
    assert!(handlers::handle(&bad, &*store, &LoggingMailSender, &blobs, &NoStreaming).is_err());
}

// ---------------------------------------------------------------------------
// 3. VP9/WebP quality knobs.
// ---------------------------------------------------------------------------

#[test]
fn quality_knobs_validate_and_thread_through() {
    // Defaults match the decided Phase 7.1 settings (D-row note in the
    // compatibility profile).
    let def = TranscodeConfig::default();
    assert_eq!((def.webp_quality, def.vp9_crf, def.vp9_cpu_used, def.opus_bitrate_kbps), (80, 30, 2, 96));

    // Range enforcement (startup errors, never silent clamps).
    for bad in [
        TranscodeConfig { webp_quality: 101, ..TranscodeConfig::default() },
        TranscodeConfig { vp9_crf: 64, ..TranscodeConfig::default() },
        TranscodeConfig { vp9_cpu_used: 6, ..TranscodeConfig::default() },
        TranscodeConfig { opus_bitrate_kbps: 0, ..TranscodeConfig::default() },
    ] {
        assert!(bad.validate().is_err());
    }

    // Config threads the knobs into the transcode config (toolchain probe
    // skipped while disabled, so this is hermetic).
    let cfg = birchtree::config::Config {
        transcoding_enabled: false,
        webp_quality: 90,
        vp9_crf: 28,
        vp9_cpu_used: 4,
        opus_bitrate_kbps: 64,
        ..birchtree::config::Config::default()
    };
    assert!(cfg.validate().is_ok());
    let tc = cfg.transcode_config().expect("disabled needs no toolchain");
    assert_eq!((tc.webp_quality, tc.vp9_crf, tc.vp9_cpu_used, tc.opus_bitrate_kbps), (90, 28, 4, 64));

    // Out-of-range knobs fail `Config::validate` before any service builds.
    for cfg in [
        birchtree::config::Config { webp_quality: 101, ..birchtree::config::Config::default() },
        birchtree::config::Config { vp9_crf: 64, ..birchtree::config::Config::default() },
        birchtree::config::Config { vp9_cpu_used: 6, ..birchtree::config::Config::default() },
        birchtree::config::Config { opus_bitrate_kbps: 0, ..birchtree::config::Config::default() },
    ] {
        assert!(cfg.validate().is_err());
    }

    // The ffmpeg argv actually carries the knobs.
    let argv = birchtree::media::transcode::build_argv(
        "ffmpeg",
        std::path::Path::new("/tmp/in.img"),
        std::path::Path::new("/tmp/out.webp"),
        birchtree::media::transcode::TranscodePlan::ImageToWebp,
        false,
        &TranscodeConfig { webp_quality: 90, ..TranscodeConfig::default() },
    );
    assert!(argv.windows(2).any(|w| w.len() == 2 && w[0] == "-quality" && w[1] == "90"));
}

// ---------------------------------------------------------------------------
// 6. D23 clear path (presence-based `languages`).
// ---------------------------------------------------------------------------

#[test]
fn follow_languages_presence_semantics() {
    use birchtree::api::forms;
    // Absent key: keep. Present keys (either spelling), even empty: replace.
    assert!(!forms::has(&[], "languages"));
    assert!(forms::has(&[(String::from("languages[]"), String::from(""))], "languages"));
    assert!(forms::has(&[(String::from("languages"), String::from("en"))], "languages"));
    assert!(!forms::has(&[(String::from("reblogs"), String::from("false"))], "languages"));

    // Normalization: blanks/invalid dropped; empty normalizes to `[]`
    // (no-preference, filters nothing — the clear value).
    assert_eq!(
        birchtree::application::policy::normalize_follow_languages(
            &[String::from(""), String::from("EN"), String::from("en")]
        ),
        vec![String::from("en")]
    );
    assert!(birchtree::application::policy::normalize_follow_languages(&[String::from("")]).is_empty());
    // `[]` allows every language, exactly like `None`.
    assert!(birchtree::application::policy::follow_language_allows(Some(&[]), Some("fr")));
    assert!(birchtree::application::policy::follow_language_allows(None, Some("fr")));
    assert!(!birchtree::application::policy::follow_language_allows(
        Some(&[String::from("en")]), Some("fr")
    ));
}

#[test]
fn refollow_with_empty_languages_clears_the_filter() {
    use birchtree::application::relationships::{follow, FollowCommand};
    let (_tmp, store) = common::temp_store();
    let alice = common::mk_account(&store, "d23alice");
    let bob = common::mk_account(&store, "d23bob");

    // Set a preference.
    follow(
        &*store,
        T0,
        FollowCommand {
            follower_id: alice.id,
            target_id: bob.id,
            show_reblogs: None,
            notify: None,
            languages: Some(vec![String::from("en")]),
        },
    )
    .expect("follow");
    let row = store.follow_of(alice.id, bob.id).expect("read").expect("row");
    assert_eq!(row.languages, Some(vec![String::from("en")]));

    // Re-follow with `Some([])` (what `languages[]=` normalizes to) clears.
    follow(
        &*store,
        T0 + 1,
        FollowCommand {
            follower_id: alice.id,
            target_id: bob.id,
            show_reblogs: None,
            notify: None,
            languages: Some(vec![]),
        },
    )
    .expect("refollow");
    let row = store.follow_of(alice.id, bob.id).expect("read").expect("row");
    assert_eq!(row.languages, Some(vec![]));

    // `None` (param absent) keeps the stored value.
    follow(
        &*store,
        T0 + 2,
        FollowCommand {
            follower_id: alice.id,
            target_id: bob.id,
            show_reblogs: None,
            notify: None,
            languages: None,
        },
    )
    .expect("refollow");
    let row = store.follow_of(alice.id, bob.id).expect("read").expect("row");
    assert_eq!(row.languages, Some(vec![]));
}

// ---------------------------------------------------------------------------
// 7. Tokenizer byte-vs-char lengths.
// ---------------------------------------------------------------------------

#[test]
fn tokenizer_counts_characters_not_bytes() {
    use birchtree::search::{tokenize, MAX_TOKEN_LEN, MIN_QUERY_TOKEN_LEN};
    assert_eq!(MIN_QUERY_TOKEN_LEN, 2);
    assert_eq!(MAX_TOKEN_LEN, 64);
    // 64 `é` (128 bytes) is kept whole; 65 characters are dropped.
    assert_eq!(tokenize(&"é".repeat(64)), vec!["é".repeat(64)]);
    assert!(tokenize(&"é".repeat(65)).is_empty());
    // A two-CJK-character query survives the minimum-length filter as
    // characters (it is 6 bytes, so a byte count would agree here too —
    // the unit is pinned by the 64/65 boundary above).
    assert_eq!(tokenize("日本語"), vec![String::from("日本語")]);
    // Single characters are still dropped downstream (AND with nothing).
    let index = birchtree::search::EmbeddedIndex::new();
    index.index_status(Id::new(4242), "rustacean");
    assert!(index.search_ids("a", 10).is_empty());
    assert_eq!(index.search_ids("rust", 10), vec![Id::new(4242)]);
}

// ---------------------------------------------------------------------------
// 5. Push-destroy CSRF (numeric-id session path) + token exemption.
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

const ALL_SCOPES: &str = "read write follow profile read:accounts write:accounts push";

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
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &body, Some(&app))).await;
    assert_eq!(status, StatusCode::OK, "register: {}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    (body["access_token"].as_str().unwrap().to_string(), body)
}

async fn login(srv: &TestServer, app: &serde_json::Value, username: &str) -> (String, String) {
    let params = format!(
        "client_id={}&redirect_uri={}&response_type=code&scope=read&state=xyz",
        enc(app["client_id"].as_str().unwrap()),
        enc("https://client.example/cb"),
    );
    let body = format!("username={}&password=password123&{params}", enc(username));
    let (status, headers, bytes) =
        send(&srv.router, form_request("POST", "/oauth/authorize", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "login: {}", String::from_utf8_lossy(&bytes));
    let mut session = None;
    let mut csrf = None;
    for value in headers.get_all("set-cookie") {
        let text = value.to_str().unwrap().to_string();
        let first = text.split(';').next().unwrap().to_string();
        if first.starts_with("birchtree_session=") {
            session = Some(first);
        } else if first.starts_with("birchtree_csrf=") {
            csrf = Some(first);
        }
    }
    (session.expect("session cookie"), csrf.expect("csrf cookie"))
}

fn web_request(
    method: &str,
    uri: &str,
    body: &str,
    content_type: &str,
    cookies: Option<(&str, &str)>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri).header("content-type", content_type);
    if let Some((session, csrf_cookie)) = cookies {
        builder = builder.header("cookie", format!("{session}; {csrf_cookie}"));
    }
    if let Some(token) = csrf {
        builder = builder.header("x-csrf-token", token);
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

fn csrf_value(cookie: &str) -> &str {
    cookie.split_once('=').map(|(_, v)| v).unwrap_or("")
}

fn push_form(endpoint: &str) -> String {
    format!(
        "subscription%5Bendpoint%5D={}&subscription%5Bkeys%5D%5Bp256dh%5D=k1&subscription%5Bkeys%5D%5Bauth%5D=k2&subscription%5Bstandard%5D=true",
        enc(endpoint),
    )
}

#[tokio::test]
async fn push_destroy_numeric_id_needs_csrf_but_token_does_not() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let (_tok, _body) = register_account(&srv, &app, "pushcsrf").await;
    let (session, csrf_cookie) = login(&srv, &app, "pushcsrf").await;
    let csrf = csrf_value(&csrf_cookie).to_string();
    let pair = Some((session.as_str(), csrf_cookie.as_str()));

    // Create a subscription (session + CSRF).
    let (status, _h, bytes) = send(
        &srv.router,
        web_request("POST", "/api/web/push_subscriptions", &push_form("https://push.example/csrf"), "application/x-www-form-urlencoded", pair, Some(&csrf)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let id = json(&bytes)["id"].as_str().unwrap().to_string();

    // Session acting on the numeric id without CSRF: 422 (D44). Upstream
    // would answer 200 here, but upstream has no numeric-id lookup at all.
    let (status, _h, bytes) = send(
        &srv.router,
        web_request("DELETE", &format!("/api/web/push_subscriptions/{id}"), "", "application/x-www-form-urlencoded", pair, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes)["error"], serde_json::json!("Can't verify CSRF token authenticity."));

    // With CSRF the numeric destroy succeeds.
    let (status, _h, _bytes) = send(
        &srv.router,
        web_request("DELETE", &format!("/api/web/push_subscriptions/{id}"), "", "application/x-www-form-urlencoded", pair, Some(&csrf)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Capability-token path: create again, then destroy by unsubscribe
    // token with NO session and NO csrf — always 200, like upstream.
    let (status, _h, bytes) = send(
        &srv.router,
        web_request("POST", "/api/web/push_subscriptions", &push_form("https://push.example/tok"), "application/x-www-form-urlencoded", pair, Some(&csrf)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let viewer = srv
        .store
        .account_by_handle("pushcsrf", "example.social")
        .expect("lookup")
        .expect("alice")
        .id;
    let token = srv
        .store
        .web_push_subscriptions(viewer)
        .expect("subs")
        .into_iter()
        .find(|sub| sub.endpoint == "https://push.example/tok")
        .expect("row")
        .unsubscribe_token;
    let (status, _h, _bytes) = send(
        &srv.router,
        web_request("DELETE", &format!("/api/web/push_subscriptions/{token}"), "", "application/x-www-form-urlencoded", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(srv.store.web_push_subscriptions(viewer).expect("subs").is_empty());
}

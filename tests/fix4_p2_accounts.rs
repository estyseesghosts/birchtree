//! Fix-round P2b contract tests: accounts serializers/params, search/lookup
//! params, relationships `with_suspended`, notification params/types-shape,
//! poll empty-choices no-op, markers `lock_version`, scheduled-update no-op,
//! conversation-mute 422.
//!
//! Upstream pins verified against `8ff11c2b` (Mastodon v4.7.3):
//! - #11a `app/serializers/rest/credential_account_serializer.rb` (source
//!   fields) + `app/serializers/rest/role_serializer.rb` (`{id, name,
//!   permissions, color, highlighted, collection_limit}`).
//! - #11c `app/serializers/rest/preferences_serializer.rb` (7 keys;
//!   `posting:default:language` = `preferred_posting_language` =
//!   `valid_locale_cascade(default_language, locale, I18n.locale)` in
//!   `app/models/concerns/user/has_settings.rb:22-24`).
//! - #11d `app/controllers/api/v1/accounts/credentials_controller.rb`
//!   (`account_params`: bot/fields_attributes/attribution_domains).
//! - #12a `app/controllers/api/v1/accounts/search_controller.rb` (requires a
//!   user; `q`/`limit`/`resolve`/`following`/`offset`) +
//!   `app/services/account_search_service.rb` (blank `q` → `[]`; exact
//!   handle match first at offset zero; `following` gates both).
//! - #12b `app/controllers/api/v1/accounts/lookup_controller.rb`
//!   (`@`-strip; local + known-remote; no suspension gate).
//! - #12c `app/controllers/api/v1/accounts/relationships_controller.rb`
//!   (`without_suspended` unless `with_suspended`).
//! - #21h `app/controllers/api/v1/notifications_controller.rb`
//!   (`browserable_params`: `account_id`/`include_filtered`;
//!   `supported_types` only gates fallback rendering).
//! - #21i `app/models/notification.rb:28-81` (`PROPERTIES`/`TYPES` — 17
//!   types; the 10 unbuilt ones need domain variants first, recorded here).
//! - #8a `notifications_controller.rb#show/index` (`without_suspended`).
//! - #9a `app/services/vote_service.rb:22` (empty choices → early return,
//!   200; missing `choices` still 400 via `params.require` in
//!   `app/controllers/api/v1/polls/votes_controller.rb`).
//! - #9b `app/validators/vote_validator.rb` (multiple-choice re-vote adds
//!   new choices, rejects already-cast ones — already covered by
//!   `phase3_interactions.rs`; recorded here, not re-tested).
//! - #10a `POST /api/v1/markers` `lock_version` (stale → 409).
//! - #10b `app/controllers/api/v1/scheduled_statuses_controller.rb:22-25`
//!   (empty update persists nothing → 200).
//! - #21j `app/controllers/api/v1/statuses/mutes_controller.rb`
//!   (`set_conversation` raises `ValidationError`/422 for non-direct
//!   statuses).

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::ports::{AccountStore, AdminStore, JobQueue, UserSettingsUpdate};
use birchtree::domain::UserRole;
use birchtree::jobs::handlers;

use common::{TestServer, form_request, get_request, json, send};

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
        "client_name=P2b+Tests&redirect_uris={}&scopes={}",
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
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

/// Every scope this file's endpoints touch.
const ALL_SCOPES: &str = "read write follow profile read:accounts write:accounts \
read:follows read:statuses write:statuses read:notifications write:notifications write:mutes";

async fn users(names: &[&str]) -> (TestServer, HashMap<String, String>) {
    let srv = TestServer::new(true);
    let app = register_app(&srv, ALL_SCOPES).await;
    let mut tokens = HashMap::new();
    for name in names {
        tokens.insert(name.to_string(), register_account(&srv, &app, name).await);
    }
    (srv, tokens)
}

fn tok<'a>(users: &'a HashMap<String, String>, name: &str) -> &'a str {
    users.get(name).expect("user token")
}

fn account_id(srv: &TestServer, name: &str) -> String {
    srv.store
        .account_by_handle(name, "example.social")
        .unwrap()
        .unwrap()
        .id
        .to_string()
}

async fn post(
    srv: &TestServer,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", path, body, token)).await;
    (status, json(&bytes))
}

async fn post_ok(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> Value {
    let (status, body) = post(srv, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "POST {path}: {body}");
    body
}

async fn put(
    srv: &TestServer,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(&srv.router, form_request("PUT", path, body, token)).await;
    (status, json(&bytes))
}

/// `PATCH` form data and return `(status, json body)`.
async fn patch(
    srv: &TestServer,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(&srv.router, form_request("PATCH", path, body, token)).await;
    (status, json(&bytes))
}

async fn patch_ok(srv: &TestServer, path: &str, token: Option<&str>, body: &str) -> Value {
    let (status, body) = patch(srv, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "PATCH {path}: {body}");
    body
}

async fn get_ok(srv: &TestServer, path: &str, token: Option<&str>) -> Value {
    let (status, _h, bytes) = send(&srv.router, get_request(path, token, None)).await;
    assert_eq!(status, StatusCode::OK, "GET {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

/// Run every pending durable job once (notifications are post-commit work).
fn drain_jobs(srv: &TestServer) {
    use birchtree::domain::JobKind;
    let mail = birchtree::mail::LoggingMailSender::default();
    for job in srv.store.claim_due(4_000_000_000_000, 64).unwrap() {
        match job.kind {
            // Fail-closed mail (prod-2 item 4): without SMTP the job refuses;
            // complete it so notification jobs still run.
            JobKind::SendConfirmationEmail | JobKind::SendPasswordResetEmail => {
                assert!(handlers::handle(
                    &job,
                    &*srv.store,
                    &mail,
                    &*srv.blobs,
                    &birchtree::application::ports::NoStreaming,
                )
                .is_err());
                srv.store.complete(job.id, job.due_at_ms).unwrap();
            }
            _ => {
                handlers::handle(
                    &job,
                    &*srv.store,
                    &mail,
                    &*srv.blobs,
                    &birchtree::application::ports::NoStreaming,
                )
                .unwrap();
                srv.store.complete(job.id, job.due_at_ms).unwrap();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// #11a credential role: real role for staff, legacy User shape otherwise.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn credential_role_is_real_for_staff() {
    let (srv, users) = users(&["alice", "mod", "admin"]).await;
    let (alice, moderator, admin) = (tok(&users, "alice"), tok(&users, "mod"), tok(&users, "admin"));
    let mod_id = srv
        .store
        .account_by_handle("mod", "example.social")
        .unwrap()
        .unwrap()
        .id;
    let admin_id = srv
        .store
        .account_by_handle("admin", "example.social")
        .unwrap()
        .unwrap()
        .id;
    srv.store.set_user_role(mod_id, UserRole::Moderator).unwrap();
    srv.store.set_user_role(admin_id, UserRole::Admin).unwrap();

    let plain = get_ok(&srv, "/api/v1/accounts/verify_credentials", Some(alice)).await;
    assert_eq!(plain["role"]["name"], "User");
    assert_eq!(plain["source"]["language"], Value::Null);

    let staff = get_ok(&srv, "/api/v1/accounts/verify_credentials", Some(moderator)).await;
    assert_eq!(staff["role"]["name"], "Moderator");
    assert_eq!(staff["role"]["permissions"], "1049884");
    assert_eq!(staff["role"]["highlighted"], true);

    let boss = get_ok(&srv, "/api/v1/accounts/verify_credentials", Some(admin)).await;
    assert_eq!(boss["role"]["name"], "Admin");
    assert_eq!(boss["role"]["highlighted"], true);
    assert_ne!(boss["role"]["permissions"], "0");
}

// ---------------------------------------------------------------------------
// #11c preferences: posting language cascades default_language -> locale.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preferences_language_prefers_default_then_locale() {
    let (srv, users) = users(&["alice"]).await;
    let alice = tok(&users, "alice");
    let alice_id = srv
        .store
        .account_by_handle("alice", "example.social")
        .unwrap()
        .unwrap()
        .id;

    // Registration locale is `en`; no default set -> cascade yields `en`.
    let prefs = get_ok(&srv, "/api/v1/preferences", Some(alice)).await;
    assert_eq!(prefs["posting:default:language"], "en");
    assert_eq!(prefs["reading:expand:media"], "default");
    assert_eq!(prefs["reading:expand:spoilers"], false);
    assert_eq!(prefs["reading:autoplay:gifs"], false);

    srv.store
        .update_user_settings(
            alice_id,
            UserSettingsUpdate {
                default_language: Some("fr".to_string()),
                ..UserSettingsUpdate::default()
            },
        )
        .unwrap();
    let prefs = get_ok(&srv, "/api/v1/preferences", Some(alice)).await;
    assert_eq!(prefs["posting:default:language"], "fr");
}

// ---------------------------------------------------------------------------
// #11d update_credentials: bot/fields/attribution accepted; limits 422.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_credentials_accepts_new_params_with_upstream_limits() {
    let (srv, users) = users(&["alice"]).await;
    let alice = tok(&users, "alice");

    // Five fields -> 422 like upstream's length validator.
    let mut body = String::from("display_name=Alice");
    for i in 0..5 {
        body.push_str(&format!("&fields_attributes[][name]=f{i}&fields_attributes[][value]=v{i}"));
    }
    let (status, response) = patch(&srv, "/api/v1/accounts/update_credentials", Some(alice), &body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(
        response["error"],
        "Validation failed: Fields is too long (maximum is 4 characters)"
    );

    // A value without a label -> 422 like `EmptyProfileFieldNamesValidator`.
    let (status, response) = patch(
        &srv,
        "/api/v1/accounts/update_credentials",
        Some(alice),
        "fields_attributes[][value]=orphan",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    assert_eq!(
        response["error"],
        "Validation failed: Fields contains values with missing labels"
    );

    // bot + fields + attribution_domains are accepted (200, no pipeline).
    let ok = patch_ok(
        &srv,
        "/api/v1/accounts/update_credentials",
        Some(alice),
        "bot=true&fields_attributes[][name]=site&fields_attributes[][value]=https://example.com\
         &attribution_domains[]=example.com",
    )
    .await;
    assert!(ok["id"].is_string());
    assert_eq!(ok["source"]["privacy"], "public");
}

// ---------------------------------------------------------------------------
// #12a/#12b search + lookup params.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn account_search_gates_and_orders_like_upstream() {
    let (srv, users) = users(&["alice", "bob", "bobby"]).await;
    let alice = tok(&users, "alice");
    let bob_id = account_id(&srv, "bob");

    // Anonymous search requires a user upstream -> 401.
    let (status, _h, _bytes) =
        send(&srv.router, get_request("/api/v1/accounts/search?q=bob", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Blank query -> [].
    let found = get_ok(&srv, "/api/v1/accounts/search?q=%20", Some(alice)).await;
    assert_eq!(found, Value::Array(Vec::new()));

    // Substring search finds both bobs.
    let found = get_ok(&srv, "/api/v1/accounts/search?q=bob", Some(alice)).await;
    let names: Vec<&str> = found
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["username"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"bob") && names.contains(&"bobby"), "{names:?}");

    // Exact handle match sorts first.
    let found = get_ok(&srv, "/api/v1/accounts/search?q=bob@example.social", Some(alice)).await;
    assert_eq!(found[0]["username"], "bob");

    // following=true restricts to followed accounts.
    let before = get_ok(&srv, "/api/v1/accounts/search?q=bob&following=true", Some(alice)).await;
    assert_eq!(before, Value::Array(Vec::new()));
    post_ok(&srv, &format!("/api/v1/accounts/{bob_id}/follow"), Some(alice), "").await;
    drain_jobs(&srv);
    let after = get_ok(&srv, "/api/v1/accounts/search?q=bob&following=true", Some(alice)).await;
    assert!(after.as_array().unwrap().iter().any(|a| a["username"] == "bob"));

    // Suspended accounts never surface.
    srv.store
        .admin_update_account(
            srv.store.account_by_handle("alice", "example.social").unwrap().unwrap().id,
            srv.store.account_by_handle("bobby", "example.social").unwrap().unwrap().id,
            birchtree::application::ports::AdminAccountPatch {
                suspended: Some(true),
                audit_action: "suspend".to_string(),
                ..birchtree::application::ports::AdminAccountPatch::default()
            },
            1,
        )
        .unwrap();
    let found = get_ok(&srv, "/api/v1/accounts/search?q=bobby", Some(alice)).await;
    assert!(
        found.as_array().unwrap().iter().all(|a| a["username"] != "bobby"),
        "{found}"
    );
}

#[tokio::test]
async fn account_lookup_strips_at_and_matches_case_insensitively() {
    let (srv, users) = users(&["alice"]).await;
    let alice = tok(&users, "alice");

    let plain = get_ok(&srv, "/api/v1/accounts/lookup?acct=alice", Some(alice)).await;
    let prefixed = get_ok(&srv, "/api/v1/accounts/lookup?acct=%40alice", None).await;
    assert_eq!(plain["id"], prefixed["id"]);

    let upper = get_ok(
        &srv,
        "/api/v1/accounts/lookup?acct=ALICE%40EXAMPLE.SOCIAL",
        Some(alice),
    )
    .await;
    assert_eq!(upper["id"], plain["id"]);

    let (status, _h, _bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/lookup?acct=nobody-here", Some(alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// #12c relationships with_suspended.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn relationships_hide_suspended_unless_asked() {
    let (srv, users) = users(&["alice", "bob"]).await;
    let alice = tok(&users, "alice");
    let bob_id = account_id(&srv, "bob");
    let alice_id = account_id(&srv, "alice");
    srv.store
        .admin_update_account(
            alice_id.parse::<birchtree::domain::Id>().unwrap(),
            bob_id.parse::<birchtree::domain::Id>().unwrap(),
            birchtree::application::ports::AdminAccountPatch {
                suspended: Some(true),
                audit_action: "suspend".to_string(),
                ..birchtree::application::ports::AdminAccountPatch::default()
            },
            1,
        )
        .unwrap();

    let hidden = get_ok(
        &srv,
        &format!("/api/v1/accounts/relationships?id[]={bob_id}"),
        Some(alice),
    )
    .await;
    assert_eq!(hidden, Value::Array(Vec::new()));

    let shown = get_ok(
        &srv,
        &format!("/api/v1/accounts/relationships?id[]={bob_id}&with_suspended=true"),
        Some(alice),
    )
    .await;
    assert_eq!(shown.as_array().unwrap().len(), 1);
    assert_eq!(shown[0]["id"], bob_id);
}

// ---------------------------------------------------------------------------
// #21h notification params + #8a suspension gate.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notification_index_filters_and_hides_suspended() {
    let (srv, users) = users(&["alice", "bob", "carol"]).await;
    let (alice, bob, carol) = (tok(&users, "alice"), tok(&users, "bob"), tok(&users, "carol"));
    let alice_id = account_id(&srv, "alice");
    let bob_id = account_id(&srv, "bob");

    // Bob + carol follow alice: two follow notifications for alice.
    post_ok(&srv, &format!("/api/v1/accounts/{alice_id}/follow"), Some(bob), "").await;
    post_ok(&srv, &format!("/api/v1/accounts/{alice_id}/follow"), Some(carol), "").await;
    drain_jobs(&srv);
    let items = get_ok(&srv, "/api/v1/notifications", Some(alice)).await;
    assert_eq!(items.as_array().unwrap().len(), 2);

    // account_id narrows to one actor.
    let items = get_ok(
        &srv,
        &format!("/api/v1/notifications?account_id={bob_id}"),
        Some(alice),
    )
    .await;
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["account"]["id"], bob_id);

    // supported_types is accepted (it only gates fallback rendering).
    let items = get_ok(
        &srv,
        "/api/v1/notifications?supported_types[]=mention",
        Some(alice),
    )
    .await;
    assert_eq!(items.as_array().unwrap().len(), 2);

    // Suspending bob hides his notification from the index and 404s show.
    let notif_id = items
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["account"]["id"] == bob_id)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    srv.store
        .admin_update_account(
            alice_id.parse::<birchtree::domain::Id>().unwrap(),
            bob_id.parse::<birchtree::domain::Id>().unwrap(),
            birchtree::application::ports::AdminAccountPatch {
                suspended: Some(true),
                audit_action: "suspend".to_string(),
                ..birchtree::application::ports::AdminAccountPatch::default()
            },
            1,
        )
        .unwrap();
    let items = get_ok(&srv, "/api/v1/notifications", Some(alice)).await;
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_ne!(items[0]["account"]["id"], bob_id);
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/notifications/{notif_id}"), Some(alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// #9a poll vote: empty choices 200 no-op, missing 400, garbage 422.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_empty_choices_are_a_noop() {
    let (srv, users) = users(&["alice", "bob"]).await;
    let (alice, bob) = (tok(&users, "alice"), tok(&users, "bob"));
    let created = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        "status=pick&poll[multiple]=false&poll[expires_in]=3600&poll[options][]=tea&poll[options][]=coffee",
    )
    .await;
    let poll_id = created["poll"]["id"].as_str().unwrap().to_string();

    // Missing key -> 400.
    let (status, body) = post(&srv, &format!("/api/v1/polls/{poll_id}/votes"), Some(bob), "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        serde_json::json!({"error": "param is missing or the value is empty: choices"})
    );

    // Present-but-empty -> 200 with the poll unchanged.
    let (status, body) = post(
        &srv,
        &format!("/api/v1/polls/{poll_id}/votes"),
        Some(bob),
        "choices[]=",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["votes_count"], 0);
    assert_eq!(body["voted"], false);

    // Garbage choice -> 422 like upstream's invalid_choice.
    let (status, body) = post(
        &srv,
        &format!("/api/v1/polls/{poll_id}/votes"),
        Some(bob),
        "choices[]=nope",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"],
        "Validation failed: The chosen vote option does not exist"
    );

    // A real vote still works after the no-op.
    let vote = post_ok(
        &srv,
        &format!("/api/v1/polls/{poll_id}/votes"),
        Some(bob),
        "choices[]=1",
    )
    .await;
    assert_eq!(vote["votes_count"], 1);
}

// ---------------------------------------------------------------------------
// #10a markers lock_version round-trip.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn markers_lock_version_conflicts() {
    let (srv, users) = users(&["alice"]).await;
    let alice = tok(&users, "alice");

    let first = post_ok(&srv, "/api/v1/markers", Some(alice), "home[last_read_id]=42").await;
    assert_eq!(first["home"]["version"], 1);

    // Correct version -> 200 with a bump.
    let second = post_ok(
        &srv,
        "/api/v1/markers",
        Some(alice),
        "home[last_read_id]=43&home[lock_version]=1",
    )
    .await;
    assert_eq!(second["home"]["version"], 2);

    // Stale version -> 409 with the pinned body.
    let (status, body) = post(
        &srv,
        "/api/v1/markers",
        Some(alice),
        "home[last_read_id]=44&home[lock_version]=1",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "Conflict during update, please try again");
}

// ---------------------------------------------------------------------------
// #10b scheduled-update with no params is a 200 no-op.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scheduled_update_without_params_is_a_noop() {
    let (srv, users) = users(&["alice"]).await;
    let alice = tok(&users, "alice");

    // Schedule 10 minutes out (past the 5-minute floor; the test server
    // runs on the real clock, so anchor to now, not to T0).
    let future = (chrono::Utc::now() + chrono::Duration::minutes(10))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let created = post_ok(
        &srv,
        "/api/v1/statuses",
        Some(alice),
        &format!("status=later&scheduled_at={}", enc(&future)),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let at = created["scheduled_at"].as_str().unwrap().to_string();

    // Empty PUT body -> 200 with the row unchanged.
    let (status, body) = put(&srv, &format!("/api/v1/scheduled_statuses/{id}"), Some(alice), "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], id);
    assert_eq!(body["scheduled_at"], at);
}

// ---------------------------------------------------------------------------
// #21j conversation mute needs a direct status (422 otherwise).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversation_mute_rejects_non_direct_statuses() {
    let (srv, users) = users(&["alice"]).await;
    let alice = tok(&users, "alice");

    let public = post_ok(&srv, "/api/v1/statuses", Some(alice), "status=hello").await;
    let public_id = public["id"].as_str().unwrap();

    let (status, body) = post(
        &srv,
        &format!("/api/v1/statuses/{public_id}/mute"),
        Some(alice),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let (status, _body) = post(
        &srv,
        &format!("/api/v1/statuses/{public_id}/unmute"),
        Some(alice),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

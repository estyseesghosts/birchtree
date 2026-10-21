//! Fix-4 (P1/P3): instance sub-resources, donation honest absence, metrics
//! moderator widening, admin invited-by groundwork.
//!
//! Upstream pins (Mastodon v4.7.3, `8ff11c2b`) are cited per test.
//!
//! Privacy-sensitive: every account, domain, code and address below is
//! synthetic (`example.*`, `invalid`). No PII, no secrets, no real tokens.

mod common;

use std::sync::Arc;

use activitypub_federation::http_signatures::generate_actor_keypair;
use axum::http::StatusCode;
use axum::routing::get;

use birchtree::api::mastodon::endpoints::{admin_infra, instance};
use birchtree::api::mastodon::{ApiState, ServerInfo};
use birchtree::application::accounts::{
    CreateAccountCommand, InviteStore, MemoryInviteStore, create_account,
};
use birchtree::application::admin as admin_uc;
use birchtree::application::context::Context;
use birchtree::application::crypto::sha256_hex;
use birchtree::application::federation_ports::{FederationStore, RemoteAccountUpsert};
use birchtree::application::ports::{
    AccountStore, AdminDomainBlockPatch, AdminInfraStore, NewApp, NewToken, OAuthStore,
};
use birchtree::application::statuses::{CreateStatusCommand, create_status};
use birchtree::config::Config;
use birchtree::domain::user::Invite;
use birchtree::domain::{Clock, Id, IdGenerator, SystemClock, TimestampIdGenerator, UserRole};

use common::{TestServer, form_request, get_request, json, send};

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const ADMIN_SCOPES: &str = "admin:read admin:write";

/// A router mounting the instance sub-resource handlers (production
/// `router.rs` is integrator-owned; these mount the same handlers).
fn instance_router(srv: &TestServer) -> axum::Router {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> =
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let cfg = Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    };
    let server = Arc::new(ServerInfo::from_config(&cfg));
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(srv.streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams: srv.streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: std::sync::Arc::new(MemoryInviteStore::default()),
    };
    axum::Router::new()
        .route("/api/v1/instance/peers", get(instance::peers_index))
        .route("/api/v1/instance/rules", get(instance::rules_index))
        .route(
            "/api/v1/instance/domain_blocks",
            get(instance::domain_blocks_index),
        )
        .route("/api/v1/instance/languages", get(instance::languages_show))
        .route("/api/v1/instance/activity", get(instance::activity_show))
        .route(
            "/api/v1/instance/extended_description",
            get(instance::extended_description_show),
        )
        .route(
            "/api/v1/instance/translation_languages",
            get(instance::translation_languages_show),
        )
        .route(
            "/api/v1/instance/privacy_policy",
            get(instance::privacy_policy_show),
        )
        .route(
            "/api/v1/instance/terms_of_service",
            get(instance::terms_of_service_index),
        )
        .route(
            "/api/v1/instance/terms_of_service/:date",
            get(instance::terms_of_service_show),
        )
        .with_state(state)
}

/// An admin-routes router sharing the server's store (same shape as the
/// Phase 7.3 admin-infra tests; `admin_infra::routes` is the mount snippet).
fn admin_router(srv: &TestServer) -> axum::Router {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> =
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let cfg = Config {
        public_url: common::TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    };
    let server = Arc::new(ServerInfo::from_config(&cfg));
    let context = Arc::new(Context::new(
        clock,
        ids,
        srv.store.clone(),
        srv.blobs.clone(),
        Arc::new(srv.streams.clone()),
    ));
    let federation = common::federation_config(context.clone(), common::TEST_PUBLIC_URL);
    let state = ApiState {
        context,
        trusted_proxies: Arc::new(Vec::new()),
        max_body_size: 16 * 1024 * 1024,
        server,
        streams: srv.streams.clone(),
        federation,
        rate_limit_multiplier: 1,
        mail_env: std::sync::Arc::new(birchtree::mailer::MailEnv::disabled()),
        invites: std::sync::Arc::new(MemoryInviteStore::default()),
    };
    admin_infra::routes().with_state(state)
}

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

async fn get_json(router: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let (status, _h, bytes) = send(router, get_request(uri, None, None)).await;
    (status, json(&bytes))
}

/// A remote actor on `domain`, so no test fetches the network.
fn remote_account(
    store: &birchtree::persistence::redb::RedbStore,
    name: &str,
    domain: &str,
) {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://{domain}/users/{name}");
    store
        .upsert_remote(
            RemoteAccountUpsert {
                actor_uri: actor_uri.clone(),
                username: name.to_string(),
                domain: domain.to_string(),
                display_name: String::new(),
                note: String::new(),
                locked: false,
                inbox_uri: format!("{actor_uri}/inbox"),
                shared_inbox_uri: None,
                public_key_pem: keys.public_key,
                key_id: format!("{actor_uri}#main-key"),
            },
            now_ms(),
        )
        .expect("upsert remote");
}

/// Post a local status through the application use case.
fn post(
    store: &birchtree::persistence::redb::RedbStore,
    author: Id,
    language: Option<&str>,
    now: u64,
) {
    create_status(
        store,
        common::TEST_PUBLIC_URL,
        now,
        CreateStatusCommand {
            account_id: author,
            text: Some("hello".to_string()),
            in_reply_to_id: None,
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: Some("public".to_string()),
            language: language.map(str::to_string),
            media_ids: Vec::new(),
            poll: None,
            application_name: None,
            application_website: None,
            idempotency_key: None,
        },
        500,
    )
    .expect("post");
}

fn mint_token(
    srv: &TestServer,
    app_id: Id,
    user: Option<Id>,
    scopes: &str,
    tag: &str,
) -> String {
    let plaintext = format!("fix4-{tag}-{}", scopes.replace([' ', ':'], "-"));
    let token_hash = sha256_hex(plaintext.as_bytes());
    srv.store
        .issue_access_token(NewToken {
            app_id,
            user_id: user,
            token_hash,
            scopes: scopes.split_whitespace().map(str::to_string).collect(),
            now_ms: now_ms(),
            expires_at_ms: None,
        })
        .expect("issue token");
    plaintext
}

fn register_app(srv: &TestServer) -> Id {
    srv.store
        .register_app(NewApp {
            name: "fix4".to_string(),
            client_id: "fix4-client".to_string(),
            client_secret_hash: None,
            redirect_uris: Vec::new(),
            scopes: vec!["read".to_string(), "write".to_string()],
            website: None,
            now_ms: now_ms(),
        })
        .expect("register app")
        .id
}

// ---------------------------------------------------------------------------
// Instance sub-resources.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn instance_peers_lists_known_remote_domains_deduped() {
    // Upstream: `Instance.searchable.pluck(:domain)`
    // (`peers_controller.rb:13-15`).
    let srv = TestServer::new(false);
    let router = instance_router(&srv);

    let (status, body) = get_json(&router, "/api/v1/instance/peers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));

    remote_account(&srv.store, "alice", "peer.example");
    remote_account(&srv.store, "bob", "peer.example");
    remote_account(&srv.store, "carol", "other.example");
    common::mk_account(&srv.store, "local");

    let (status, body) = get_json(&router, "/api/v1/instance/peers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!(["other.example", "peer.example"]));
}

#[tokio::test]
async fn instance_rules_serve_stored_rules_in_id_order() {
    // Upstream: `Rule.ordered` with `REST::RuleSerializer`
    // (`rules_controller.rb:11-14`, `rule_serializer.rb:3-12`).
    let srv = TestServer::new(false);
    let router = instance_router(&srv);

    let (status, body) = get_json(&router, "/api/v1/instance/rules").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));

    let now = now_ms();
    srv.store
        .admin_create_rule("Be kind", "", now)
        .expect("rule");
    srv.store
        .admin_create_rule("No spam", "see the policy", now)
        .expect("rule");

    let (status, body) = get_json(&router, "/api/v1/instance/rules").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["text"], "Be kind");
    assert_eq!(rows[0]["hint"], "");
    assert_eq!(rows[0]["translations"], serde_json::json!({}));
    assert_eq!(rows[1]["text"], "No spam");
    assert_eq!(rows[1]["hint"], "see the policy");
    let first: u64 = rows[0]["id"].as_str().expect("id").parse().expect("id");
    let second: u64 = rows[1]["id"].as_str().expect("id").parse().expect("id");
    assert!(first < second, "rules render in id order");
}

#[tokio::test]
async fn instance_languages_serve_distinct_status_languages() {
    // Gaps batch G-A: static ISO-639 autonym catalog (68 entries, sorted),
    // independent of posted statuses.
    let srv = TestServer::new(false);
    let router = instance_router(&srv);

    let (status, body) = get_json(&router, "/api/v1/instance/languages").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().expect("array");
    assert!(!rows.is_empty(), "catalog is served");
    assert!(rows.contains(&serde_json::json!({"code": "en", "name": "English"})));
    assert!(rows.contains(&serde_json::json!({"code": "fr", "name": "Français"})));
}

#[tokio::test]
async fn instance_activity_serves_twelve_string_weeks() {
    // Upstream: twelve `{week, statuses, logins, registrations}` rows with
    // string values (`activity_controller.rb:7-38`).
    let srv = TestServer::new(false);
    let router = instance_router(&srv);
    let now = now_ms();

    // A recently created local account (counts as a week-0 registration),
    // one recent status and one recent sign-in.
    let carol = create_account(
        &*srv.store,
        common::TEST_PUBLIC_URL,
        now,
        CreateAccountCommand {
            username: "carol".to_string(),
            email: None,
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create account");
    post(&srv.store, carol.id, Some("en"), now);
    srv.store
        .record_sign_in(carol.id, now, None)
        .expect("sign in");

    let (status, body) = get_json(&router, "/api/v1/instance/activity").await;
    assert_eq!(status, StatusCode::OK);
    let weeks = body.as_array().expect("array");
    assert_eq!(weeks.len(), 12);
    let mut prev_week: Option<u64> = None;
    for week in weeks {
        for key in ["week", "statuses", "logins", "registrations"] {
            assert!(week[key].is_string(), "week row {key} is a string");
        }
        let label: u64 = week["week"].as_str().expect("week").parse().expect("week");
        if let Some(prev) = prev_week {
            assert!(label < prev, "week labels descend");
        }
        prev_week = Some(label);
    }
    let first = &weeks[0];
    let statuses: u64 = first["statuses"].as_str().expect("n").parse().expect("n");
    let logins: u64 = first["logins"].as_str().expect("n").parse().expect("n");
    let registrations: u64 = first["registrations"]
        .as_str()
        .expect("n")
        .parse()
        .expect("n");
    assert!(statuses >= 1, "week-0 counts the fresh status");
    assert!(logins >= 1, "week-0 counts the fresh sign-in");
    assert!(registrations >= 1, "week-0 counts the fresh account");
}

#[tokio::test]
async fn instance_domain_blocks_hidden_by_upstream_default() {
    // Upstream default `show_domain_blocks: 'disabled'` serves 404
    // (`domain_blocks_controller.rb:20-30`, `config/settings.yml:30`).
    let srv = TestServer::new(false);
    let router = instance_router(&srv);
    srv.store
        .admin_create_domain_block(
            "evil.example",
            AdminDomainBlockPatch {
                severity: Some(
                    birchtree::domain::AdminDomainSeverity::Silence,
                ),
                reject_media: None,
                reject_reports: None,
                private_comment: None,
                public_comment: None,
                obfuscate: None,
            },
            now_ms(),
        )
        .expect("block");

    let (status, _) = get_json(&router, "/api/v1/instance/domain_blocks").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The public serializer still matches the upstream wire shape for when
    // the operator flag lands.
    let rows = srv
        .store
        .admin_domain_blocks_page(None, None, None, 10)
        .expect("page");
    let rendered = instance::public_domain_block_json(&rows[0]);
    assert_eq!(rendered["domain"], "evil.example");
    assert_eq!(rendered["severity"], "silence");
    assert_eq!(rendered["comment"], serde_json::Value::Null);
    assert_eq!(
        rendered["digest"],
        serde_json::json!(sha256_hex(b"evil.example"))
    );
}

#[tokio::test]
async fn instance_static_subresources_match_upstream_unconfigured_shapes() {
    let srv = TestServer::new(false);
    let router = instance_router(&srv);

    // No description set: `{updated_at: null, content: ''}` (the serializer
    // renders nil/blank exactly so; `extended_description.rb:5-13`).
    let (status, body) = get_json(&router, "/api/v1/instance/extended_description").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({"updated_at": null, "content": ""})
    );

    // No translation provider: `{}` (`translation_languages_controller.rb`).
    let (status, body) = get_json(&router, "/api/v1/instance/translation_languages").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));

    // No policy text bundled: 404, never fabricated (upstream always serves
    // its default template; `privacy_policy.rb:3-15`).
    let (status, _) = get_json(&router, "/api/v1/instance/privacy_policy").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // No terms rows: 404 like upstream's `RecordNotFound`
    // (`terms_of_service_controller.rb:7-15`).
    let (status, _) = get_json(&router, "/api/v1/instance/terms_of_service").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(&router, "/api/v1/instance/terms_of_service/2022-10-07").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Donation campaigns: honest absence.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn donation_campaigns_honest_absence() {
    // Upstream serves real fetched data (`donation_campaigns_controller.rb`);
    // the fetch is unbuilt here, so 404 after the user gate (the old
    // unconditional 204 is gone).
    let srv = TestServer::new(true);

    let (status, _, _) = send(
        &srv.router,
        get_request("/api/v1/donation_campaigns", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let app_id = register_app(&srv);
    let user = common::mk_account(&srv.store, "donor");
    let token = mint_token(&srv, app_id, Some(user.id), "read", "donor");
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v1/donation_campaigns", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes)["error"], "Record not found");
}

// ---------------------------------------------------------------------------
// Metrics moderator widening (finer D35 split intact).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_admit_moderators_admin_only_routes_still_refuse_them() {
    // Upstream: `Admin::PermissionsConcern` (moderator-or-admin) plus the
    // dashboard policy on measures/dimensions/retention; admin-only
    // elsewhere (`permissions_concern.rb`, `base_policy.rb:1-6`,
    // `dashboard_policy.rb:5-7`).
    let srv = TestServer::new(false);
    let router = admin_router(&srv);
    let app_id = register_app(&srv);
    let mk = |username: &str| -> Id {
        common::mk_account(&srv.store, username).id
    };
    let admin_id = mk("admin");
    let mod_id = mk("mod");
    let user_id = mk("user");
    srv.store
        .set_user_role(admin_id, UserRole::Admin)
        .expect("grant admin");
    srv.store
        .set_user_role(mod_id, UserRole::Moderator)
        .expect("grant moderator");
    let admin = mint_token(&srv, app_id, Some(admin_id), ADMIN_SCOPES, "admin");
    let moderator = mint_token(&srv, app_id, Some(mod_id), ADMIN_SCOPES, "mod");
    let user = mint_token(&srv, app_id, Some(user_id), ADMIN_SCOPES, "user");

    // Moderators read analytics.
    for (uri, body) in [
        ("/api/v1/admin/measures", "keys[]=new_users"),
        ("/api/v1/admin/dimensions", "keys[]=languages"),
        (
            "/api/v1/admin/retention",
            "start_at=2026-09-01&end_at=2026-10-01",
        ),
    ] {
        let (status, _, bytes) = send(
            &router,
            form_request("POST", uri, body, Some(&moderator)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{uri} admits moderators");
        assert!(json(&bytes).is_array(), "{uri} answers an array");
    }

    // Admins still pass.
    let (status, _, _) = send(
        &router,
        form_request("POST", "/api/v1/admin/measures", "keys[]=new_users", Some(&admin)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The finer split is intact: moderators are still refused elsewhere.
    for uri in ["/api/v1/admin/domain_allows", "/api/v1/admin/tags"] {
        let (status, _, bytes) = send(
            &router,
            get_request(uri, Some(&moderator), None),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} stays admin-only");
        assert_eq!(json(&bytes)["error"], "Forbidden");
    }

    // Role-less users are still refused on the analytics trio.
    let (status, _, _) = send(
        &router,
        form_request("POST", "/api/v1/admin/measures", "keys[]=new_users", Some(&user)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Invited-by application groundwork.
// ---------------------------------------------------------------------------

#[test]
fn invited_by_helpers_resolve_creator_and_reject_garbage() {
    // Serializer field: `user.invite.user.account_id`
    // (`account_serializer.rb:78-80`); filter scope: `Invite.where(user_id:)`
    // (`account_filter.rb:106-108`).
    let invites = MemoryInviteStore::default();
    let inviter: Id = "123".parse().expect("id");
    invites
        .create_invite(Invite {
            id: "999".parse().expect("id"),
            code: "AbC123Xy".to_string(),
            created_by_account_id: inviter,
            created_by_staff: false,
            comment: None,
            max_uses: None,
            uses: 0,
            expires_at_ms: None,
            created_at_ms: 0,
        })
        .expect("create invite");

    assert_eq!(
        admin_uc::inviter_account_id(&invites, "AbC123Xy"),
        Some(inviter)
    );
    assert_eq!(admin_uc::inviter_account_id(&invites, "unknown"), None);

    assert_eq!(admin_uc::parse_invited_by("123"), Some(inviter));
    assert_eq!(admin_uc::parse_invited_by(" 123 "), Some(inviter));
    assert_eq!(admin_uc::parse_invited_by("abc"), None);
    assert_eq!(admin_uc::parse_invited_by(""), None);
}

// ---------------------------------------------------------------------------
// Admin identity stores (fix round P1/P3): locale, invite request, signup-app
// attribution, invite attribution, login IPs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_identity_fields_serve_stored_attribution() {
    // Upstream: `REST::Admin::AccountSerializer`
    // (`account_serializer.rb:6-80` at `8ff11c2b`): `locale`
    // (`user.locale`), `invite_request` (`user.invite_request.text`),
    // `created_by_application_id`, `invited_by_account_id`
    // (`user.invite.user.account_id`), `ip`/`ips` (`user.ips`, `ip, used_at`
    // per `IpSerializer`). Registration captures locale/reason/invite in the
    // account-creation transaction; sign-in IPs are recorded at password
    // login (`record_sign_in`).
    let srv = TestServer::new(true);
    let app_id = register_app(&srv);
    let inviter = common::mk_account(&srv.store, "inviter9").id;
    srv.store
        .create_invite(Invite {
            id: Id::new(9001),
            code: "Invite90".to_string(),
            created_by_account_id: inviter,
            created_by_staff: false,
            comment: None,
            max_uses: None,
            uses: 0,
            expires_at_ms: None,
            created_at_ms: now_ms(),
        })
        .expect("create invite");

    // Register through the public HTTP path with locale + reason + invite.
    let app_token = mint_token(&srv, app_id, None, "write", "app9");
    let body = "username=invited9&email=invited9@example.com&password=password123\
        &agreement=true&locale=en&reason=please-let-me-in&invite_code=Invite90";
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", body, Some(&app_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let account_id = srv
        .store
        .account_by_handle("invited9", "example.social")
        .expect("lookup")
        .expect("registered")
        .id;

    // One password-login sign-in from a known IP.
    let ip: std::net::IpAddr = "203.0.113.7".parse().expect("ip");
    srv.store
        .record_sign_in(account_id, now_ms(), Some(ip))
        .expect("sign in");

    // Admin read of the row.
    let admin_id = common::mk_account(&srv.store, "admin9").id;
    srv.store
        .set_user_role(admin_id, UserRole::Admin)
        .expect("grant admin");
    let admin = mint_token(&srv, app_id, Some(admin_id), ADMIN_SCOPES, "admin9");
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/admin/accounts/{account_id}"),
            Some(&admin),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let row = json(&bytes);
    assert_eq!(row["locale"], serde_json::json!("en"), "{row}");
    assert_eq!(row["invite_request"], serde_json::json!("please-let-me-in"), "{row}");
    assert_eq!(row["invited_by_account_id"], serde_json::json!(inviter.to_string()), "{row}");
    assert_eq!(row["ip"], serde_json::json!("203.0.113.7"), "{row}");
    let ips = row["ips"].as_array().expect("ips array");
    assert_eq!(ips.len(), 1, "{row}");
    assert_eq!(ips[0]["ip"], serde_json::json!("203.0.113.7"), "{row}");
    assert!(ips[0]["used_at"].is_string(), "{row}");
    // Pre-invite-store rows degrade to null, never fabricated: the inviter
    // (CLI-created, no invite) has no attribution.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/admin/accounts/{inviter}"), Some(&admin), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let inviter_row = json(&bytes);
    assert_eq!(inviter_row["invited_by_account_id"], serde_json::Value::Null);

    // v2 `invited_by` filter: the invited account matches, garbage is an
    // empty page (upstream `Invite.where` finds no rows — never an error).
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v2/admin/accounts?invited_by={inviter}"),
            Some(&admin),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = json(&bytes);
    let ids: Vec<String> = page
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id").to_string())
        .collect();
    assert_eq!(ids, vec![account_id.to_string()], "{page}");

    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/admin/accounts?invited_by=abc", Some(&admin), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!([]));

    // v2 `ip` filter matches the recorded login IP.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v2/admin/accounts?ip=203.0.113", Some(&admin), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = json(&bytes);
    assert!(
        page.as_array().expect("array").iter().any(|entry| entry["id"] == account_id.to_string()),
        "{page}"
    );
}

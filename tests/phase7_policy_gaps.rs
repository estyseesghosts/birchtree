//! Phase 7 policy gaps D22/D23/D25/D26 (operator decisions 2026-10-20).
//!
//! - D22 (do NOT match upstream by default): per-user `Show` (default) /
//!   `Collapse` / `Filter` handling of home statuses that mention a
//!   blocked/muted account, applied in the REST home filter and the home
//!   stream gate (`home_includes`).
//! - D23 (implement): per-follow `languages` applied on home like upstream
//!   `filter_from_home`.
//! - D25 (follow upstream): NFKC-folding `normalize_tag` parity; `#café` and
//!   `#cafe` are one tag; plus the tag-backfill planner.
//! - D26 (follow upstream `NotifyService`): drop conditions at creation; the
//!   retroactive rule is LEAVE (pre-existing notifications stay — see
//!   `notification_drop` docs and the `d26_` retroactive test).
//!
//! Contract dependencies (parent patches, see the Phase 7 report's PATCHES
//! FOR PARENT): `LocalUser.home_mention_policy` + `update_credentials`
//! `home_mention_policy` param, `Follow.languages` + `FollowCommand`/
//! `FollowOptions` `languages` + `POST follow` `languages[]` param +
//! relationship `languages` rendering, `normalize_tag` on tag writes +
//! widened `extract_tags`, status-DTO `filtered` fold entry, `jobs::mod`
//! `tag_backfill` declaration. Pure-logic tests pass on this tree alone;
//! behavior tests marked NEEDS-PATCH require those patches.

mod common;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::notifications::{
    fan_out_from_payload, fan_out_payload, notification_drop,
};
use birchtree::application::policy::{
    follow_language_allows, normalize_follow_languages, HomeMentionPolicy,
};
use birchtree::application::ports::{
    AccountStore, StatusStore, TimelineQuery,
};
use birchtree::application::relationships::{self, FollowCommand};
use birchtree::application::statuses::{self, CreateStatusCommand, normalize_tag};
use birchtree::application::timelines;
use birchtree::domain::{Id, NotificationType};
use birchtree::jobs::tag_backfill::{
    plan_backfill_batch, plan_status_tags, plan_tag_renames, tags_need_rewrite,
    BACKFILL_BATCH_CAP,
};

use common::{form_request, get_request, json, mk_account, send, temp_store, TestServer, T0};

// ----- shared helpers --------------------------------------------------------

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

async fn user_token(srv: &TestServer, username: &str) -> String {
    let body = format!(
        "client_name=PolicyGaps&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc("read write"),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app = json(&bytes);
    let token_body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope=write",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &token_body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app_token = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let account_body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &account_body, Some(&app_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

fn account_id(srv: &TestServer, username: &str) -> String {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
        .to_string()
}

async fn post_ok(srv: &TestServer, method: &str, path: &str, token: &str, body: &str) -> Value {
    let (status, _h, bytes) =
        send(&srv.router, form_request(method, path, body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{method} {path}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn create_with(srv: &TestServer, token: &str, body: &str) -> String {
    post_ok(srv, "POST", "/api/v1/statuses", token, body).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn status_form(text: &str, extra: &str) -> String {
    let mut body = format!("status={}&visibility=public", enc(text));
    if !extra.is_empty() {
        body.push('&');
        body.push_str(extra);
    }
    body
}

async fn get_json(srv: &TestServer, uri: &str, token: Option<&str>) -> Value {
    let (status, _h, bytes) = send(&srv.router, get_request(uri, token, None)).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

fn ids_of(body: &Value) -> Vec<String> {
    body.as_array()
        .unwrap_or_else(|| panic!("array expected: {body}"))
        .iter()
        .map(|status| status["id"].as_str().unwrap().to_string())
        .collect()
}

fn post_status(
    store: &dyn birchtree::application::ports::Store,
    author: Id,
    text: &str,
    language: Option<&str>,
) -> birchtree::domain::Status {
    statuses::create_status(
        store,
        "https://example.social",
        T0,
        CreateStatusCommand {
            account_id: author,
            text: Some(text.to_string()),
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
    .expect("post status")
    .status
}

fn home_query() -> TimelineQuery {
    TimelineQuery {
        limit: 20,
        ..Default::default()
    }
}

// ----- D22: mention blocks/mutes on home -------------------------------------

#[test]
fn d22_policy_parsing_and_defaults() {
    assert_eq!(HomeMentionPolicy::from_stored(None), HomeMentionPolicy::Show);
    assert_eq!(HomeMentionPolicy::from_stored(Some("show")), HomeMentionPolicy::Show);
    assert_eq!(HomeMentionPolicy::from_stored(Some("bogus")), HomeMentionPolicy::Show);
    assert_eq!(HomeMentionPolicy::from_stored(Some("collapse")), HomeMentionPolicy::Collapse);
    assert_eq!(HomeMentionPolicy::from_stored(Some("fold")), HomeMentionPolicy::Collapse);
    assert_eq!(HomeMentionPolicy::from_stored(Some("filter")), HomeMentionPolicy::Filter);
    assert_eq!(HomeMentionPolicy::from_stored(Some("hide")), HomeMentionPolicy::Filter);
    assert_eq!(HomeMentionPolicy::Show.as_str(), "show");
    assert_eq!(HomeMentionPolicy::Collapse.as_str(), "collapse");
    assert_eq!(HomeMentionPolicy::Filter.as_str(), "filter");
    assert_eq!(HomeMentionPolicy::default(), HomeMentionPolicy::Show);
}

#[test]
fn d22_default_show_keeps_mention_blocked_status_on_home() {
    let (_dir, store) = temp_store();
    let viewer = mk_account(&store, "viewer").id;
    let author = mk_account(&store, "author").id;
    let victim = mk_account(&store, "victim").id;
    relationships::follow(
        &*store,
        T0,
        FollowCommand {
            follower_id: viewer,
            target_id: author,
            show_reblogs: None,
            notify: None,
            // NEEDS-PATCH P2: `FollowCommand.languages` (None = no preference).
            languages: None,
        },
    )
    .expect("follow");
    let posted = post_status(&*store, author, "@victim hello", None);
    assert!(posted.mentions.contains(&victim));
    relationships::block(&*store, viewer, victim, T0).expect("block");

    // Default policy: the status stays on home (upstream would drop it).
    assert_eq!(timelines::home_mention_policy_of(&*store, viewer), HomeMentionPolicy::Show);
    let page = timelines::home(&*store, viewer, home_query(), T0).expect("home");
    assert!(page.statuses.iter().any(|s| s.id == posted.id));
    assert!(timelines::home_includes(&*store, viewer, &posted, T0).expect("gate"));
    assert!(!timelines::home_mention_fold(&*store, viewer, &posted, T0).expect("fold"));
}

#[tokio::test]
async fn d22_filter_mode_removes_from_rest_home_and_stream_gate() {
    // NEEDS-PATCH P1 (pref plumbing) + P6 (DTO/stream no-op note): without the
    // pref patch this stays in `Show` and the exclusion assertions fail.
    let srv = TestServer::new(true);
    let viewer = user_token(&srv, "viewer").await;
    let author = user_token(&srv, "author").await;
    let _ = user_token(&srv, "victim").await;
    let author_id = account_id(&srv, "author");
    let victim_id = account_id(&srv, "victim");
    let viewer_id: Id = account_id(&srv, "viewer").parse().expect("viewer id");

    post_ok(&srv, "POST", &format!("/api/v1/accounts/{author_id}/follow"), &viewer, "").await;
    let posted = create_with(&srv, &author, &status_form("@victim hello", "")).await;
    post_ok(&srv, "POST", &format!("/api/v1/accounts/{victim_id}/block"), &viewer, "").await;

    // Default first: still shown.
    let home = ids_of(&get_json(&srv, "/api/v1/timelines/home", Some(&viewer)).await);
    assert!(home.contains(&posted), "default show keeps it: {home:?}");

    // Filter mode: gone from REST home and from the shared home gate (which is
    // also the home stream gate — see streaming/fanout.rs).
    post_ok(&srv, "PATCH", "/api/v1/accounts/update_credentials", &viewer, "home_mention_policy=filter").await;
    let home = ids_of(&get_json(&srv, "/api/v1/timelines/home", Some(&viewer)).await);
    assert!(!home.contains(&posted), "filter mode removes it: {home:?}");
    let status = srv
        .store
        .status_by_id(posted.parse().expect("status id"))
        .expect("store")
        .expect("status");
    assert!(!timelines::home_includes(&*srv.store, viewer_id, &status, T0).expect("gate"));

    // Unknown values fall back to show.
    post_ok(&srv, "PATCH", "/api/v1/accounts/update_credentials", &viewer, "home_mention_policy=bogus").await;
    let home = ids_of(&get_json(&srv, "/api/v1/timelines/home", Some(&viewer)).await);
    assert!(home.contains(&posted), "unknown falls back to show: {home:?}");
}

#[tokio::test]
async fn d22_collapse_mode_keeps_with_fold_marker() {
    // NEEDS-PATCH P1 (pref plumbing) + P7 (DTO `filtered` fold entry).
    let srv = TestServer::new(true);
    let viewer = user_token(&srv, "viewer").await;
    let author = user_token(&srv, "author").await;
    let author_id = account_id(&srv, "author");
    let _ = user_token(&srv, "victim").await;
    let victim_id = account_id(&srv, "victim");
    let viewer_id: Id = account_id(&srv, "viewer").parse().expect("viewer id");

    post_ok(&srv, "POST", &format!("/api/v1/accounts/{author_id}/follow"), &viewer, "").await;
    let posted = create_with(&srv, &author, &status_form("@victim hello", "")).await;
    post_ok(&srv, "POST", &format!("/api/v1/accounts/{victim_id}/block"), &viewer, "").await;

    post_ok(&srv, "PATCH", "/api/v1/accounts/update_credentials", &viewer, "home_mention_policy=collapse").await;
    let home = get_json(&srv, "/api/v1/timelines/home", Some(&viewer)).await;
    let entry = home
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"].as_str() == Some(posted.as_str()))
        .unwrap_or_else(|| panic!("collapse keeps it: {home}"));
    let folds: Vec<&Value> = entry["filtered"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["filter"]["id"] == "home-mention-fold")
        .collect();
    assert_eq!(folds.len(), 1, "one fold marker: {entry}");
    assert_eq!(folds[0]["filter"]["filter_action"], "warn");

    // The application helper agrees (this is what the DTO patch calls).
    let status = srv
        .store
        .status_by_id(posted.parse().expect("status id"))
        .expect("store")
        .expect("status");
    assert!(timelines::home_mention_fold(&*srv.store, viewer_id, &status, T0).expect("fold"));
    assert!(timelines::home_includes(&*srv.store, viewer_id, &status, T0).expect("gate"));
}

// ----- D23: follow languages -------------------------------------------------

#[test]
fn d23_language_helper_rules() {
    // No preference -> everything kept.
    assert!(follow_language_allows(None, Some("es")));
    assert!(follow_language_allows(Some(&[]), Some("es")));
    // Language-less statuses pass any preference (upstream: only filters when
    // the status HAS a language the follow does not list).
    assert!(follow_language_allows(Some(&["en".to_string()]), None));
    // Mismatch filtered, match kept (case-insensitive).
    assert!(!follow_language_allows(Some(&["en".to_string()]), Some("es")));
    assert!(follow_language_allows(Some(&["en".to_string()]), Some("en")));
    assert!(follow_language_allows(Some(&["EN".to_string()]), Some("en")));
    assert!(follow_language_allows(
        Some(&["en".to_string(), "pt-BR".to_string()]),
        Some("pt-br")
    ));
    // Normalization for the write path.
    assert_eq!(
        normalize_follow_languages(&["EN".to_string(), "pt-BR".to_string(), "xx!".to_string(), String::new()]),
        vec!["en".to_string(), "pt-br".to_string()]
    );
    // Duplicates collapse.
    assert_eq!(
        normalize_follow_languages(&["en".to_string(), "EN".to_string()]),
        vec!["en".to_string()]
    );
}

#[test]
fn d23_no_follow_language_preference_keeps_all_languages() {
    let (_dir, store) = temp_store();
    let viewer = mk_account(&store, "viewer").id;
    let author = mk_account(&store, "author").id;
    relationships::follow(
        &*store,
        T0,
        FollowCommand {
            follower_id: viewer,
            target_id: author,
            show_reblogs: None,
            notify: None,
            // NEEDS-PATCH P2: `FollowCommand.languages`.
            languages: None,
        },
    )
    .expect("follow");
    let es = post_status(&*store, author, "hola", Some("es"));
    let page = timelines::home(&*store, viewer, home_query(), T0).expect("home");
    assert!(page.statuses.iter().any(|s| s.id == es.id));
}

#[tokio::test]
async fn d23_follow_languages_filter_home_and_roundtrip() {
    // NEEDS-PATCH P2 (follow languages storage + `languages[]` param) + P3
    // (relationship `languages` rendering).
    let srv = TestServer::new(true);
    let viewer = user_token(&srv, "viewer").await;
    let author = user_token(&srv, "author").await;
    let author_id = account_id(&srv, "author");
    let viewer_id: Id = account_id(&srv, "viewer").parse().expect("viewer id");

    let rel = post_ok(
        &srv,
        "POST",
        &format!("/api/v1/accounts/{author_id}/follow"),
        &viewer,
        "reblogs=true&languages[]=es",
    )
    .await;
    assert_eq!(rel["languages"], Value::Array(vec![Value::String("es".to_string())]));

    let kept = create_with(&srv, &author, &status_form("hola", "language=es")).await;
    let dropped = create_with(&srv, &author, &status_form("bonjour", "language=fr")).await;
    let unprefixed = create_with(&srv, &author, &status_form("no language", "")).await;

    let home = ids_of(&get_json(&srv, "/api/v1/timelines/home", Some(&viewer)).await);
    assert!(home.contains(&kept), "match kept: {home:?}");
    assert!(!home.contains(&dropped), "mismatch filtered: {home:?}");
    assert!(home.contains(&unprefixed), "language-less kept: {home:?}");

    // The relationship read echoes the stored preference (was `null`).
    let rels = get_json(&srv, &format!("/api/v1/accounts/relationships?id={author_id}"), Some(&viewer)).await;
    assert_eq!(rels[0]["languages"], Value::Array(vec![Value::String("es".to_string())]));

    // The shared home gate agrees with the REST filter.
    let dropped_status = srv
        .store
        .status_by_id(dropped.parse().expect("status id"))
        .expect("store")
        .expect("status");
    assert!(!timelines::home_includes(&*srv.store, viewer_id, &dropped_status, T0).expect("gate"));
    let kept_status = srv
        .store
        .status_by_id(kept.parse().expect("status id"))
        .expect("store")
        .expect("status");
    assert!(timelines::home_includes(&*srv.store, viewer_id, &kept_status, T0).expect("gate"));
}

// ----- D25: hashtag normalization --------------------------------------------

#[test]
fn d25_normalize_tag_nfkc_parity() {
    // The headline case: `#café` and `#cafe` are one tag.
    assert_eq!(normalize_tag("café"), "cafe");
    assert_eq!(normalize_tag("cafe"), "cafe");
    assert_eq!(normalize_tag("#Café"), "cafe");
    assert_eq!(normalize_tag("CAFÉ"), "cafe");
    // Decomposed form (e + combining acute) converges too.
    assert_eq!(normalize_tag("café"), "cafe");
    // NFKC compatibility folds.
    assert_eq!(normalize_tag("ＡＢＣ"), "abc");
    assert_eq!(normalize_tag("ﬁsh"), "fish");
    // Existing behavior preserved: ASCII lowercase, alnum/`_` kept, other
    // ASCII punctuation dropped.
    assert_eq!(normalize_tag("#Music"), "music");
    assert_eq!(normalize_tag("hello-world"), "helloworld");
    assert_eq!(normalize_tag("a_b2"), "a_b2");
    assert_eq!(normalize_tag("#"), "");
    assert_eq!(normalize_tag(""), "");
    assert_eq!(normalize_tag("münchen"), "munchen");
    assert_eq!(normalize_tag("naïve"), "naive");
    // Upstream keeps these separators/letters (HASHTAG_INVALID_CHARS_RE).
    assert_eq!(normalize_tag("a·b"), "a·b");
    // `ß` has no NFKC/ASCII fold (upstream keeps it as-is).
    assert_eq!(normalize_tag("straße"), "straße");
}

#[test]
fn d25_backfill_planner_converges_idempotently_and_bounded() {
    // Converged spellings plan nothing; variants collapse onto one canonical.
    assert!(plan_tag_renames(&["cafe".to_string(), "music".to_string()]).is_empty());
    let renames = plan_tag_renames(&["café".to_string(), "cafe".to_string(), "CAFÉ".to_string()]);
    assert!(renames.contains(&("café".to_string(), "cafe".to_string())));
    assert!(!renames.iter().any(|(from, _)| from == "cafe"));
    // Re-planning the outputs is a fixed point (restartable + idempotent).
    let outputs: Vec<String> = renames.iter().map(|(_, to)| to.clone()).collect();
    assert!(plan_tag_renames(&outputs).is_empty());

    // Status-level re-derivation. NEEDS-PATCH P4 (widened `extract_tags` +
    // normalized tag writes): until then `#café` extracts as `caf`.
    assert_eq!(plan_status_tags("#Café #cafe #music"), vec!["cafe".to_string(), "music".to_string()]);
    assert!(tags_need_rewrite(&["cafe".to_string()], "#café").is_none());
    assert_eq!(
        tags_need_rewrite(&["caf".to_string()], "#café"),
        Some(vec!["cafe".to_string()])
    );

    // Batches are bounded and deterministic.
    let rows: Vec<(Id, Vec<String>, String)> = (0..BACKFILL_BATCH_CAP + 10)
        .map(|i| (Id::new(1000 + i as u64), vec!["stale".to_string()], format!("#fresh{i}")))
        .collect();
    let ops = plan_backfill_batch(&rows);
    assert_eq!(ops.len(), BACKFILL_BATCH_CAP);
    assert_eq!(ops[0].status_id, Id::new(1000));
    assert_eq!(ops[0].new_tags, vec!["fresh0".to_string()]);
}

#[test]
fn d25_tag_timeline_unifies_cafe_variants() {
    // NEEDS-PATCH P4 (widened `extract_tags` + normalized tag writes).
    let (_dir, store) = temp_store();
    let viewer = mk_account(&store, "viewer").id;
    let author = mk_account(&store, "author").id;
    let first = post_status(&*store, author, "#café au lait", None);
    let second = post_status(&*store, author, "#cafe con leche", None);
    assert_eq!(first.tags, vec!["cafe".to_string()]);
    assert_eq!(second.tags, vec!["cafe".to_string()]);
    for query in ["café", "cafe", "#CAFÉ"] {
        let page = timelines::tag(&*store, Some(viewer), query, home_query(), T0).expect("tag");
        let ids: Vec<Id> = page.statuses.iter().map(|s| s.id).collect();
        assert!(ids.contains(&first.id) && ids.contains(&second.id), "query {query}: {ids:?}");
    }
}

// ----- D26: notification drop conditions -------------------------------------

fn fanout_follow(
    store: &dyn birchtree::application::ports::Store,
    recipient: Id,
    actor: Id,
) -> Option<Id> {
    let payload = fan_out_payload(recipient, NotificationType::Follow, Some(actor), None, T0);
    fan_out_from_payload(store, &payload).expect("fan-out")
}

fn notifications_of(
    store: &dyn birchtree::application::ports::Store,
    recipient: Id,
) -> Vec<birchtree::domain::Notification> {
    store
        .notifications_page(recipient, 20, None, None, None)
        .expect("notifications page")
}

#[test]
fn d26_control_delivers_without_relations() {
    let (_dir, store) = temp_store();
    let recipient = mk_account(&store, "recipient").id;
    let actor = mk_account(&store, "actor").id;
    assert!(!notification_drop(&*store, recipient, actor, T0).expect("drop check"));
    assert!(fanout_follow(&*store, recipient, actor).is_some());
    assert_eq!(notifications_of(&*store, recipient).len(), 1);
}

#[test]
fn d26_block_drops() {
    let (_dir, store) = temp_store();
    let recipient = mk_account(&store, "recipient").id;
    let actor = mk_account(&store, "actor").id;
    relationships::block(&*store, recipient, actor, T0).expect("block");
    assert!(notification_drop(&*store, recipient, actor, T0).expect("drop check"));
    assert!(fanout_follow(&*store, recipient, actor).is_none());
    assert!(notifications_of(&*store, recipient).is_empty());
}

#[test]
fn d26_blocked_by_drops() {
    let (_dir, store) = temp_store();
    let recipient = mk_account(&store, "recipient").id;
    let actor = mk_account(&store, "actor").id;
    relationships::block(&*store, actor, recipient, T0).expect("block");
    assert!(notification_drop(&*store, recipient, actor, T0).expect("drop check"));
    assert!(fanout_follow(&*store, recipient, actor).is_none());
    assert!(notifications_of(&*store, recipient).is_empty());
}

#[test]
fn d26_mute_drops_even_when_notifications_kept() {
    // Operator text says "mutes" (any active mute), deliberately broader than
    // upstream `NotifyService`, which only honors notification-hiding mutes.
    for hide_notifications in [true, false] {
        let (_dir, store) = temp_store();
        let recipient = mk_account(&store, "recipient").id;
        let actor = mk_account(&store, "actor").id;
        relationships::mute(&*store, recipient, actor, 0, hide_notifications, T0).expect("mute");
        assert!(
            notification_drop(&*store, recipient, actor, T0).expect("drop check"),
            "hide={hide_notifications}"
        );
        assert!(fanout_follow(&*store, recipient, actor).is_none());
        assert!(notifications_of(&*store, recipient).is_empty());
    }
}

#[test]
fn d26_domain_block_drops_unless_following() {
    let (_dir, store) = temp_store();
    let recipient = mk_account(&store, "recipient").id;
    let actor = mk_account(&store, "actor").id;
    relationships::domain_block(&*store, recipient, "example.social", T0).expect("domain block");
    // Not following: dropped (upstream `domain_blocking?` needs `not_following?`
    // to spare, i.e. a followed account on a blocked domain still notifies).
    assert!(notification_drop(&*store, recipient, actor, T0).expect("drop check"));
    assert!(fanout_follow(&*store, recipient, actor).is_none());

    // Following the actor spares the notification despite the domain block.
    relationships::follow(
        &*store,
        T0,
        FollowCommand {
            follower_id: recipient,
            target_id: actor,
            show_reblogs: None,
            notify: None,
            // NEEDS-PATCH P2: `FollowCommand.languages`.
            languages: None,
        },
    )
    .expect("follow");
    assert!(!notification_drop(&*store, recipient, actor, T0).expect("drop check"));
    assert!(fanout_follow(&*store, recipient, actor).is_some());
}

#[test]
fn d26_retroactive_rule_leaves_preexisting_notifications() {
    // Operator decision: pre-existing notifications are LEFT in place when a
    // block/mute/domain-block lands later (historical record behind the
    // `markers` cursor; upstream `NotifyService` has no retroactive deletion;
    // open streams already retract via `retraction_candidates`).
    let (_dir, store) = temp_store();
    let recipient = mk_account(&store, "recipient").id;
    let actor = mk_account(&store, "actor").id;
    let created = fanout_follow(&*store, recipient, actor).expect("created");
    assert_eq!(notifications_of(&*store, recipient).len(), 1);

    relationships::block(&*store, recipient, actor, T0).expect("block");
    // The old notification stays readable...
    let kept = notifications_of(&*store, recipient);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, created);
    // ...while new fan-out from the same actor drops.
    assert!(fanout_follow(&*store, recipient, actor).is_none());
    assert_eq!(notifications_of(&*store, recipient).len(), 1);
}

#[tokio::test]
async fn d25_edits_recompute_tags_and_move_index_keys() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let created = create_with(&srv, &alice, &status_form("#café au lait", "")).await;
    let id: Id = created.parse().expect("status id");
    let stored = srv.store.status_by_id(id).expect("store").expect("status");
    assert_eq!(stored.tags, vec!["cafe".to_string()]);

    // Edit swaps the hashtag: stored list recomputed, index moved.
    post_ok(
        &srv,
        "PUT",
        &format!("/api/v1/statuses/{created}"),
        &alice,
        &status_form("#music loudly", ""),
    )
    .await;
    let edited = srv.store.status_by_id(id).expect("store").expect("status");
    assert_eq!(edited.tags, vec!["music".to_string()]);
    assert!(edited.edited_at_ms.is_some());
    let old = get_json(&srv, "/api/v1/timelines/tag/cafe", Some(&alice)).await;
    assert!(
        !old.as_array().unwrap().iter().any(|s| s["id"] == created),
        "old tag released: {old}"
    );
    let new = get_json(&srv, "/api/v1/timelines/tag/music", Some(&alice)).await;
    assert!(
        new.as_array().unwrap().iter().any(|s| s["id"] == created),
        "new tag indexed: {new}"
    );

    // A no-op PUT still converges (tags unchanged, no new version).
    let before = edited.edited_at_ms;
    post_ok(
        &srv,
        "PUT",
        &format!("/api/v1/statuses/{created}"),
        &alice,
        &status_form("#music loudly", ""),
    )
    .await;
    let same = srv.store.status_by_id(id).expect("store").expect("status");
    assert_eq!(same.tags, vec!["music".to_string()]);
    assert_eq!(same.edited_at_ms, before, "no-op edit bumps nothing");
}

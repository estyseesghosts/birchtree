//! Fix-round P2c evidence (push / media / timelines / federation).
//!
//! - #16a push cleanup: every 4xx except 408/429 destroys the subscription.
//! - #16b VAPID `exp` is 24 h.
//! - #16c `Unsubscribe-URL` is plumbed through delivery (header sent when set).
//! - #17c nodeinfo `metadata`: proposed (needs `FedContext` fields; snippets
//!   in the fix log) — not pinned here.
//! - #18 reports: `category` enum-gated (`rule_ids` force `violation`),
//!   `forward_to_domains`, `rule_ids`, `collection_ids`.
//! - #19 outbox/followers/following pages (implemented, not amended).
//! - #20d audience exact-match (the author's own followers URI).
//! - #20b bounded dedup window + D33 rule helpers.
//! - #20c webfinger suspended → 410.
//! - #20e sensitized forcing on ingest input.
//! - #20f BCP47 language canonicalization at parse.
//! - #21a tag `any`/`all`/`none`.
//! - #21f hashtag subscription normalization (pinned by test).
//!
//! Media rows 15a/b were REFUTED (guard present) — no test. #20a (retry
//! budget), #20f caps/timestamps, #21d (locked-down gates), #17a/#17c value
//! sources are register/propose items in the fix log — no test.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use birchtree::activitypub::activities::{
    audience_visibility, note_input_for, question_input_for,
};
use birchtree::activitypub::wire::{
    canonical_language_tag, followers_uri_of, Note, Question, QuestionOption,
    QuestionReplies,
};
use birchtree::api::mastodon::endpoints::reports::report_category;
use birchtree::application::push::{
    build_push_request, classify_push_response, deliver_push, PushOutcome,
    VapidKeys, VAPID_JWT_LIFETIME_SECS,
};
use birchtree::domain::{Id, Visibility};
use birchtree::federation::dedup::{entry_live, prune_cutoff_ms, DEDUP_TTL_MS};
use birchtree::streaming::channels::{Channel, Params, StreamKind};

use common::{form_request, get_request, json, send, TestServer};

const PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";

// ---------------------------------------------------------------------------
// #16a push cleanup: every 4xx except 408/429 destroys.
// Upstream `Web::PushNotificationWorker#send` (`8ff11c2b`).
// ---------------------------------------------------------------------------

#[test]
fn push_cleanup_destroys_on_all_4xx_except_408_and_429() {
    assert_eq!(VAPID_JWT_LIFETIME_SECS, 24 * 3600, "#16b: VAPID exp is 24h");
    for status in [400, 401, 403, 404, 410, 422, 451, 499] {
        assert_eq!(
            classify_push_response(status),
            PushOutcome::Gone,
            "4xx {status} destroys the subscription"
        );
    }
    for status in [200, 201, 204] {
        assert_eq!(
            classify_push_response(status),
            PushOutcome::Delivered,
            "2xx {status} delivers"
        );
    }
    for status in [408, 429, 500, 502, 503] {
        assert!(
            matches!(classify_push_response(status), PushOutcome::Retry(_)),
            "{status} retries (408/429 are the upstream exemptions)"
        );
    }
}

// ---------------------------------------------------------------------------
// #16c `Unsubscribe-URL` plumbing: the header is sent when the request sets
// it (the job layer leaves it unset until the token source lands).
// ---------------------------------------------------------------------------

fn vapid_keys() -> VapidKeys {
    use base64::Engine as _;
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .expect("generate");
    let der = doc.as_ref();
    let priv_off = der.windows(2).position(|w| *w == [0x04, 0x20]).expect("priv");
    let pub_off = der.windows(4).position(|w| *w == [0x03, 0x42, 0x00, 0x04]).expect("pub");
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    VapidKeys {
        public_b64: enc.encode(&der[pub_off + 3..pub_off + 3 + 65]),
        private_b64: enc.encode(&der[priv_off + 2..priv_off + 34]),
    }
}

fn test_subscription(endpoint: &str) -> birchtree::domain::WebPushSubscription {
    birchtree::domain::WebPushSubscription {
        id: Id::new(1),
        token_id: Id::new(2),
        user_id: Id::new(3),
        endpoint: endpoint.to_string(),
        key_p256dh: String::new(),
        key_auth: String::new(),
        standard: false,
        policy: "all".to_string(),
        alerts: Default::default(),
        unsubscribe_token: String::new(),
        unsubscribe_url: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    }
}

#[tokio::test]
async fn push_unsubscribe_url_header_sent_when_set() {
    // A loopback push endpoint that captures the `Unsubscribe-URL` header.
    let seen: std::sync::Arc<std::sync::Mutex<Option<Option<String>>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen_clone = seen.clone();
    let app = axum::Router::new().route(
        "/push",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let seen_clone = seen_clone.clone();
            async move {
                let value = headers
                    .get("unsubscribe-url")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                *seen_clone.lock().unwrap() = Some(value);
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });

    let client = reqwest::Client::new();
    let keys = vapid_keys();
    let endpoint = format!("http://{addr}/push");

    // Unset: no header goes out, the request still succeeds.
    let mut request = build_push_request(&test_subscription(&endpoint), &keys, "mailto:a@b.c", 1_700_000_000_000)
        .expect("request");
    assert!(request.unsubscribe_url.is_none());
    assert_eq!(deliver_push(&client, &request).await, PushOutcome::Delivered);
    assert_eq!(*seen.lock().unwrap(), Some(None));

    // Set: the header goes out verbatim.
    request.unsubscribe_url = Some("https://example.social/api/web/push_subscriptions/token123".into());
    assert_eq!(deliver_push(&client, &request).await, PushOutcome::Delivered);
    assert_eq!(
        *seen.lock().unwrap(),
        Some(Some("https://example.social/api/web/push_subscriptions/token123".to_string()))
    );
}

// ---------------------------------------------------------------------------
// #20d audience exact-match.
// Upstream `StatusParser#visibility` (`8ff11c2b`): `audience_to.include?(
// @options[:followers_collection])`.
// ---------------------------------------------------------------------------

fn url(s: &str) -> url::Url {
    s.parse().expect("url")
}

#[test]
fn audience_matches_only_the_authors_own_followers_collection() {
    let followers = "https://example.social/users/alice/followers";
    let evil = "https://evil.example/users/alice/followers";
    let to = vec![url(followers)];
    assert_eq!(
        audience_visibility(&to, &[], followers),
        Visibility::Private,
        "own followers URI is followers-only"
    );
    // A foreign host's `…/followers` URI must not pass for this author.
    assert_eq!(
        audience_visibility(&[url(evil)], &[], followers),
        Visibility::Direct,
        "another host's followers URI is not this author's collection"
    );
    // A different local user's collection is not this author's either.
    assert_eq!(
        audience_visibility(
            &[url("https://example.social/users/bob/followers")],
            &[],
            followers
        ),
        Visibility::Direct
    );
    // Public in `to`, unlisted in `cc` keep their precedence.
    assert_eq!(
        audience_visibility(&[url(PUBLIC)], &[], followers),
        Visibility::Public
    );
    assert_eq!(
        audience_visibility(&[], &[url(PUBLIC)], followers),
        Visibility::Unlisted
    );
}

fn test_note(to: Vec<url::Url>) -> Note {
    Note {
        kind: activitypub_federation::kinds::object::NoteType::Note,
        id: url("https://example.social/users/alice/statuses/1"),
        attributed_to: url("https://example.social/users/alice"),
        content: "hi".into(),
        name: None,
        summary: None,
        sensitive: None,
        published: None,
        updated: None,
        in_reply_to: None,
        to,
        cc: vec![],
        tag: vec![],
        attachment: vec![],
        quote: None,
        quote_authorization: None,
        content_map: None,
        url: None,
        replies: None,
        location: None,
    }
}

#[test]
fn note_visibility_matches_only_the_authors_own_followers_collection() {
    // Own collection → followers-only.
    let own = test_note(vec![url("https://example.social/users/alice/followers")]);
    assert_eq!(own.visibility(), Visibility::Private);
    // Foreign lookalike → direct.
    let evil = test_note(vec![url("https://evil.example/users/alice/followers")]);
    assert_eq!(evil.visibility(), Visibility::Direct);
    // Helper builds the expected URI from the actor.
    assert_eq!(
        followers_uri_of(&url("https://example.social/users/alice")),
        "https://example.social/users/alice/followers"
    );
}

// ---------------------------------------------------------------------------
// #20e sensitized forcing.
// Upstream `Activity::Create`: `sensitive: @account.sensitized? ||
// @status_parser.sensitive`.
// ---------------------------------------------------------------------------

#[test]
fn sensitized_account_forces_sensitive_on_notes_and_questions() {
    let note = test_note(vec![url(PUBLIC)]);
    assert!(!note_input_for(&note, Id::new(7), "remote.example", false).sensitive);
    assert!(note_input_for(&note, Id::new(7), "remote.example", true).sensitive);

    let sensitive_note = Note { sensitive: Some(true), ..test_note(vec![url(PUBLIC)]) };
    assert!(note_input_for(&sensitive_note, Id::new(7), "remote.example", false).sensitive);

    let question = Question {
        kind: Default::default(),
        id: url("https://remote.example/questions/1"),
        attributed_to: url("https://remote.example/users/bob"),
        content: "pick".into(),
        published: None,
        updated: None,
        url: None,
        in_reply_to: None,
        to: vec![url(PUBLIC)],
        cc: vec![],
        one_of: Some(vec![QuestionOption {
            kind: Some("Note".into()),
            name: "a".into(),
            replies: Some(QuestionReplies { kind: None, total_items: 0 }),
        }]),
        any_of: None,
        end_time: None,
        closed: None,
        voters_count: None,
    };
    let (plain, _) = question_input_for(&question, Id::new(9), false, 1_700_000_000_000);
    assert!(!plain.sensitive);
    let (forced, _) = question_input_for(&question, Id::new(9), true, 1_700_000_000_000);
    assert!(forced.sensitive);
}

// ---------------------------------------------------------------------------
// #20f BCP47 canonicalization at parse (mechanism without the locale
// catalog: agrees with upstream's downcase-keyed table on standard tags).
// ---------------------------------------------------------------------------

#[test]
fn language_tags_canonicalize_to_bcp47() {
    assert_eq!(canonical_language_tag("en"), "en");
    assert_eq!(canonical_language_tag("EN"), "en");
    assert_eq!(canonical_language_tag("pt-br"), "pt-BR");
    assert_eq!(canonical_language_tag("pt_BR"), "pt-BR");
    assert_eq!(canonical_language_tag("zh-Hans-CN"), "zh-Hans-CN");
    assert_eq!(canonical_language_tag("ZH-HANS-CN"), "zh-Hans-CN");
    assert_eq!(canonical_language_tag("es-419"), "es-419");
}

#[test]
fn note_language_uses_the_single_content_map_key() {
    let mut single = BTreeMap::new();
    single.insert("EN".to_string(), "<p>hi</p>".to_string());
    let note = Note { content_map: Some(single), ..test_note(vec![url(PUBLIC)]) };
    assert_eq!(note.language(), Some("en".to_string()));

    let mut multi = BTreeMap::new();
    multi.insert("en".to_string(), "<p>hi</p>".to_string());
    multi.insert("fr".to_string(), "<p>salut</p>".to_string());
    let note = Note { content_map: Some(multi), ..test_note(vec![url(PUBLIC)]) };
    assert_eq!(note.language(), None);
}

// ---------------------------------------------------------------------------
// #20b bounded dedup window (the D33 applied-only rule lives in
// `activities::record`; this pins the retention policy).
// ---------------------------------------------------------------------------

#[test]
fn dedup_window_is_seven_days() {
    assert_eq!(DEDUP_TTL_MS, 7 * 86_400_000);
    let now = 1_700_000_000_000u64;
    assert_eq!(prune_cutoff_ms(now), now - DEDUP_TTL_MS);
    assert_eq!(prune_cutoff_ms(1_000), 0, "saturates instead of wrapping");
    assert!(entry_live(now, now));
    assert!(entry_live(now - DEDUP_TTL_MS, now), "boundary is live");
    assert!(!entry_live(now - DEDUP_TTL_MS - 1, now), "past the window re-evaluates");
    assert!(entry_live(0, 0), "a past clock keeps everything (fail closed)");
}

// ---------------------------------------------------------------------------
// #18 report category gating (pure helper; the endpoint wires it).
// Upstream `ReportService#call` + `Report` enum (`8ff11c2b`).
// ---------------------------------------------------------------------------

#[test]
fn report_category_gating() {
    assert_eq!(report_category(None, false).unwrap(), "other");
    assert_eq!(report_category(Some(""), false).unwrap(), "other");
    assert_eq!(report_category(Some("spam"), false).unwrap(), "spam");
    assert_eq!(report_category(Some("legal"), false).unwrap(), "legal");
    assert_eq!(report_category(Some("spam"), true).unwrap(), "violation");
    assert_eq!(report_category(None, true).unwrap(), "violation");
    assert!(report_category(Some("bogus"), false).is_err());
    assert!(report_category(Some("OTHER"), false).is_err(), "enum is case-sensitive");
}

// ---------------------------------------------------------------------------
// #21f hashtag subscription normalization (same rule as the REST tag
// timeline: `normalize_tag`).
// ---------------------------------------------------------------------------

#[test]
fn hashtag_subscribe_normalizes_like_the_rest_timeline() {
    use birchtree::streaming::channels::resolve;
    let kind = resolve(
        Channel::Hashtag { local: false },
        &Params { tag: Some("#RuSt".to_string()), list: None },
    )
    .expect("resolve");
    assert_eq!(
        kind,
        StreamKind::Hashtag { tag: "rust".to_string(), local: false }
    );
}

// --- HTTP-level evidence ---------------------------------------------------

fn enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn user_token(srv: &TestServer, username: &str) -> String {
    let app_body = format!(
        "client_name=P2c&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc("read write follow profile read:statuses write:statuses write:reports read:accounts"),
    );
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/apps", &app_body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app = json(&bytes);
    let token_body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc("read write follow profile read:statuses write:statuses write:reports read:accounts"),
    );
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/oauth/token", &token_body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app_tok = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let reg_body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/accounts", &reg_body, Some(&app_tok))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn post_status(srv: &TestServer, token: &str, text: &str) -> String {
    let body = format!("status={}&visibility=public", enc(text));
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/statuses", &body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().to_string()
}

fn tag_ids(body: &serde_json::Value) -> Vec<String> {
    body.as_array()
        .expect("array")
        .iter()
        .filter_map(|s| s["id"].as_str().map(str::to_string))
        .collect()
}

// #21a tag any/all/none over HTTP.
#[tokio::test]
async fn tag_any_all_none() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "tagger").await;
    let rust_only = post_status(&srv, &token, "hello #rust").await;
    let both = post_status(&srv, &token, "hello #rust #async").await;
    let _other = post_status(&srv, &token, "hello #async").await;

    // Base tag only.
    let (status, _, bytes) = send(&srv.router, get_request("/api/v1/timelines/tag/rust", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    let ids = tag_ids(&body);
    assert!(ids.contains(&rust_only) && ids.contains(&both), "{body}");

    // `any` widens: #async statuses join.
    let (status, _, bytes) =
        send(&srv.router, get_request("/api/v1/timelines/tag/rust?any[]=async", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(tag_ids(&body).len(), 3, "{body}");

    // `all` narrows: only the status carrying both.
    let (status, _, bytes) =
        send(&srv.router, get_request("/api/v1/timelines/tag/rust?all[]=async", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(tag_ids(&body), vec![both], "{body}");

    // `none` excludes: the both-tagged status drops out.
    let (status, _, bytes) =
        send(&srv.router, get_request("/api/v1/timelines/tag/rust?none[]=async", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(tag_ids(&body), vec![rust_only], "{body}");
}

// #18 reports over HTTP.
#[tokio::test]
async fn reports_category_forward_rules() {
    use birchtree::application::ports::{AccountStore, AdminInfraStore};
    let srv = TestServer::new(true);
    let reporter = user_token(&srv, "reporter").await;
    let _target = user_token(&srv, "reported").await;
    let target_id = srv
        .store
        .account_by_handle("reported", "example.social")
        .expect("store")
        .expect("target")
        .id
        .to_string();
    let rule = srv.store.admin_create_rule("no spam", "", 1_700_000_000_000).expect("rule");

    // `category=spam` renders back.
    let body = format!("account_id={target_id}&comment=spammy&category=spam");
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/reports", &body, Some(&reporter))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let created = json(&bytes);
    assert_eq!(created["category"], "spam");
    assert_eq!(created["forwarded"], false, "local target never forwards");

    // Unknown category 422s.
    let body = format!("account_id={target_id}&comment=x&category=bogus");
    let (status, _h, _bytes) = send(&srv.router, form_request("POST", "/api/v1/reports", &body, Some(&reporter))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown rule ids 422 (upstream `validate_rule_ids`).
    let body = format!("account_id={target_id}&comment=x&rule_ids[]=999999999");
    let (status, _h, _bytes) = send(&srv.router, form_request("POST", "/api/v1/reports", &body, Some(&reporter))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Real rule ids force `violation` and render back.
    let body = format!("account_id={target_id}&comment=x&rule_ids[]={}", rule.id.value());
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/reports", &body, Some(&reporter))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let created = json(&bytes);
    assert_eq!(created["category"], "violation");
    assert_eq!(created["rule_ids"], serde_json::json!([rule.id.value().to_string()]));
}

// #19 collection pages over HTTP.
#[tokio::test]
async fn federation_collections_page() {
    use birchtree::application::ports::AccountStore;
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let bob = user_token(&srv, "bob").await;
    let alice_id = srv
        .store
        .account_by_handle("alice", "example.social")
        .expect("store")
        .expect("alice")
        .id
        .to_string();
    // Bob follows Alice: her followers collection gains a member.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{alice_id}/follow"), "", Some(&bob)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    // Base followers document: counts + first page link.
    let (status, _, bytes) =
        send(&srv.router, get_request("/users/alice/followers", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["type"], "OrderedCollection");
    assert!(body["totalItems"].as_u64().unwrap() >= 1, "{body}");
    assert_eq!(body["first"], "https://example.social/users/alice/followers?page=1");

    // Page 1: an OrderedCollectionPage of actor URIs.
    let (status, _, bytes) =
        send(&srv.router, get_request("/users/alice/followers?page=1", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["type"], "OrderedCollectionPage");
    assert_eq!(body["partOf"], "https://example.social/users/alice/followers");
    let items = body["orderedItems"].as_array().expect("items");
    assert!(items.iter().any(|u| u.as_str().unwrap().contains("/users/bob")), "{body}");

    // Following mirrors it.
    let (status, _, bytes) =
        send(&srv.router, get_request("/users/bob/following?page=1", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["type"], "OrderedCollectionPage");
    let items = body["orderedItems"].as_array().expect("items");
    assert!(items.iter().any(|u| u.as_str().unwrap().contains("/users/alice")), "{body}");

    // Outbox base: real count + first/last links.
    let _sid = post_status(&srv, &alice, "public post").await;
    let (status, _, bytes) = send(&srv.router, get_request("/users/alice/outbox", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["type"], "OrderedCollection");
    assert!(body["totalItems"].as_u64().unwrap() >= 1, "{body}");
    assert_eq!(body["first"], "https://example.social/users/alice/outbox?page=true");

    // Outbox page: Create activities with prev cursors.
    let (status, _, bytes) =
        send(&srv.router, get_request("/users/alice/outbox?page=true", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["type"], "OrderedCollectionPage");
    let items = body["orderedItems"].as_array().expect("items");
    assert!(!items.is_empty(), "{body}");
    assert_eq!(items[0]["type"], "Create", "{body}");
}

// #20c webfinger suspended → 410.
#[tokio::test]
async fn webfinger_suspended_is_gone() {
    use birchtree::application::ports::{AccountStore, AdminStore};
    use birchtree::application::ports::AdminAccountPatch;
    let srv = TestServer::new(true);
    let token = user_token(&srv, "doomed").await;
    let account = srv
        .store
        .account_by_handle("doomed", "example.social")
        .expect("store")
        .expect("account");
    // Alive first.
    let (status, _, _) = send(
        &srv.router,
        get_request(
            "/.well-known/webfinger?resource=acct:doomed@example.social",
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Suspend, then the same URI is 410.
    srv.store
        .admin_update_account(
            account.id,
            account.id,
            AdminAccountPatch { suspended: Some(true), audit_action: "suspend".into(), ..Default::default() },
            1_700_000_000_001,
        )
        .expect("suspend");
    let (status, _, _) = send(
        &srv.router,
        get_request(
            "/.well-known/webfinger?resource=acct:doomed@example.social",
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    let _ = token;
}

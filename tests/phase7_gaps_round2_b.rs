//! Gap-closing round 2, subagent B: streaming + session policy (D15, D20, D21, D24).
//!
//! - D15: a block/mute retracts ALL of the target's home+list stream
//!   statuses (upstream `FeedManager#clear_from_home`/
//!   `clear_from_list`), read in bounded `max_id` batches, not capped at 200.
//! - D20 (planned gap): `notifications_merged` is never emitted until the
//!   notification-request filter pipeline exists — locked by test.
//! - D21: `chosen_languages` user setting, applied to the REST public
//!   timeline and the public + hashtag streams like upstream; the REST tag
//!   timeline, home and list feeds keep no such filter (verified against
//!   `PublicFeed`/`TagFeed` at v4.7.3).
//! - D24: suspension/disablement revokes the account's tokens; open streams
//!   for killed tokens close and reuse answers 401.

mod common;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::{TEST_PUBLIC_URL, T0, TestServer, form_request, send};
use serde_json::Value;

use birchtree::application::auth;
use birchtree::application::ports::AccountStore;
use birchtree::application::relationships::FollowCommand;
use birchtree::application::statuses::{CreateStatusCommand, create_status};
use birchtree::application::{policy, relationships, timelines};
use birchtree::domain::Id;
use birchtree::streaming::Limits;

const SCOPES: &str = "read write follow read:statuses write:statuses read:notifications \
    read:lists write:lists read:accounts write:accounts read:blocks write:blocks \
    read:mutes write:mutes read:filters";
const ADMIN_SCOPES: &str = "read write follow admin:read admin:write \
    admin:read:accounts admin:write:accounts admin:read:reports admin:write:reports";

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(500);

async fn serve(srv: &TestServer) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let router = srv.router.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    addr
}

fn token(srv: &TestServer, username: &str, scopes: &str) -> String {
    let account = match srv
        .store
        .account_by_handle(username, "example.social")
        .expect("store")
    {
        Some(account) => account,
        None => common::mk_account(&srv.store, username),
    };
    let app = auth::register_app(
        &*srv.store,
        format!("GapsB {username}"),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .expect("register app");
    let scopes: Vec<String> = scopes.split_whitespace().map(str::to_string).collect();
    let (plaintext, _) =
        auth::issue_user_token(&*srv.store, app.app.id, account.id, scopes, T0).expect("token");
    plaintext
}

fn account_id(srv: &TestServer, username: &str) -> Id {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
}

fn encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", enc(key), enc(value)))
        .collect::<Vec<_>>()
        .join("&")
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

async fn call(srv: &TestServer, method: &str, path: &str, token: &str, body: &str) -> (StatusCode, Value) {
    let (status, _h, bytes) = send(&srv.router, form_request(method, path, body, Some(token))).await;
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn ok(srv: &TestServer, method: &str, path: &str, token: &str, body: &str) -> Value {
    let (status, value) = call(srv, method, path, token, body).await;
    assert_eq!(status, StatusCode::OK, "{method} {path}: {value}");
    value
}

fn drain_jobs(srv: &TestServer) {
    use birchtree::application::ports::JobQueue;
    let mail = birchtree::mail::LoggingMailSender;
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as u64
            + 3_600_000;
        let jobs = srv.store.claim_due(now, 64).expect("claim");
        if jobs.is_empty() {
            break;
        }
        for job in jobs {
            birchtree::jobs::handlers::handle(&job, &*srv.store, &mail, &*srv.blobs, &srv.streams)
                .expect("handle job");
            srv.store.complete(job.id).expect("complete");
        }
    }
}

// ---------------------------------------------------------------------------
// SSE client (minimal; mirrors tests/phase5_streaming.rs).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Block {
    Comment(String),
    Event { name: String, data: String },
}

struct Sse {
    response: reqwest::Response,
    buffer: String,
    pending: std::collections::VecDeque<Block>,
    name: Option<String>,
    data: Option<String>,
}

impl Sse {
    async fn next_block(&mut self, wait: Duration) -> Option<Block> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(block) = self.pending.pop_front() {
                return Some(block);
            }
            while let Some(end) = self.buffer.find('\n') {
                let line: String = self.buffer.drain(..=end).collect();
                let line = line.trim_end_matches('\n').trim_end_matches('\r');
                if let Some(comment) = line.strip_prefix(':') {
                    self.pending.push_back(Block::Comment(comment.to_string()));
                } else if let Some(value) = line.strip_prefix("event: ") {
                    self.name = Some(value.to_string());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    self.data = Some(value.to_string());
                } else if line.is_empty() {
                    if let (Some(name), Some(data)) = (self.name.take(), self.data.take()) {
                        self.pending.push_back(Block::Event { name, data });
                    }
                }
            }
            if let Some(block) = self.pending.pop_front() {
                return Some(block);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let chunk = tokio::time::timeout(left, self.response.chunk())
                .await
                .ok()?
                .ok()??;
            if chunk.is_empty() {
                return None;
            }
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    async fn collect(&mut self, wait: Duration) -> Vec<Block> {
        let deadline = Instant::now() + wait;
        let mut blocks = Vec::new();
        while let Some(block) = self.next_block(deadline.saturating_duration_since(Instant::now())).await {
            blocks.push(block);
        }
        blocks
    }
}

async fn open_sse(addr: SocketAddr, path: &str, token: Option<&str>) -> Sse {
    let mut request = reqwest::Client::new().get(format!("http://{addr}{path}"));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = request.send().await.expect("sse request");
    assert_eq!(response.status(), StatusCode::OK, "sse {path}");
    Sse {
        response,
        buffer: String::new(),
        pending: std::collections::VecDeque::new(),
        name: None,
        data: None,
    }
}

fn ids_of(blocks: &[Block], name: &str) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Event { name: event, data } if event == name => {
                Some(serde_json::from_str::<Value>(data).ok()?["id"].as_str()?.to_string())
            }
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// D15: full retraction, batched.
// ---------------------------------------------------------------------------

/// Post one public status straight through the application use case (fast
/// bulk setup; the streaming `update` publish is the adapter's job and is
/// not needed here).
fn post_direct(srv: &TestServer, author: Id, text: &str, language: Option<&str>, now_ms: u64) -> String {
    create_status(
        &*srv.store,
        TEST_PUBLIC_URL,
        now_ms,
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
        "off",
        "off",
        None,
        "off",
    )
    .expect("create status")
    .status
    .id
    .to_string()
}

#[test]
fn d15_retraction_candidates_cover_more_than_200() {
    let srv = TestServer::new(false);
    let viewer = common::mk_account(&srv.store, "d15_viewer").id;
    let target = common::mk_account(&srv.store, "d15_target").id;
    relationships::follow(
        &*srv.store,
        T0,
        FollowCommand {
            follower_id: viewer,
            target_id: target,
            show_reblogs: None,
            notify: None,
            languages: None,
        },
    )
    .expect("follow");
    // 600 target statuses (past the old 200 cap AND past one 500-match
    // batch, so the cursor loop is exercised) plus viewer statuses that
    // mention the target (the mention arm of the rule).
    let mut expected: Vec<String> = Vec::new();
    for i in 0..600 {
        expected.push(post_direct(
            &srv,
            target,
            &format!("d15 target post {i}"),
            None,
            T0 + i as u64 + 1,
        ));
    }
    for i in 0..5 {
        expected.push(post_direct(
            &srv,
            viewer,
            &format!("d15 viewer post mentioning @d15_target {i}"),
            None,
            T0 + 10_000 + i as u64,
        ));
    }
    let started = Instant::now();
    let (found, stats) =
        timelines::retraction_candidates_with_stats(&*srv.store, viewer, target).expect("retract");
    let elapsed = started.elapsed();
    eprintln!(
        "D15 retraction: retracted={} batches={} truncated={} elapsed_ms={}",
        stats.retracted,
        stats.batches,
        stats.truncated,
        elapsed.as_millis()
    );
    assert_eq!(found.len(), expected.len(), "every retractable status is found");
    assert!(!stats.truncated, "the safety bound must not truncate: {stats:?}");
    assert!(stats.batches >= 2, "the scan pages past one batch: {stats:?}");
    let mut got: Vec<String> = found.iter().map(|s| s.id.to_string()).collect();
    got.sort();
    let mut want = expected.clone();
    want.sort();
    assert_eq!(got, want);
    // Empty page still terminates the scan: a second call finds the same set.
    let (again, _) =
        timelines::retraction_candidates_with_stats(&*srv.store, viewer, target).expect("retract");
    assert_eq!(again.len(), expected.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d15_block_deletes_all_260_from_the_open_home_stream() {
    let srv = TestServer::with_limits(Limits {
        queue_frames: 4096,
        ..Default::default()
    });
    let viewer_tok = token(&srv, "d15s_viewer", SCOPES);
    let target_tok = token(&srv, "d15s_target", SCOPES);
    let target = account_id(&srv, "d15s_target");
    ok(
        &srv,
        "POST",
        &format!("/api/v1/accounts/{target}/follow"),
        &viewer_tok,
        "",
    )
    .await;
    let mut expected: Vec<String> = Vec::new();
    for i in 0..260 {
        expected.push(post_direct(&srv, target, &format!("d15s post {i}"), None, T0 + i as u64 + 1));
    }
    let _ = target_tok;
    let addr = serve(&srv).await;
    let mut home = open_sse(addr, "/api/v1/streaming/user", Some(&viewer_tok)).await;
    // Drain the opens (`:)` only); nothing was published for the direct posts.
    let _ = home.collect(QUIET).await;
    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/accounts/{target}/block"),
        &viewer_tok,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let deadline = Instant::now() + WAIT;
    let mut deletes: Vec<String> = Vec::new();
    while deletes.len() < expected.len() {
        let left = deadline.checked_duration_since(Instant::now()).expect("retraction in time");
        match home.next_block(left).await {
            Some(Block::Event { name, data }) if name == "delete" => deletes.push(data),
            Some(_) => continue,
            None => break,
        }
    }
    let mut deletes_sorted = deletes.clone();
    deletes_sorted.sort();
    let mut want = expected.clone();
    want.sort();
    assert_eq!(deletes_sorted, want, "all 260 retracted statuses arrive as deletes");
}

// ---------------------------------------------------------------------------
// D20 (planned gap): no `notifications_merged` without the pipeline.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d20_notifications_merged_is_never_sent() {
    let srv = TestServer::new(false);
    let addr = serve(&srv).await;
    let alice = token(&srv, "d20_alice", SCOPES);
    let bob = token(&srv, "d20_bob", SCOPES);
    let mut bob_home = open_sse(addr, "/api/v1/streaming/user", Some(&bob)).await;
    let mut bob_notes =
        open_sse(addr, "/api/v1/streaming/user/notification", Some(&bob)).await;
    // A mention creates a real notification through the fan-out job.
    let body = encode(&[("status", "hi @d20_bob"), ("visibility", "public")]);
    let (status, value) = call(&srv, "POST", "/api/v1/statuses", &alice, &body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    drain_jobs(&srv);
    let mut saw_notification = false;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match bob_notes.next_block(left).await {
            Some(Block::Event { name, data }) => {
                assert_ne!(name, "notifications_merged", "no merge signal without the request pipeline");
                if name == "notification" {
                    let payload: Value = serde_json::from_str(&data).expect("notification json");
                    assert_eq!(payload["type"], "mention");
                    saw_notification = true;
                    break;
                }
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(saw_notification, "the mention notification arrives");
    // And nothing merged arrives on either stream afterwards.
    for blocks in [
        bob_home.collect(QUIET).await,
        bob_notes.collect(QUIET).await,
    ] {
        assert!(
            blocks.iter().all(|b| !matches!(b, Block::Event { name, .. } if name == "notifications_merged")),
            "no notifications_merged anywhere: {blocks:?}"
        );
    }
    // The requests surface stays honest: empty, and merges are vacuous.
    let (status, body) = call(&srv, "GET", "/api/v1/notifications/requests", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, Value::Array(Vec::new()));
    let (status, body) = call(&srv, "GET", "/api/v1/notifications/requests/merged", &bob, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["merged"], Value::Bool(true));
}

// ---------------------------------------------------------------------------
// D21: chosen_languages.
// ---------------------------------------------------------------------------

#[test]
fn d21_policy_helpers_match_upstream() {
    // No preference: everything passes, including language-less.
    assert!(policy::chosen_language_allows(None, None));
    assert!(policy::chosen_language_allows(None, Some("en")));
    assert!(policy::chosen_language_allows(Some(&[]), Some("fr")));
    // Set: exact match only; language-less never passes (upstream SQL NULL
    // semantics and streaming indexOf(null) === -1).
    let chosen = vec!["en".to_string(), "pt-BR".to_string()];
    assert!(policy::chosen_language_allows(Some(&chosen), Some("en")));
    assert!(policy::chosen_language_allows(Some(&chosen), Some("pt-BR")));
    assert!(!policy::chosen_language_allows(Some(&chosen), Some("EN")));
    assert!(!policy::chosen_language_allows(Some(&chosen), Some("fr")));
    assert!(!policy::chosen_language_allows(Some(&chosen), None));
    // Normalization mirrors upstream compact_blank.presence (no case folding).
    assert_eq!(
        policy::normalize_chosen_languages(
            &["en".to_string(), " ".to_string(), "en".to_string(), "fr".to_string()]
        ),
        vec!["en".to_string(), "fr".to_string()]
    );
    assert!(policy::normalize_chosen_languages(&["  ".to_string()]).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d21_chosen_languages_filter_public_but_not_tag() {
    let srv = TestServer::new(false);
    let alice = token(&srv, "d21_alice", SCOPES);
    let bob = token(&srv, "d21_bob", SCOPES);
    let post_lang = |text: &str, lang: Option<&str>| {
        let mut pairs = vec![("status", text), ("visibility", "public")];
        if let Some(lang) = lang {
            pairs.push(("language", lang));
        }
        encode(&pairs)
    };
    let en = ok(&srv, "POST", "/api/v1/statuses", &alice, &post_lang("hello world", Some("en"))).await;
    let fr = ok(&srv, "POST", "/api/v1/statuses", &alice, &post_lang("bonjour monde", Some("fr"))).await;
    let un = ok(&srv, "POST", "/api/v1/statuses", &alice, &post_lang("no language here", None)).await;
    let en_id = en["id"].as_str().unwrap().to_string();
    let fr_id = fr["id"].as_str().unwrap().to_string();
    let un_id = un["id"].as_str().unwrap().to_string();
    // Tagged pair for the tag-timeline check.
    let ten = ok(
        &srv,
        "POST",
        "/api/v1/statuses",
        &alice,
        &post_lang("#babel hello", Some("en")),
    )
    .await;
    let tfr = ok(
        &srv,
        "POST",
        "/api/v1/statuses",
        &alice,
        &post_lang("#babel bonjour", Some("fr")),
    )
    .await;
    let ten_id = ten["id"].as_str().unwrap().to_string();
    let tfr_id = tfr["id"].as_str().unwrap().to_string();

    // Without a preference bob sees everything.
    let (_, page) = call(&srv, "GET", "/api/v1/timelines/public?limit=40", &bob, "").await;
    let page_ids: Vec<String> = page
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert!(page_ids.contains(&en_id) && page_ids.contains(&fr_id) && page_ids.contains(&un_id));

    // Bob sets chosen languages to English only.
    ok(
        &srv,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        &bob,
        "source[chosen_languages][]=en",
    )
    .await;
    let (_, page) = call(&srv, "GET", "/api/v1/timelines/public?limit=40", &bob, "").await;
    let page_ids: Vec<String> = page
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert!(page_ids.contains(&en_id), "{page_ids:?}");
    assert!(!page_ids.contains(&fr_id), "french is filtered: {page_ids:?}");
    assert!(!page_ids.contains(&un_id), "language-less is filtered like upstream: {page_ids:?}");

    // The REST tag timeline keeps NO chosen-language filter (upstream
    // TagFeed has no language scope): both tagged statuses stay.
    let (_, tag) = call(&srv, "GET", "/api/v1/timelines/tag/babel?limit=40", &bob, "").await;
    let tag_ids: Vec<String> = tag
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert!(tag_ids.contains(&ten_id) && tag_ids.contains(&tfr_id), "{tag_ids:?}");

    // Streams: public and hashtag both filter; the french tag stays out of
    // the hashtag stream even though the REST tag timeline keeps it.
    let addr = serve(&srv).await;
    let mut public = open_sse(addr, "/api/v1/streaming/public", Some(&bob)).await;
    let mut hashtag = open_sse(addr, "/api/v1/streaming/hashtag?tag=babel", Some(&bob)).await;
    let _ = public.collect(QUIET).await;
    let _ = hashtag.collect(QUIET).await;
    ok(&srv, "POST", "/api/v1/statuses", &alice, &post_lang("second hello", Some("en"))).await;
    ok(&srv, "POST", "/api/v1/statuses", &alice, &post_lang("second bonjour", Some("fr"))).await;
    ok(
        &srv,
        "POST",
        "/api/v1/statuses",
        &alice,
        &post_lang("#babel third hello", Some("en")),
    )
    .await;
    ok(
        &srv,
        "POST",
        "/api/v1/statuses",
        &alice,
        &post_lang("#babel troisieme bonjour", Some("fr")),
    )
    .await;
    let public_updates = ids_of(&public.collect(Duration::from_secs(3)).await, "update");
    assert_eq!(public_updates.len(), 2, "only the english posts: {public_updates:?}");
    let tag_updates = ids_of(&hashtag.collect(Duration::from_secs(3)).await, "update");
    assert_eq!(tag_updates.len(), 1, "only the english tag: {tag_updates:?}");

    // Clearing the preference (empty list) restores the unfiltered feed.
    ok(
        &srv,
        "PATCH",
        "/api/v1/accounts/update_credentials",
        &bob,
        "source[chosen_languages][]=",
    )
    .await;
    let (_, page) = call(&srv, "GET", "/api/v1/timelines/public?limit=40", &bob, "").await;
    let page_ids: Vec<String> = page
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert!(page_ids.contains(&fr_id) && page_ids.contains(&un_id), "{page_ids:?}");
}

// ---------------------------------------------------------------------------
// D24: suspension/disablement kills tokens and closes streams.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d24_suspend_kills_tokens_and_closes_streams() {
    use birchtree::domain::UserRole;
    let srv = TestServer::new(false);
    let boss = token(&srv, "d24_boss", ADMIN_SCOPES);
    let victim = token(&srv, "d24_victim", SCOPES);
    srv.store
        .set_user_role(account_id(&srv, "d24_boss"), UserRole::Admin)
        .expect("role");
    let victim_id = account_id(&srv, "d24_victim").to_string();
    let addr = serve(&srv).await;
    let mut stream = open_sse(addr, "/api/v1/streaming/user", Some(&victim)).await;

    // Suspend through the admin action path.
    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/admin/accounts/{victim_id}/action"),
        &boss,
        "type=suspend",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");

    // The open socket closes (upstream `kill`).
    let started = Instant::now();
    let ended = loop {
        match stream.next_block(WAIT).await {
            Some(Block::Comment(_)) => continue,
            Some(event) => break Some(event),
            None => break None,
        }
    };
    assert!(ended.is_none(), "the victim's stream closes on suspend: {ended:?}");
    assert!(started.elapsed() < WAIT, "the stream closed, rather than timing out");

    // The killed token is refused everywhere with 401.
    let (status, body) = call(&srv, "GET", "/api/v1/accounts/verify_credentials", &victim, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let mut request =
        reqwest::Client::new().get(format!("http://{addr}/api/v1/streaming/user"));
    request = request.header("authorization", format!("Bearer {victim}"));
    let response = request.send().await.expect("sse request");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Unsuspension does not resurrect the token (revocation is permanent).
    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/admin/accounts/{victim_id}/unsuspend"),
        &boss,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (status, _) =
        call(&srv, "GET", "/api/v1/accounts/verify_credentials", &victim, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "unsuspend keeps the kill");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d24_disable_kills_tokens() {
    use birchtree::domain::UserRole;
    let srv = TestServer::new(false);
    let boss = token(&srv, "d24d_boss", ADMIN_SCOPES);
    let victim = token(&srv, "d24d_victim", SCOPES);
    srv.store
        .set_user_role(account_id(&srv, "d24d_boss"), UserRole::Admin)
        .expect("role");
    let victim_id = account_id(&srv, "d24d_victim").to_string();
    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/admin/accounts/{victim_id}/action"),
        &boss,
        "type=disable",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (status, body) =
        call(&srv, "GET", "/api/v1/accounts/verify_credentials", &victim, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    // A silence (non-access-removing action) leaves other tokens alone.
    let other = token(&srv, "d24d_other", SCOPES);
    let other_id = account_id(&srv, "d24d_other").to_string();
    let (status, value) = call(
        &srv,
        "POST",
        &format!("/api/v1/admin/accounts/{other_id}/action"),
        &boss,
        "type=silence",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (status, _) =
        call(&srv, "GET", "/api/v1/accounts/verify_credentials", &other, "").await;
    assert_eq!(status, StatusCode::OK);
}

#[test]
fn d24_kill_revokes_every_token_of_the_account() {
    use birchtree::application::admin::kill_account_tokens;
    use birchtree::application::ports::{OAuthStore, StreamingSink};
    struct NullSink;
    impl StreamingSink for NullSink {
        fn publish(&self, _event: birchtree::application::streaming::StreamEvent) {}
    }
    let srv = TestServer::new(false);
    let first = token(&srv, "d24k_victim", SCOPES);
    let second = token(&srv, "d24k_victim", SCOPES);
    assert_ne!(first, second);
    let killed = kill_account_tokens(&*srv.store, &NullSink, account_id(&srv, "d24k_victim"))
        .expect("kill");
    assert_eq!(killed, 2, "both tokens die in one call");
    let store: &dyn OAuthStore = &*srv.store;
    assert!(store.token_by_plaintext(&first).expect("lookup").unwrap().revoked);
    assert!(store.token_by_plaintext(&second).expect("lookup").unwrap().revoked);
    // Idempotent: a second kill revokes nothing new.
    let killed = kill_account_tokens(&*srv.store, &NullSink, account_id(&srv, "d24k_victim"))
        .expect("kill");
    assert_eq!(killed, 0);
}

/// D15 backlog measurement: 10k retractable statuses. `#[ignore]`d so the
/// normal suite stays fast; run explicitly for the batching numbers. This is
/// a measurement harness, not a boundary assertion: it reports batches and
/// wall time for the round-2 summary.
#[test]
#[ignore]
fn d15_retraction_10k_measurement() {
    let srv = TestServer::new(false);
    let viewer = common::mk_account(&srv.store, "d15m_viewer").id;
    let target = common::mk_account(&srv.store, "d15m_target").id;
    relationships::follow(
        &*srv.store,
        T0,
        FollowCommand {
            follower_id: viewer,
            target_id: target,
            show_reblogs: None,
            notify: None,
            languages: None,
        },
    )
    .expect("follow");
    let setup = Instant::now();
    for i in 0..10_000 {
        post_direct(&srv, target, &format!("d15m post {i}"), None, T0 + i as u64 + 1);
    }
    eprintln!("D15 setup 10k posts: {} ms", setup.elapsed().as_millis());
    let started = Instant::now();
    let (found, stats) =
        timelines::retraction_candidates_with_stats(&*srv.store, viewer, target).expect("retract");
    eprintln!(
        "D15 retraction 10k: retracted={} batches={} truncated={} elapsed_ms={}",
        stats.retracted,
        stats.batches,
        stats.truncated,
        started.elapsed().as_millis()
    );
    assert_eq!(found.len(), 10_000);
    assert!(!stats.truncated);
}

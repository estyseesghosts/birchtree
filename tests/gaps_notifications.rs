//! Gaps batch, G-B: notification + streaming completeness (D51/D52).
//!
//! - Real D51 producers: every `emit_*` builder in
//!   `application::notifications` builds `FanOutNotification` payloads that
//!   flow through `fan_out_from_payload` into stored rows, served by
//!   `index` with `types[]`/`exclude_types[]` semantics. Types whose trigger
//!   feature does not exist yet (`severed_relationships`,
//!   `added_to_collection`, `collection_update`) pin the honest empty
//!   outcome: builders return `[]` for empty inputs and no stored rows name
//!   those types.
//! - `include_filtered`: filtered rows stay out of the default index and are
//!   served through `merge_filtered_index` (numeric-id ordering pinned).
//! - `supported_types`: baseline/fallback rules + fallback object shape.
//! - Pre-token push backfill: send-or-mark (`Unsubscribe-URL` once a token
//!   exists, never fabricated).
//! - Streaming: `announcement`, `announcement.reaction`,
//!   `announcement.delete` captured on real sockets through the real hub.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::announcements as ann_uc;
use birchtree::application::announcements::AnnouncementStore;
use birchtree::application::notifications as notif;
use birchtree::application::ports::AccountStore;
use birchtree::domain::{Id, Notification, NotificationType};
use birchtree::streaming::fanout;

use common::{T0, TestServer, form_request, json, send};

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(500);

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
        "client_name=GapsB&redirect_uris={}&scopes={}",
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

async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(srv, "read:accounts write:accounts follow read write").await;
    register_account(srv, &app, username).await
}

fn account_id(srv: &TestServer, username: &str) -> Id {
    srv.store
        .account_by_handle(username, "example.social")
        .expect("store")
        .expect("account")
        .id
}

/// Run every `emit_*` job through the real fan-out handler; returns the ids
/// of newly created notifications.
fn run_pending(srv: &TestServer, jobs: Vec<notif::PendingFanOut>) -> Vec<Id> {
    let store: &dyn birchtree::application::ports::Store = &*srv.store;
    let mut created = Vec::new();
    for job in jobs {
        assert!(
            matches!(
                job.kind,
                birchtree::domain::JobKind::FanOutNotification
            ),
            "producers enqueue FanOutNotification jobs only"
        );
        if let Some(id) = notif::fan_out_from_payload(store, &job.payload).expect("fan out") {
            created.push(id);
        }
    }
    created
}

fn index_all(srv: &TestServer, recipient: Id) -> Vec<Notification> {
    let store: &dyn birchtree::application::ports::Store = &*srv.store;
    notif::index(
        store,
        recipient,
        notif::NotificationQuery {
            max_id: None,
            since_id: None,
            min_id: None,
            limit: 40,
            from_account: None,
            include_filtered: false,
        },
    )
    .expect("index")
}

// ---------------------------------------------------------------------------
// D51: wired producers flow end to end.
// ---------------------------------------------------------------------------

#[test]
fn d51_status_producer_reaches_notify_followers() {
    let srv = TestServer::new(false);
    let alice = common::mk_account(&srv.store, "gb_alice");
    let bob = common::mk_account(&srv.store, "gb_bob");
    let status_id = Id::new(910_001);

    // Empty input: no fabricated rows.
    assert!(notif::emit_status_for_followers(alice.id, status_id, &[], T0).is_empty());

    let created = run_pending(
        &srv,
        notif::emit_status_for_followers(alice.id, status_id, &[bob.id], T0),
    );
    assert_eq!(created.len(), 1);

    let mut rows = index_all(&srv, bob.id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].r#type, NotificationType::Status);
    assert_eq!(rows[0].actor_id, Some(alice.id));
    assert_eq!(rows[0].status_id, Some(status_id));

    // `types[]` narrows to it; `exclude_types[]` removes it (endpoint-side
    // `retain` semantics, pinned here at the use-case level).
    notif::retain_types(&mut rows, &[NotificationType::Status]);
    assert_eq!(rows.len(), 1);
    let mut rows = index_all(&srv, bob.id);
    rows.retain(|n| n.r#type != NotificationType::Status);
    assert!(rows.is_empty());

    // The author gets nothing (no self-notification).
    assert!(index_all(&srv, alice.id).is_empty());
}

#[test]
fn d51_update_and_quoted_update_reach_rebloggers_and_quoters() {
    let srv = TestServer::new(false);
    let author = common::mk_account(&srv.store, "gb_author");
    let booster = common::mk_account(&srv.store, "gb_booster");
    let quoter = common::mk_account(&srv.store, "gb_quoter");
    let status_id = Id::new(910_011);

    assert!(notif::emit_update_for_rebloggers(author.id, status_id, &[], T0).is_empty());
    assert!(notif::emit_quoted_update_for_quoters(author.id, status_id, &[], T0).is_empty());

    let created = run_pending(
        &srv,
        notif::emit_update_for_rebloggers(author.id, status_id, &[booster.id], T0),
    );
    assert_eq!(created.len(), 1);
    let created = run_pending(
        &srv,
        notif::emit_quoted_update_for_quoters(author.id, status_id, &[quoter.id], T0 + 1),
    );
    assert_eq!(created.len(), 1);

    let rows = index_all(&srv, booster.id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].r#type, NotificationType::Update);
    assert_eq!(rows[0].status_id, Some(status_id));

    let rows = index_all(&srv, quoter.id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].r#type, NotificationType::QuotedUpdate);
    assert_eq!(rows[0].actor_id, Some(author.id));
}

#[test]
fn d51_actorless_rows_are_visible_in_index_and_show() {
    let srv = TestServer::new(false);
    let target = common::mk_account(&srv.store, "gb_target");
    let admin = common::mk_account(&srv.store, "gb_admin");
    let reporter = common::mk_account(&srv.store, "gb_reporter");

    // Actor-less (warning/annual) + staff-routed (sign-up/report) producers.
    let jobs = notif::emit_moderation_warning(target.id, T0);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].payload["type"], Value::String("moderation_warning".into()));
    let jobs2 = notif::emit_annual_report(target.id, T0 + 1);
    let jobs3 = notif::emit_admin_sign_up(target.id, &[admin.id], T0 + 2);
    assert_eq!(jobs3[0].payload["actor_id"], Value::from(target.id.value()));
    let jobs4 = notif::emit_admin_report(reporter.id, &[admin.id], T0 + 3);
    let all: Vec<_> = jobs.into_iter().chain(jobs2).chain(jobs3).chain(jobs4).collect();
    let created = run_pending(&srv, all);
    assert_eq!(created.len(), 4, "actor-less rows are kept, not dropped");

    // Visible in the index (the narrowed `notification_excluded`): the
    // warning + annual report belong to the target, the staff pair to admin.
    let store: &dyn birchtree::application::ports::Store = &*srv.store;
    let target_rows = index_all(&srv, target.id);
    assert_eq!(target_rows.len(), 2);
    let mut kinds: Vec<_> = target_rows.iter().map(|n| n.r#type.as_str()).collect();
    kinds.sort_unstable();
    assert_eq!(kinds, vec!["annual_report", "moderation_warning"]);

    // `show` serves them (no 404 for actor-less rows).
    for row in &target_rows {
        let shown = notif::show(store, target.id, row.id).expect("show actor-less");
        assert_eq!(shown.id, row.id);
    }

    let admin_rows = index_all(&srv, admin.id);
    assert_eq!(admin_rows.len(), 2);
    let mut kinds: Vec<_> = admin_rows.iter().map(|n| n.r#type.as_str()).collect();
    kinds.sort_unstable();
    assert_eq!(kinds, vec!["admin.report", "admin.sign_up"]);
}

#[test]
fn d51_unwired_types_stay_empty() {
    let srv = TestServer::new(false);
    let alice = common::mk_account(&srv.store, "gb_u_alice");
    let bob = common::mk_account(&srv.store, "gb_u_bob");

    // Builders with empty inputs invent nothing.
    assert!(notif::emit_added_to_collection(bob.id, alice.id, T0).len() == 1);
    assert!(notif::emit_collection_update(&[], alice.id, T0).is_empty());
    assert!(notif::emit_severed_relationships(bob.id, T0).len() == 1);

    // And with no trigger feature wired, no call site runs them: after real
    // activity (a mention + a follow), pages naming those types are empty.
    let status_id = Id::new(910_021);
    let jobs = notif::emit_status_for_followers(alice.id, status_id, &[bob.id], T0);
    run_pending(&srv, jobs);
    for kind in [
        NotificationType::SeveredRelationships,
        NotificationType::AddedToCollection,
        NotificationType::CollectionUpdate,
    ] {
        let mut rows = index_all(&srv, bob.id);
        notif::retain_types(&mut rows, &[kind]);
        assert!(rows.is_empty(), "no {kind:?} rows without the feature");
    }
    // Sanity: the control row exists, so the empty pages are honest.
    assert_eq!(index_all(&srv, bob.id).len(), 1);
}

// ---------------------------------------------------------------------------
// `include_filtered`.
// ---------------------------------------------------------------------------

fn crafted(id_value: u64, actor: Option<Id>, filtered: bool) -> Notification {
    Notification {
        id: Id::new(id_value),
        recipient_id: Id::new(1),
        r#type: NotificationType::Mention,
        actor_id: actor,
        status_id: None,
        read: false,
        filtered,
        created_at_ms: T0,
    }
}

#[test]
fn include_filtered_merge_orders_numerically_and_respects_cursors() {
    // Decimal-string ids compare NUMERICALLY: 9 < 100 even though "9" > "100"
    // lexicographically. The merge must be newest-first by value.
    let page = vec![crafted(100, Some(Id::new(7)), false)];
    let filtered = vec![
        crafted(9, Some(Id::new(7)), true),
        crafted(50, Some(Id::new(8)), true),
    ];
    let query = notif::NotificationQuery {
        max_id: None,
        since_id: None,
        min_id: None,
        limit: 10,
        from_account: None,
        include_filtered: true,
    };
    let merged = notif::merge_filtered_index(page, filtered, &query);
    let ids: Vec<u64> = merged.iter().map(|n| n.id.value()).collect();
    assert_eq!(ids, vec![100, 50, 9]);

    // Exclusive cursors apply to both halves.
    let query = notif::NotificationQuery {
        max_id: Some(Id::new(100)),
        limit: 10,
        ..query
    };
    let merged = notif::merge_filtered_index(
        vec![crafted(100, Some(Id::new(7)), false)],
        vec![crafted(9, Some(Id::new(7)), true)],
        &query,
    );
    assert_eq!(
        merged.iter().map(|n| n.id.value()).collect::<Vec<_>>(),
        vec![9]
    );

    // Actor narrowing + limit truncation.
    let query = notif::NotificationQuery {
        max_id: None,
        from_account: Some(Id::new(8)),
        limit: 1,
        ..query
    };
    let merged = notif::merge_filtered_index(
        vec![crafted(100, Some(Id::new(7)), false)],
        vec![
            crafted(50, Some(Id::new(8)), true),
            crafted(60, Some(Id::new(8)), true),
        ],
        &query,
    );
    assert_eq!(
        merged.iter().map(|n| n.id.value()).collect::<Vec<_>>(),
        vec![60]
    );
}

#[test]
fn include_filtered_serves_a_real_filtered_row() {
    let srv = TestServer::new(false);
    let alice = common::mk_account(&srv.store, "gb_f_alice");
    let bob = common::mk_account(&srv.store, "gb_f_bob");
    let store: &dyn birchtree::application::ports::Store = &*srv.store;

    // A genuinely filtered row (stored hidden, as the policy pipeline writes
    // it): the default index hides it.
    let filtered_row = store
        .create_notification(birchtree::application::ports::NewNotification {
            recipient_id: bob.id,
            r#type: NotificationType::Mention,
            actor_id: Some(alice.id),
            status_id: None,
            created_at_ms: T0,
            filtered: true,
        })
        .expect("store")
        .expect("row");
    assert!(filtered_row.filtered);
    let page = index_all(&srv, bob.id);
    assert!(page.iter().all(|n| n.id != filtered_row.id));

    // With `include_filtered`, the endpoint-supplemented merge serves it.
    let query = notif::NotificationQuery {
        max_id: None,
        since_id: None,
        min_id: None,
        limit: 40,
        from_account: None,
        include_filtered: true,
    };
    let merged = notif::merge_filtered_index(page, vec![filtered_row.clone()], &query);
    assert!(merged.iter().any(|n| n.id == filtered_row.id));
}

// ---------------------------------------------------------------------------
// `supported_types` fallback.
// ---------------------------------------------------------------------------

#[test]
fn supported_types_baseline_and_fallback_rules_match_upstream() {
    // Upstream PROPERTIES baseline flags: 11 baseline, 6 fallback candidates.
    let baseline = [
        NotificationType::Mention,
        NotificationType::Status,
        NotificationType::Reblog,
        NotificationType::Follow,
        NotificationType::FollowRequest,
        NotificationType::Favourite,
        NotificationType::PollVote,
        NotificationType::Update,
        NotificationType::AnnualReport,
        NotificationType::Quote,
        NotificationType::QuotedUpdate,
    ];
    for kind in baseline {
        assert!(notif::baseline_type(kind), "{kind:?} is baseline");
        // Baseline types never get a fallback, even when the client lists
        // nothing and the parameter is present.
        assert!(!notif::needs_fallback(kind, Some(&[]), true));
        assert!(!notif::needs_fallback(kind, None, true));
    }
    let candidates = [
        NotificationType::SeveredRelationships,
        NotificationType::ModerationWarning,
        NotificationType::AdminSignUp,
        NotificationType::AdminReport,
        NotificationType::AddedToCollection,
        NotificationType::CollectionUpdate,
    ];
    for kind in candidates {
        assert!(!notif::baseline_type(kind), "{kind:?} is not baseline");
        // Absent parameter: never any fallback.
        assert!(!notif::needs_fallback(kind, None, true));
        // Listed by the client: no fallback.
        assert!(!notif::needs_fallback(kind, Some(&[kind]), true));
    }
    // Unlisted + present: fallback, except the activity-gated three without
    // their activity (never fabricated).
    assert!(notif::needs_fallback(
        NotificationType::ModerationWarning,
        Some(&[]),
        false
    ));
    assert!(notif::needs_fallback(
        NotificationType::AdminSignUp,
        Some(&[]),
        false
    ));
    assert!(!notif::needs_fallback(
        NotificationType::SeveredRelationships,
        Some(&[]),
        false
    ));
    assert!(notif::needs_fallback(
        NotificationType::SeveredRelationships,
        Some(&[]),
        true
    ));
    assert!(!notif::needs_fallback(
        NotificationType::AdminReport,
        Some(&[]),
        false
    ));
    assert!(!notif::needs_fallback(
        NotificationType::AddedToCollection,
        Some(&[]),
        false
    ));
    assert!(notif::needs_fallback(
        NotificationType::CollectionUpdate,
        Some(&[]),
        true
    ));
}

#[test]
fn fallback_object_shape_matches_the_concern() {
    // Keys per `NotificationFallbackConcern#fallback`: title/summary (+ the
    // concern's `description: nil`, NOT the docs-site `details`).
    let fallback = notif::fallback_for(NotificationType::AddedToCollection, "alice");
    assert_eq!(
        fallback["title"],
        Value::String("@alice added you to a collection".into())
    );
    assert!(fallback["summary"].is_string());
    assert_eq!(fallback["description"], Value::Null);
    assert!(fallback.get("details").is_none());

    let fallback = notif::fallback_for(NotificationType::ModerationWarning, "alice");
    assert_eq!(
        fallback["title"],
        Value::String("A moderator has taken action on your account".into())
    );
    let fallback = notif::fallback_for(NotificationType::AdminSignUp, "newbie");
    assert_eq!(fallback["title"], Value::String("@newbie signed up".into()));
}

// ---------------------------------------------------------------------------
// Pre-token push backfill.
// ---------------------------------------------------------------------------

#[test]
fn push_backfill_heals_pre_url_rows_and_leaves_complete_rows_alone() {
    use birchtree::application::auth;

    let srv = TestServer::new(false);
    let store: &dyn birchtree::application::ports::Store = &*srv.store;
    let alice = common::mk_account(&srv.store, "gb_p_alice");
    let app = auth::register_app(
        &*srv.store,
        "GapsB push".to_string(),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .expect("app");
    let (_, token_row) = auth::issue_user_token(
        &*srv.store,
        app.app.id,
        alice.id,
        vec!["read".to_string(), "read:notifications".to_string()],
        T0,
    )
    .expect("token");

    // A pre-URL row (created without the base URL, as before the fix round):
    // token minted, URL absent, no header at send time.
    let row = store
        .upsert_push_subscription(
            token_row.id,
            alice.id,
            "https://push.example/sub".to_string(),
            String::new(),
            String::new(),
            true,
            "all".to_string(),
            HashMap::from([("mention".to_string(), true)]),
            None,
            T0,
        )
        .expect("subscribe");
    assert!(!row.unsubscribe_token.is_empty(), "token minted even then");
    assert_eq!(row.unsubscribe_url, None);

    // Heal: same row, token preserved, absolute URL built — never fabricated
    // (it names the real bearer token).
    let base = "https://example.social";
    match notif::backfill_push_unsubscribe(store, token_row.id, base, T0 + 1_000).expect("backfill") {
        notif::PushBackfillOutcome::Healed(healed) => {
            assert_eq!(healed.id, row.id);
            assert_eq!(healed.unsubscribe_token, row.unsubscribe_token);
            assert_eq!(
                healed.unsubscribe_url,
                Some(format!("{base}/api/web/push_subscriptions/{}", row.unsubscribe_token))
            );
        }
        other => panic!("expected Healed, got {other:?}"),
    }

    // Second call: complete rows are returned untouched (no write churn).
    let before = store
        .push_subscription_by_token(token_row.id)
        .expect("read")
        .expect("row");
    match notif::backfill_push_unsubscribe(store, token_row.id, base, T0 + 2_000).expect("backfill") {
        notif::PushBackfillOutcome::Complete(row) => {
            assert_eq!(row.updated_at_ms, before.updated_at_ms);
        }
        other => panic!("expected Complete, got {other:?}"),
    }

    // Unknown token: nothing to heal, nothing created.
    assert_eq!(
        notif::backfill_push_unsubscribe(store, Id::new(987_654_321), base, T0)
            .expect("backfill"),
        notif::PushBackfillOutcome::Missing
    );

    // New subscriptions created WITH the base URL are complete immediately.
    let app2 = auth::register_app(
        &*srv.store,
        "GapsB push2".to_string(),
        vec!["https://client.example/cb".into()],
        vec!["read".into()],
        None,
        true,
        T0,
    )
    .expect("app");
    let (_, token2) = auth::issue_user_token(
        &*srv.store,
        app2.app.id,
        alice.id,
        vec!["read".to_string()],
        T0,
    )
    .expect("token");
    let fresh = store
        .upsert_push_subscription(
            token2.id,
            alice.id,
            "https://push.example/sub2".to_string(),
            String::new(),
            String::new(),
            true,
            "all".to_string(),
            HashMap::new(),
            Some(base),
            T0,
        )
        .expect("subscribe");
    assert!(fresh.unsubscribe_url.is_some());
}

// ---------------------------------------------------------------------------
// Streaming announcement events (socket capture through the real hub).
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

async fn open_sse(addr: SocketAddr, path: &str, token: &str) -> Sse {
    let response = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .expect("sse request");
    assert_eq!(response.status(), StatusCode::OK, "sse {path}");
    Sse {
        response,
        buffer: String::new(),
        pending: std::collections::VecDeque::new(),
        name: None,
        data: None,
    }
}

async fn wait_event(stream: &mut Sse, name: &str) -> String {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match stream.next_block(left).await {
            Some(Block::Event { name: got, data }) if got == name => return data,
            Some(_) => continue,
            None => break,
        }
    }
    panic!("timed out waiting for stream event {name}");
}

#[test]
fn announcement_payload_shape_matches_the_rest_serializer() {
    let srv = TestServer::new(false);
    let alice = common::mk_account(&srv.store, "gb_a_alice");
    let bob = common::mk_account(&srv.store, "gb_a_bob");
    let server = common::server_info();
    let store: &dyn birchtree::application::ports::Store = &*srv.store;

    let row = ann_uc::create_announcement(
        store,
        ann_uc::NewAnnouncement {
            text: "Hello @gb_a_bob #news".to_string(),
            all_day: false,
            starts_at_ms: None,
            ends_at_ms: None,
            scheduled_at_ms: None,
        },
        T0,
        Id::new(760_001),
    )
    .expect("create");
    assert!(row.published, "no schedule pins published immediately");

    // Bob's view: unread, mention resolved, tag extracted, no reactions yet.
    let payload =
        fanout::announcement_payload(&server, store, bob.id, &row).expect("payload");
    assert_eq!(payload["id"], Value::String(row.id.to_string()));
    assert_eq!(
        payload["content"],
        Value::String("Hello @gb_a_bob #news".into()),
        "content is the escaped text (no escapable chars here); handles resolve separately"
    );
    assert_eq!(payload["read"], Value::Bool(false));
    assert_eq!(payload["mentions"][0]["username"], Value::String("gb_a_bob".into()));
    assert_eq!(payload["tags"][0]["name"], Value::String("news".into()));
    assert_eq!(payload["reactions"], Value::Array(Vec::new()));
    assert_eq!(payload["statuses"], Value::Array(Vec::new()));

    // Alice reacts: count + `me` flag are per-viewer.
    ann_uc::add_reaction(store, row.id, alice.id, "🎉", None, T0 + 1, Id::new(760_002))
        .expect("react");
    let alice_view = fanout::announcement_payload(&server, store, alice.id, &row).expect("payload");
    assert_eq!(alice_view["reactions"][0]["name"], Value::String("🎉".into()));
    assert_eq!(alice_view["reactions"][0]["count"], Value::from(1u64));
    assert_eq!(alice_view["reactions"][0]["me"], Value::Bool(true));
    let bob_view = fanout::announcement_payload(&server, store, bob.id, &row).expect("payload");
    assert_eq!(bob_view["reactions"][0]["me"], Value::Bool(false));

    // Bob dismisses: his view reads `read: true`, Alice's stays false.
    ann_uc::dismiss(store, row.id, bob.id, T0 + 2).expect("dismiss");
    let bob_view = fanout::announcement_payload(&server, store, bob.id, &row).expect("payload");
    assert_eq!(bob_view["read"], Value::Bool(true));
    let alice_view = fanout::announcement_payload(&server, store, alice.id, &row).expect("payload");
    assert_eq!(alice_view["read"], Value::Bool(false));

    // Reaction payload: the documented `{name, count, announcement_id}`.
    let reaction = fanout::announcement_reaction_payload(&server, row.id, "🎉", 1, None);
    assert_eq!(reaction["name"], Value::String("🎉".into()));
    assert_eq!(reaction["count"], Value::from(1u64));
    assert_eq!(reaction["announcement_id"], Value::String(row.id.to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn announcement_events_reach_user_streams_but_not_notification_streams() {
    use birchtree::streaming::hub::AnnouncementEvent;

    let srv = TestServer::new(true);
    let addr = serve(&srv).await;
    let alice = user_token(&srv, "gb_s_alice").await;
    let alice_id = account_id(&srv, "gb_s_alice");
    let store: &dyn birchtree::application::ports::Store = &*srv.store;

    let row = ann_uc::create_announcement(
        store,
        ann_uc::NewAnnouncement {
            text: "Downtime on Sunday".to_string(),
            all_day: false,
            starts_at_ms: None,
            ends_at_ms: None,
            scheduled_at_ms: None,
        },
        T0,
        Id::new(760_011),
    )
    .expect("create");

    // Open BEFORE publishing: streams never replay.
    let mut user = open_sse(addr, "/api/v1/streaming/user", &alice).await;
    let mut user_notif = open_sse(addr, "/api/v1/streaming/user/notification", &alice).await;

    srv.streams.publish_announcement(AnnouncementEvent::Published {
        announcement_id: row.id,
    });
    let data = wait_event(&mut user, "announcement").await;
    let payload: Value = serde_json::from_str(&data).expect("announcement json");
    assert_eq!(payload["id"], Value::String(row.id.to_string()));
    assert!(payload["content"].as_str().expect("content").contains("Downtime"));

    // A reaction publishes `{name, count, announcement_id}`.
    ann_uc::add_reaction(store, row.id, alice_id, "🎉", None, T0 + 1, Id::new(760_012))
        .expect("react");
    srv.streams.publish_announcement(AnnouncementEvent::Reaction {
        announcement_id: row.id,
        name: "🎉".to_string(),
    });
    let data = wait_event(&mut user, "announcement.reaction").await;
    let payload: Value = serde_json::from_str(&data).expect("reaction json");
    assert_eq!(payload, serde_json::json!({
        "name": "🎉",
        "count": 1,
        "announcement_id": row.id.to_string(),
        "url": null,
        "static_url": null,
    }));

    // Deletion carries the bare id string (the row is gone).
    srv.store
        .delete_announcement(row.id)
        .expect("destroy");
    srv.streams.publish_announcement(AnnouncementEvent::Deleted {
        announcement_id: row.id,
    });
    let data = wait_event(&mut user, "announcement.delete").await;
    assert_eq!(data, row.id.to_string(), "bare id payload, like `delete`");

    // The `user:notification` stream (the `:notifications` channel upstream)
    // carries none of the three.
    for block in user_notif.collect(QUIET).await {
        assert!(
            !matches!(&block, Block::Event { name, .. } if name == "announcement" || name == "announcement.reaction" || name == "announcement.delete"),
            "notification streams carry no announcement events: {block:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Regression pin: the narrowed exclusion still drops suspended/gone actors.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn suspended_actor_rows_stay_hidden() {
    let srv = TestServer::new(true);
    let _bob = user_token(&srv, "gb_x_bob").await;
    let _alice = user_token(&srv, "gb_x_alice").await;

    let bob_id = account_id(&srv, "gb_x_bob");
    let alice_id = account_id(&srv, "gb_x_alice");
    let store: &dyn birchtree::application::ports::Store = &*srv.store;
    let status_id = Id::new(910_031);
    let created = run_pending(
        &srv,
        notif::emit_status_for_followers(bob_id, status_id, &[alice_id], T0),
    );
    assert_eq!(created.len(), 1);
    assert_eq!(index_all(&srv, alice_id).len(), 1);

    // After the actor is suspended, the row hides (the `without_suspended`
    // half of `notification_excluded`, unchanged by the actor-less
    // narrowing) and `show` 404s.
    store
        .admin_account_action(alice_id, bob_id, "suspend", None, None, T0 + 1)
        .expect("suspend");
    assert!(index_all(&srv, alice_id).is_empty());
    assert!(notif::show(store, alice_id, created[0]).is_err());
}

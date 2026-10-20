//! Round 4, D39 remainder: the unified interaction-event journal and the
//! served `interactions` measure.
//!
//! Upstream pins (Mastodon v4.7.3, `8ff11c2b`) — the five
//! `ActivityTracker('activity:interactions')` call sites:
//! - `app/services/post_status_service.rb:262-266`
//!   (`bump_potential_friendship!`): ONLY non-self replies increment.
//!   Top-level posts and self-replies never do — there is no `Post` kind.
//! - `app/services/favourite_service.rb:40-42`: after create; existing rows
//!   return early without incrementing.
//! - `app/services/reblog_service.rb:49-51`: same dedup shape.
//! - `app/services/vote_service.rb:33`: unconditional after the ballot
//!   transaction (even when `already_voted`).
//! - `app/services/follow_service.rb:33`: before the direct/request branch
//!   (requests count), after the already-following/requested early returns.
//! - Measure shape: `app/lib/admin/metrics/measure/interactions_measure.rb`
//!   (`:basic` per-day counter; `total` + `previous_total` + per-day `data`).
//! - `software_versions` stays refused: per-component live probes in
//!   `app/lib/admin/metrics/dimension/software_versions_dimension.rb:13-19`
//!   (mastodon/ruby/postgresql/redis/[elasticsearch|opensearch]/libvips/ffmpeg)
//!   that this server does not collect.
//!
//! Writers landed here: non-self replies (`statuses::create_status`), new
//! follows/requests (`relationships::follow`), cast ballots (`polls::vote`).
//! Favourite/reblog writers live in `application::interactions` (parent
//! snippet, pending): the HTTP measure test below asserts `total >= 3` and
//! exact per-kind counts for the landed writers, so it stays green when those
//! two land (parent then bumps the `>=` bound, it becomes 5).

mod common;

use std::sync::Arc;

use axum::http::StatusCode;

use birchtree::application::polls;
use birchtree::application::ports::{AccountStore, InteractionLog, PollStore, StatusStore};
use birchtree::application::relationships::{self, FollowCommand};
use birchtree::application::statuses::{CreateStatusCommand, PollCommand, create_status};
use birchtree::domain::{Clock, Id, InteractionEventKind, Status, SystemClock, UserRole};
use birchtree::persistence::redb::RedbStore;

use common::{TestServer, form_request, json, send};

/// Upper-bound slack for count windows: the id-range scan drops rows minted
/// past `until_ms`, so a bound within a millisecond of the write clock is
/// racy (see the invariant note in `persistence::redb::interactions`). All
/// count queries below carry minutes of slack above the last write.
const END_SLACK_MS: u64 = 120_000;
const ALL_SCOPES: &str = "read write follow read:accounts write:accounts read:statuses write:statuses read:follows write:follows write:reports admin:read admin:write admin:read:accounts admin:write:accounts admin:read:reports admin:write:reports";

fn live_now() -> u64 {
    Arc::new(SystemClock).now_ms()
}

fn count(store: &RedbStore, kind: Option<InteractionEventKind>, since: u64, until: u64) -> u64 {
    store
        .count_interactions(kind, since, until)
        .expect("count interactions")
}

/// Post via the application use case (the journaling path).
#[allow(clippy::too_many_arguments)]
fn post(
    store: &RedbStore,
    author: Id,
    text: &str,
    reply_to: Option<Id>,
    now: u64,
    key: Option<&str>,
    poll: Option<PollCommand>,
) -> Status {
    create_status(
        store,
        "https://example.social",
        now,
        CreateStatusCommand {
            account_id: author,
            text: Some(text.to_string()),
            in_reply_to_id: reply_to,
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: Some("public".to_string()),
            language: None,
            media_ids: Vec::new(),
            poll,
            application_name: None,
            application_website: None,
            idempotency_key: key.map(str::to_string),
        },
        500,
    )
    .expect("post")
    .status
}

fn follow(store: &RedbStore, follower: Id, target: Id, now: u64) {
    relationships::follow(
        store,
        now,
        FollowCommand {
            follower_id: follower,
            target_id: target,
            show_reblogs: None,
            notify: None,
            languages: None,
        },
    )
    .expect("follow");
}

// ---------------------------------------------------------------------------
// Writer: replies only (upstream `bump_potential_friendship!`).
// ---------------------------------------------------------------------------

#[test]
fn reply_journals_once_top_level_and_self_reply_do_not() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&store, "alice");
    let bob = common::mk_account(&store, "bob");
    let t = live_now();

    let root = post(&store, alice.id, "hello world", None, t, None, None);
    assert_eq!(count(&store, None, 0, t + END_SLACK_MS), 0, "top-level post journals nothing");

    post(&store, alice.id, "self reply", Some(root.id), t, None, None);
    assert_eq!(count(&store, None, 0, t + END_SLACK_MS), 0, "self-reply journals nothing");

    post(&store, bob.id, "hi alice", Some(root.id), t, None, None);
    assert_eq!(count(&store, None, 0, t + END_SLACK_MS), 1, "non-self reply journals once");
    assert_eq!(count(&store, Some(InteractionEventKind::Reply), 0, t + END_SLACK_MS), 1);
    assert_eq!(count(&store, Some(InteractionEventKind::Follow), 0, t + END_SLACK_MS), 0);
    assert_eq!(count(&store, Some(InteractionEventKind::Vote), 0, t + END_SLACK_MS), 0);
}

#[test]
fn idempotent_reply_replay_journals_once() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&store, "alice");
    let bob = common::mk_account(&store, "bob");
    let t = live_now();
    let root = post(&store, alice.id, "hello world", None, t, None, None);

    let first = create_status(
        &*store,
        "https://example.social",
        t,
        CreateStatusCommand {
            account_id: bob.id,
            text: Some("hi alice".to_string()),
            in_reply_to_id: Some(root.id),
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: Some("public".to_string()),
            language: None,
            media_ids: Vec::new(),
            poll: None,
            application_name: None,
            application_website: None,
            idempotency_key: Some("reply-k1".to_string()),
        },
        500,
    )
    .expect("first reply");
    assert!(!first.replayed);
    let second = create_status(
        &*store,
        "https://example.social",
        t,
        CreateStatusCommand {
            account_id: bob.id,
            text: Some("hi alice".to_string()),
            in_reply_to_id: Some(root.id),
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: Some("public".to_string()),
            language: None,
            media_ids: Vec::new(),
            poll: None,
            application_name: None,
            application_website: None,
            idempotency_key: Some("reply-k1".to_string()),
        },
        500,
    )
    .expect("replay");
    assert!(second.replayed, "same idempotency key replays");
    assert_eq!(
        count(&store, Some(InteractionEventKind::Reply), 0, t + END_SLACK_MS),
        1,
        "replay must not re-journal"
    );
}

// ---------------------------------------------------------------------------
// Writer: new follows and new requests (upstream `FollowService#call`).
// ---------------------------------------------------------------------------

#[test]
fn follow_journals_new_follows_and_requests_only() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&store, "alice");
    let bob = common::mk_account(&store, "bob");
    let t = live_now();

    follow(&store, bob.id, alice.id, t);
    assert_eq!(count(&store, Some(InteractionEventKind::Follow), 0, t + END_SLACK_MS), 1);

    // Option change on the existing row (distinct ms to dodge the same-ms
    // caveat documented at the writer): journals nothing.
    relationships::follow(
        &*store,
        t + 1,
        FollowCommand {
            follower_id: bob.id,
            target_id: alice.id,
            show_reblogs: Some(false),
            notify: None,
            languages: None,
        },
    )
    .expect("option change");
    assert_eq!(
        count(&store, Some(InteractionEventKind::Follow), 0, t + END_SLACK_MS),
        1,
        "option change must not re-journal"
    );

    // Unfollow + re-follow is a genuinely new follow: journals again.
    relationships::unfollow(&*store, bob.id, alice.id, t + 2).expect("unfollow");
    follow(&store, bob.id, alice.id, t + 3);
    assert_eq!(count(&store, Some(InteractionEventKind::Follow), 0, t + END_SLACK_MS), 2);
}

// ---------------------------------------------------------------------------
// Writer: cast ballots (upstream `VoteService#call`).
// ---------------------------------------------------------------------------

fn poll_command() -> PollCommand {
    PollCommand {
        multiple: false,
        hide_totals: false,
        expires_in_s: 3600,
        options: vec!["yes".to_string(), "no".to_string()],
    }
}

#[test]
fn vote_journals_cast_ballots_only() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&store, "alice");
    let bob = common::mk_account(&store, "bob");
    let t = live_now();
    let status = post(&store, alice.id, "pick one", None, t, None, Some(poll_command()));
    let poll = store
        .poll_by_status(status.id)
        .expect("poll read")
        .expect("poll row");

    polls::vote(&*store, bob.id, poll.id, &[0], t).expect("vote");
    assert_eq!(count(&store, Some(InteractionEventKind::Vote), 0, t + END_SLACK_MS), 1);

    // Duplicate ballot on a single-choice poll is rejected: journals nothing.
    assert!(polls::vote(&*store, bob.id, poll.id, &[0], t).is_err());
    // Empty ballot is rejected before the store: journals nothing.
    assert!(polls::vote(&*store, bob.id, poll.id, &[], t).is_err());
    assert_eq!(
        count(&store, Some(InteractionEventKind::Vote), 0, t + END_SLACK_MS),
        1,
        "rejected votes must not journal"
    );
}

// ---------------------------------------------------------------------------
// Local-only: store-level writes (the federation-inbound shape) journal
// nothing — inbound callers use the ports directly, bypassing the use cases.
// ---------------------------------------------------------------------------

#[test]
fn direct_store_writes_journal_nothing() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&store, "alice");
    let bob = common::mk_account(&store, "bob");
    let t = live_now();
    let status = post(&store, alice.id, "hello world", None, t, None, Some(poll_command()));
    let poll = store
        .poll_by_status(status.id)
        .expect("poll read")
        .expect("poll row");

    // Federation inbound shapes: direct port calls, no use case.
    let favs: &dyn StatusStore = &*store;
    favs.favourite_status(bob.id, status.id, t).expect("fav");
    store.vote_poll(poll.id, bob.id, &[1], t).expect("tally");
    assert_eq!(
        count(&store, None, 0, t + END_SLACK_MS),
        0,
        "remote/inbound-shaped writes stay out of the journal"
    );
}

// ---------------------------------------------------------------------------
// Reader: window semantics, kind filter, empty/inverted ranges.
// ---------------------------------------------------------------------------

#[test]
fn count_respects_window_kind_and_empty_range() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&store, "alice");
    let bob = common::mk_account(&store, "bob");

    // Separate the two writes on the wall clock so a window edge falls
    // strictly between their mint times (same-ms edges are racy against the
    // id-range scan; see the invariant note in
    // `persistence::redb::interactions`).
    let root = post(&store, alice.id, "hello world", None, live_now(), None, None);
    let t_a = live_now();
    post(&store, bob.id, "old reply", Some(root.id), t_a, None, None);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let edge = live_now();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let t_b = live_now();
    post(&store, bob.id, "new reply", Some(root.id), t_b, None, None);
    assert!(t_a < edge && edge <= t_b, "writes straddle the edge");

    let end = t_b + END_SLACK_MS;
    assert_eq!(count(&store, None, 0, end), 2);
    assert_eq!(count(&store, None, edge, end), 1, "window keeps [since, until)");
    assert_eq!(count(&store, None, 0, edge), 1, "until is exclusive");
    assert_eq!(count(&store, Some(InteractionEventKind::Reply), 0, end), 2);
    assert_eq!(count(&store, Some(InteractionEventKind::Vote), 0, end), 0);
    assert_eq!(count(&store, None, end, end), 0, "empty range");
    assert_eq!(count(&store, None, end + 2000, end), 0, "inverted range");
}

// ---------------------------------------------------------------------------
// HTTP: `interactions` served with real counts; `software_versions` refused.
// ---------------------------------------------------------------------------

async fn register_app(router: &axum::Router, scopes: &str) -> serde_json::Value {
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
    let body = format!(
        "client_name=Test+App&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) = send(router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

fn mint_user_token(store: &RedbStore, app: &serde_json::Value, account_id: Id) -> String {
    use birchtree::application::ports::OAuthStore;
    let oauth: &dyn OAuthStore = store;
    let app_id: Id = app["id"].as_str().unwrap().parse().expect("app id");
    let (plaintext, _) = birchtree::application::auth::issue_user_token(
        oauth,
        app_id,
        account_id,
        ALL_SCOPES.split(' ').map(str::to_string).collect(),
        live_now(),
    )
    .expect("issue user token");
    plaintext
}

fn by_key<'a>(rows: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    rows.as_array().unwrap().iter().find(|r| r["key"] == key)
}

#[tokio::test]
async fn measures_interactions_served_with_real_counts() {
    let srv = TestServer::new(true);
    let app = register_app(&srv.router, ALL_SCOPES).await;
    let admin = common::mk_account(&srv.store, "boss");
    srv.store
        .set_user_role(admin.id, UserRole::Admin)
        .expect("grant role");
    let admin_token = mint_user_token(&srv.store, &app, admin.id);
    let alice = common::mk_account(&srv.store, "alice");
    let bob = common::mk_account(&srv.store, "bob");
    let alice_token = mint_user_token(&srv.store, &app, alice.id);
    let bob_token = mint_user_token(&srv.store, &app, bob.id);

    // Alice posts (top-level: journals nothing) via HTTP.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", "status=hello+world", Some(&alice_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "post: {}", String::from_utf8_lossy(&bytes));
    let root_id = json(&bytes)["id"].as_str().unwrap().to_string();

    // Bob replies via HTTP (Reply +1).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/statuses",
            &format!("status=hi+there&in_reply_to_id={root_id}"),
            Some(&bob_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reply: {}", String::from_utf8_lossy(&bytes));

    // Bob follows Alice via HTTP (Follow +1).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/accounts/{}/follow", alice.id),
            "",
            Some(&bob_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow: {}", String::from_utf8_lossy(&bytes));

    // Alice posts a poll, Bob votes (Vote +1) — through the journaling use
    // cases at the live clock so the trailing-30-day window covers them.
    let now = live_now();
    let with_poll = post(&srv.store, alice.id, "pick one", None, now, None, Some(poll_command()));
    let poll = srv
        .store
        .poll_by_status(with_poll.id)
        .expect("poll read")
        .expect("poll row");
    polls::vote(&*srv.store, bob.id, poll.id, &[0], now).expect("vote");

    // Per-kind ground truth straight from the journal.
    let end = now + 1000;
    assert_eq!(srv.store.count_interactions(Some(InteractionEventKind::Reply), 0, end).unwrap(), 1);
    assert_eq!(srv.store.count_interactions(Some(InteractionEventKind::Follow), 0, end).unwrap(), 1);
    assert_eq!(srv.store.count_interactions(Some(InteractionEventKind::Vote), 0, end).unwrap(), 1);

    // The measure serves real counts in the upstream shape.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/admin/measures",
            "keys[]=interactions",
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "measures: {}", String::from_utf8_lossy(&bytes));
    let rows = json(&bytes);
    let row = by_key(&rows, "interactions").expect("interactions is served");
    assert!(row["unit"].is_null());
    let total: u64 = row["total"].as_str().unwrap().parse().expect("total string");
    // Exactly the three landed writers today (favourite/reblog writers are a
    // pending parent snippet; this bound becomes 5 when they land).
    assert!(total >= 3, "real counts, never fabricated: {row}");
    assert_eq!(row["previous_total"], "0");
    let data = row["data"].as_array().expect("daily data");
    assert!(!data.is_empty());
    let mut sum = 0u64;
    for point in data {
        assert!(point["date"].is_string());
        sum += point["value"].as_str().unwrap().parse::<u64>().expect("value string");
    }
    assert_eq!(sum, total, "total is the sum of the daily buckets, like upstream");

    // `software_versions` stays refused (absent, not zero-filled).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/admin/dimensions",
            "keys[]=software_versions&keys[]=servers",
            Some(&admin_token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dimensions: {}", String::from_utf8_lossy(&bytes));
    let rows = json(&bytes);
    assert!(by_key(&rows, "software_versions").is_none(), "no census: {rows}");
    assert!(by_key(&rows, "servers").is_some(), "sibling key served: {rows}");
}

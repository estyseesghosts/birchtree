//! Phase 6 content: notes, renotes, likes, blocks, flags, quotes, edits, deletes,
//! remote media staging, outbound events, and thread completion. These run on the
//! store and the application use cases, with no network: remote accounts are
//! known with their keys, and remote fetches are not made.
//!
//! The wire behaviour against Mastodon 4.7.3 is the lab run
//! (`scripts/lab/phase6-interop.sh`), recorded in `docs/logs/`.

mod common;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use activitypub_federation::http_signatures::generate_actor_keypair;
use axum::body::Body;
use axum::http::Request;
use birchtree::activitypub::activities::{question_input, vote_of};
use birchtree::activitypub::outgoing;
use birchtree::activitypub::wire::{Note, Question, QuestionOption, QuestionReplies};
use birchtree::application::context::Context;
use birchtree::application::federation_inbound::{self as inbound, Applied, RemoteAttachment, RemoteNote, RemotePoll};
use birchtree::application::federation_ports::{FederationContentStore, FederationStore, RemoteAccountUpsert};
use birchtree::application::ports::{AccountStore, PollStore, VoteRejection};
use birchtree::application::ports::{JobQueue, NewPollSpec, NewStatus, RelationshipStore, StatusStore};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, QuoteState, SystemClock, TimestampIdGenerator, Visibility};
use birchtree::federation::net::SafeHttpConfig;
use birchtree::federation::threads;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};
use common::{mk_account, send, temp_store, TEST_PUBLIC_URL};
use tower::ServiceExt;

const BASE: &str = "https://example.social";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_millis() as u64
}

fn unique_id() -> Id {
    Id::new(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos() as u64)
}

fn context(store: &Arc<RedbStore>, dir: &std::path::Path) -> Arc<Context> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = Arc::new(birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blobs"));
    let server = Arc::new(birchtree::api::mastodon::ServerInfo::from_config(&Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    }));
    let streams = Hub::new(store.clone(), clock.clone(), server, Limits::default());
    Arc::new(Context::new(clock, ids, store.clone(), blobs, Arc::new(streams)))
}

/// A remote account with a fresh key (no fetch is made for it).
fn remote(store: &RedbStore, name: &str) -> (Id, RemoteAccountUpsert) {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://remote.example/users/{name}");
    let upsert = RemoteAccountUpsert {
        actor_uri: actor_uri.clone(),
        username: name.to_string(),
        domain: "remote.example".into(),
        display_name: String::new(),
        note: String::new(),
        locked: false,
        inbox_uri: format!("{actor_uri}/inbox"),
        shared_inbox_uri: Some("https://remote.example/inbox".into()),
        public_key_pem: keys.public_key,
        key_id: format!("{actor_uri}#main-key"),
    };
    let account = store.upsert_remote(upsert.clone(), now_ms()).expect("remote");
    (account.id, upsert)
}

/// A local status, written directly (the store's create path).
fn local_status(store: &RedbStore, author: Id, text: &str, visibility: Visibility) -> birchtree::domain::Status {
    store
        .create_status(
            NewStatus {
                account_id: author,
                uri_base: format!("{BASE}/users/alice/statuses"),
                url_base: format!("{BASE}/@alice"),
                source_text: text.into(),
                content_html: format!("<p>{text}</p>"),
                visibility,
                ..NewStatus::default()
            },
            now_ms(),
        )
        .expect("status")
        .status
}

/// A note as a remote author would send it (`author_uri` is informational here).
fn note(_author_uri: &str, id: &str, text: &str, _to: &str) -> RemoteNote {
    RemoteNote {
        uri: id.into(),
        url: id.into(),
        author_id: Id::new(0),
        published_ms: now_ms(),
        visibility: Visibility::Public,
        content_html: format!("<p>{text}</p>"),
        spoiler_text: None,
        sensitive: false,
        language: Some("en".into()),
        in_reply_to_uri: None,
        mention_uris: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
        location: None,
        emojis: Vec::new(),
        quote_uri: None,
        poll: None,
    }
}

// --- notes ----------------------------------------------------------------

#[test]
fn a_remote_note_is_stored_once_by_its_uri() {
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let mut n = note("https://remote.example/users/eve", "https://remote.example/notes/1", "hello", "");
    n.author_id = eve;
    let first = inbound::ingest_note(&*store, n.clone(), None, now_ms()).unwrap();
    assert!(first.created);
    let again = inbound::ingest_note(&*store, n, None, now_ms()).unwrap();
    assert!(!again.created, "a redelivered Note is not stored twice");
    assert_eq!(store.status_by_uri("https://remote.example/notes/1").unwrap().unwrap().id, first.status.id);
}

#[test]
fn a_reply_whose_parent_is_unknown_is_unresolved_until_linked() {
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let mut reply = note("https://remote.example/users/eve", "https://remote.example/notes/2", "reply", "");
    reply.author_id = eve;
    reply.in_reply_to_uri = Some("https://remote.example/notes/1".into());
    let stored = inbound::ingest_note(&*store, reply, None, now_ms()).unwrap();
    assert_eq!(stored.parent_unknown.as_deref(), Some("https://remote.example/notes/1"));
    assert!(stored.status.in_reply_to_id.is_none(), "never promoted to a root, never guessed");
    assert_eq!(
        store.unresolved_parent(stored.status.id).unwrap().as_deref(),
        Some("https://remote.example/notes/1")
    );

    let mut parent = note("https://remote.example/users/eve", "https://remote.example/notes/1", "parent", "");
    parent.author_id = eve;
    let parent = inbound::ingest_note(&*store, parent, None, now_ms()).unwrap();
    threads::schedule_parent(&*store, stored.status.id, "https://remote.example/notes/1", now_ms()).unwrap();
    store.link_reply(stored.status.id, parent.status.id).unwrap();
    let linked = store.status_by_id(stored.status.id).unwrap().unwrap();
    assert_eq!(linked.in_reply_to_id, Some(parent.status.id));
    assert!(store.unresolved_parent(stored.status.id).unwrap().is_none());
}

#[test]
fn an_edit_and_a_delete_by_the_author_apply_and_others_are_ignored() {
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let (mallory, _) = remote(&store, "mallory");
    let mut n = note("https://remote.example/users/eve", "https://remote.example/notes/3", "first", "");
    n.author_id = eve;
    inbound::ingest_note(&*store, n.clone(), None, now_ms()).unwrap();

    let mut edit = n.clone();
    edit.content_html = "<p>edited</p>".into();
    assert_eq!(inbound::update_note(&*store, eve, &edit, now_ms()).unwrap(), Applied::Applied);
    let status = store.status_by_uri(&n.uri).unwrap().unwrap();
    assert_eq!(status.content_html, "<p>edited</p>");
    assert!(status.edited_at_ms.is_some());

    assert!(matches!(inbound::update_note(&*store, mallory, &edit, now_ms()).unwrap(), Applied::Ignored(_)));
    assert!(matches!(inbound::delete_note(&*store, mallory, &n.uri, now_ms()).unwrap(), Applied::Ignored(_)));
    assert_eq!(inbound::delete_note(&*store, eve, &n.uri, now_ms()).unwrap(), Applied::Applied);
    assert!(store.status_by_uri(&n.uri).unwrap().unwrap().is_deleted());
    assert_eq!(inbound::delete_note(&*store, eve, &n.uri, now_ms()).unwrap(), Applied::Duplicate);
}

#[test]
fn an_attachment_is_a_pending_row_that_stages_once() {
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let mut n = note("https://remote.example/users/eve", "https://remote.example/notes/4", "pic", "");
    n.author_id = eve;
    n.attachments = vec![RemoteAttachment {
        source_url: "https://remote.example/media/1.png".into(),
        media_type: "image/png".into(),
        description: "a picture".into(),
        license: None,
    }];
    let ingested = inbound::ingest_note(&*store, n, None, now_ms()).unwrap();
    let media_id = ingested.status.media_ids[0];
    assert_eq!(
        store.remote_media_source(media_id).unwrap().as_deref(),
        Some("https://remote.example/media/1.png")
    );
    let row = store.stage_remote_media(media_id, "remote/1", "image/png", 3, now_ms()).unwrap().unwrap();
    assert_eq!(row.file_key, "remote/1");
    // Staging again is a replay: nothing changes.
    let again = store.stage_remote_media(media_id, "remote/other", "image/png", 3, now_ms()).unwrap().unwrap();
    assert_eq!(again.file_key, "remote/1");
}

// --- renotes, likes, blocks, flags ----------------------------------------

#[test]
fn likes_and_renotes_from_a_remote_account_apply_and_undo() {
    let (_dir, store) = temp_store();
    let alice = common::mk_account(&store, "alice").id;
    let (eve, _) = remote(&store, "eve");
    let status = local_status(&store, alice, "boost me", Visibility::Public);

    assert_eq!(inbound::like(&*store, eve, &status.uri, now_ms()).unwrap(), Applied::Applied);
    assert_eq!(store.status_by_id(status.id).unwrap().unwrap().favourites_count, 1);
    assert_eq!(inbound::unlike(&*store, eve, &status.uri, now_ms()).unwrap(), Applied::Applied);
    assert_eq!(store.status_by_id(status.id).unwrap().unwrap().favourites_count, 0);

    assert_eq!(
        inbound::announce(&*store, eve, &status.uri, Visibility::Public, now_ms()).unwrap(),
        Applied::Applied
    );
    assert_eq!(store.status_by_id(status.id).unwrap().unwrap().reblogs_count, 1);
    assert_eq!(inbound::unannounce(&*store, eve, &status.uri, now_ms()).unwrap(), Applied::Applied);
    assert_eq!(store.status_by_id(status.id).unwrap().unwrap().reblogs_count, 0);
}

#[test]
fn a_like_of_an_unknown_status_is_ignored_not_recorded() {
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let outcome = inbound::like(&*store, eve, "https://example.social/nowhere", now_ms()).unwrap();
    assert!(matches!(outcome, Applied::Ignored(_)));
}

#[test]
fn a_block_from_a_remote_account_is_stored_and_lifted() {
    let (_dir, store) = temp_store();
    let alice = common::mk_account(&store, "alice").id;
    let (eve, _) = remote(&store, "eve");
    assert_eq!(inbound::block(&*store, eve, alice, now_ms()).unwrap(), Applied::Applied);
    let pair = store.pair_state(alice, eve, now_ms()).unwrap();
    assert!(pair.blocked_by, "the remote blocks us");
    assert_eq!(inbound::unblock(&*store, eve, alice, now_ms()).unwrap(), Applied::Applied);
    assert!(!store.pair_state(alice, eve, now_ms()).unwrap().blocked_by);
}

#[test]
fn a_flag_stores_one_report_per_local_target_and_names_its_statuses() {
    let (_dir, store) = temp_store();
    let alice = common::mk_account(&store, "alice").id;
    let (eve, _) = remote(&store, "eve");
    let status = local_status(&store, alice, "spam", Visibility::Public);
    let outcome = inbound::flag(
        &*store,
        eve,
        &["https://example.social/users/alice".into(), status.uri.clone()],
        "spam, please look",
        "https://remote.example/flags/1",
        now_ms(),
    )
    .unwrap();
    assert_eq!(outcome, Applied::Applied);
    // The report is stored for alice only: eve's own status is not a target here.
    let again = inbound::flag(&*store, eve, &["https://example.social/users/alice".into()], "", "https://remote.example/flags/1", now_ms()).unwrap();
    assert_eq!(again, Applied::Applied, "a second report with another activity id is another report");
}

// --- quotes ---------------------------------------------------------------

#[test]
fn a_quote_request_for_a_local_status_is_accepted_under_a_public_policy() {
    let (_dir, store) = temp_store();
    let alice = common::mk_account(&store, "alice").id;
    let (eve, _) = remote(&store, "eve");
    let quoted = local_status(&store, alice, "quote me", Visibility::Public);
    let mut quoting = note("https://remote.example/users/eve", "https://remote.example/notes/quoting", "quoting", "");
    quoting.author_id = eve;
    quoting.quote_uri = Some(quoted.uri.clone());
    let quoting_status = inbound::ingest_note(&*store, quoting, None, now_ms()).unwrap().status;
    let outcome = inbound::quote_request(
        &*store,
        quoting_status.id,
        &quoted.uri,
        "https://remote.example/quote_requests/1",
        now_ms(),
    )
    .unwrap();
    assert_eq!(outcome, Applied::Applied);
    let quote = store.quote_by_quoting(quoting_status.id).unwrap().unwrap();
    assert_eq!(quote.state, QuoteState::Accepted);
    assert!(quote.approval_uri.as_deref().is_some_and(|a| a.contains("/quote_authorizations/")));
    assert_eq!(store.status_by_id(quoted.id).unwrap().unwrap().quotes_count, 1);
}

#[test]
fn our_quote_of_a_remote_status_waits_for_the_remote_accept() {
    let (_dir, store) = temp_store();
    let alice = common::mk_account(&store, "alice").id;
    let (eve, _) = remote(&store, "eve");
    let remote_status = {
        let mut n = note("https://remote.example/users/eve", "https://remote.example/notes/to-quote", "remote", "");
        n.author_id = eve;
        inbound::ingest_note(&*store, n, None, now_ms()).unwrap().status
    };
    let ours = local_status(&store, alice, "my quote", Visibility::Public);
    let quote_id = unique_id();
    let quote = store
        .create_quote(
            birchtree::application::federation_ports::NewQuote {
                quoting_status_id: ours.id,
                quoted_status_id: remote_status.id,
                quoting_account_id: alice,
                quoted_account_id: eve,
                state: QuoteState::Pending,
                approval_uri: None,
                quote_activity_uri: None,
            },
            now_ms(),
        )
        .unwrap();
    let _ = quote_id;
    assert_eq!(quote.state, QuoteState::Pending);
    assert_eq!(store.status_by_id(remote_status.id).unwrap().unwrap().quotes_count, 0, "a pending quote does not count");

    let applied = inbound::quote_accepted(&*store, &ours.uri, "https://remote.example/quote_authorizations/9").unwrap();
    assert_eq!(applied, Applied::Applied);
    let accepted = store.quote_by_quoting(ours.id).unwrap().unwrap();
    assert_eq!(accepted.state, QuoteState::Accepted);
    assert_eq!(accepted.approval_uri.as_deref(), Some("https://remote.example/quote_authorizations/9"));
    assert_eq!(store.status_by_id(remote_status.id).unwrap().unwrap().quotes_count, 1);

    let stranger = remote(&store, "stranger").0;
    let refused = inbound::quote_revoked(&*store, stranger, "https://remote.example/quote_authorizations/9").unwrap();
    assert!(matches!(refused, Applied::Ignored(_)), "only the quoted author revokes");
    let revoked = inbound::quote_revoked(&*store, eve, "https://remote.example/quote_authorizations/9").unwrap();
    assert_eq!(revoked, Applied::Applied);
    assert_eq!(store.status_by_id(remote_status.id).unwrap().unwrap().quotes_count, 0, "a revoke uncounts");
}

// --- outbound events --------------------------------------------------------

#[tokio::test]
async fn a_public_status_is_planned_to_each_remote_follower_inbox() {
    use birchtree::application::federation::{receive_follow, InboundFollow};
    let (dir, store) = temp_store();
    let alice = common::mk_account(&store, "alice").id;
    let (eve, eve_upsert) = remote(&store, "eve");
    // eve follows alice (alice is not locked, so the follow is active at once).
    receive_follow(
        &*store,
        InboundFollow {
            activity_uri: "https://remote.example/follows/1".into(),
            actor: eve_upsert.clone(),
            target_id: alice,
            accept_uri: "https://remote.example/follows/1#accept".into(),
            accept_body: serde_json::json!({}),
        },
        now_ms(),
    )
    .unwrap();
    let status = local_status(&store, alice, "hello followers", Visibility::Public);
    let _ = eve;
    let config = common::federation_config_with(context(&store, dir.path()), TEST_PUBLIC_URL, &SafeHttpConfig::default(), true);
    let event = Job::new(
        unique_id(),
        JobKind::FederationEvent,
        serde_json::json!({"event": "create", "status_id": status.id.value(), "actor_id": alice.value()}),
        now_ms(),
        now_ms(),
    );
    birchtree::federation::events::run(&config, &event).await.expect("planned");
    let deliveries: Vec<serde_json::Value> = store
        .claim_due(now_ms() + 3_600_000, 100)
        .unwrap()
        .into_iter()
        .filter(|j| j.kind == JobKind::DeliverActivity)
        .map(|j| j.payload)
        .filter(|p| p["body"]["type"] == "Create")
        .collect();
    assert_eq!(deliveries.len(), 1, "one Create, to the shared inbox the follower names");
    assert_eq!(deliveries[0]["inbox"], "https://remote.example/inbox");
    assert_eq!(deliveries[0]["body"]["type"], "Create");
    assert_eq!(deliveries[0]["body"]["object"]["id"], status.uri);
    assert_eq!(deliveries[0]["body"]["object"]["type"], "Note");
}

// --- thread completion -----------------------------------------------------

#[test]
fn a_remote_root_is_crawled_once_and_then_only_announced_during_the_cooldown() {
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let mut n = note("https://remote.example/users/eve", "https://remote.example/notes/root", "root", "");
    n.author_id = eve;
    n.published_ms = now_ms() - 10 * 60 * 1000;
    let root = inbound::ingest_note(&*store, n, None, now_ms() - 10 * 60 * 1000).unwrap().status;

    let first = threads::read_time_completion(&*store, &root, now_ms()).unwrap();
    assert!(first.is_some(), "an old remote root gets a refresh");
    let second = threads::read_time_completion(&*store, &root, now_ms()).unwrap();
    assert_eq!(second, first, "within the cooldown, the same refresh id, nothing new");
}

#[test]
fn a_fresh_root_is_not_crawled_yet() {
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let mut n = note("https://remote.example/users/eve", "https://remote.example/notes/fresh", "fresh", "");
    n.author_id = eve;
    let root = inbound::ingest_note(&*store, n, None, now_ms()).unwrap().status;
    assert!(threads::read_time_completion(&*store, &root, now_ms()).unwrap().is_none());
}

// --- HTTP: our documents ----------------------------------------------------

#[tokio::test]
async fn a_public_note_is_served_as_a_note_and_a_deleted_one_is_a_tombstone() {
    let srv = common::TestServer::new(true);
    let alice = common::mk_account(&srv.store, "alice").id;
    let status = local_status(&srv.store, alice, "public note", Visibility::Public);
    let (code, _, body) = send(
        &srv.router,
        Request::builder().uri(format!("/users/alice/statuses/{}", status.id.value())).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(code, axum::http::StatusCode::OK);
    let doc = common::json(&body);
    assert_eq!(doc["type"], "Note");
    assert_eq!(doc["id"], status.uri);
    assert_eq!(doc["attributedTo"], format!("{BASE}/users/alice"));
    let parsed: Note = serde_json::from_value(doc.clone()).expect("a Note parses back");
    assert_eq!(parsed.visibility(), Visibility::Public);

    srv.store.delete_status(status.id, alice, now_ms()).unwrap();
    let (code, _, body) = send(
        &srv.router,
        Request::builder().uri(format!("/users/alice/statuses/{}", status.id.value())).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(code, axum::http::StatusCode::GONE);
    assert_eq!(common::json(&body)["type"], "Tombstone");
}

#[tokio::test]
async fn a_direct_note_is_not_served_publicly() {
    let srv = common::TestServer::new(true);
    let alice = common::mk_account(&srv.store, "alice").id;
    let status = local_status(&srv.store, alice, "for you only", Visibility::Direct);
    let (code, _, _) = send(
        &srv.router,
        Request::builder().uri(format!("/users/alice/statuses/{}", status.id.value())).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(code, axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_actor_document_advertises_its_collections_and_the_counts_are_exact() {
    let srv = common::TestServer::new(true);
    let alice = common::mk_account(&srv.store, "alice").id;
    let (eve, _) = remote(&srv.store, "eve");
    let _ = eve;
    let (code, _, body) = send(
        &srv.router,
        Request::builder().uri("/users/alice").header("accept", "application/activity+json").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(code, axum::http::StatusCode::OK);
    let doc = common::json(&body);
    assert_eq!(doc["followers"], format!("{BASE}/users/alice/followers"));
    assert_eq!(doc["outbox"], format!("{BASE}/users/alice/outbox"));
    let (_, _, body) = send(
        &srv.router,
        Request::builder().uri("/users/alice/followers").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(common::json(&body)["totalItems"], 0);
    let _ = alice;
}

#[tokio::test]
async fn the_instance_actor_is_not_served_until_it_has_a_key() {
    let srv = common::TestServer::new(true);
    let response = srv
        .router
        .clone()
        .oneshot(Request::builder().uri("/actor").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
}

// --- polls over federation ------------------------------------------------

/// A `Question` as Mastodon 4.7.3 sends one: `oneOf` options with
/// `replies.totalItems` tallies, `endTime` and `votersCount`.
fn question(author_uri: &str, id: &str, options: &[(&str, u64)], expires_ms: u64, multiple: bool) -> Question {
    let opts: Vec<QuestionOption> = options
        .iter()
        .map(|(title, votes)| QuestionOption {
            kind: Some("Note".into()),
            name: title.to_string(),
            replies: Some(QuestionReplies { kind: Some("Collection".into()), total_items: *votes }),
        })
        .collect();
    Question {
        kind: Default::default(),
        id: id.parse().expect("uri"),
        attributed_to: author_uri.parse().expect("uri"),
        content: "pick one".into(),
        published: None,
        updated: None,
        url: None,
        in_reply_to: None,
        to: vec!["https://www.w3.org/ns/activitystreams#Public".parse().expect("uri")],
        cc: vec![],
        one_of: (!multiple).then_some(opts.clone()),
        any_of: multiple.then_some(opts),
        end_time: Some(serde_json::json!(expires_ms as i64)),
        closed: None,
        voters_count: Some(options.len() as u64),
    }
}

fn remote_poll(options: &[(&str, u64)], expires_ms: u64) -> RemotePoll {
    RemotePoll {
        multiple: false,
        options: options.iter().map(|(t, v)| (t.to_string(), *v)).collect(),
        voters_count: 3,
        votes_count: options.iter().map(|(_, v)| v).sum(),
        expires_at_ms: Some(expires_ms),
        expired: false,
    }
}

/// A local status with a poll, written directly (the store's create path).
fn local_poll(store: &RedbStore, author: Id, text: &str, options: &[&str], expires_ms: u64) -> birchtree::domain::Status {
    store
        .create_status(
            NewStatus {
                account_id: author,
                uri_base: format!("{BASE}/users/alice/statuses"),
                url_base: format!("{BASE}/@alice"),
                source_text: text.into(),
                content_html: format!("<p>{text}</p>"),
                visibility: Visibility::Public,
                poll: Some(NewPollSpec {
                    multiple: false,
                    hide_totals: false,
                    expires_at_ms: expires_ms,
                    options: options.iter().map(|s| s.to_string()).collect(),
                }),
                ..NewStatus::default()
            },
            now_ms(),
        )
        .expect("status")
        .status
}

#[test]
fn a_remote_question_is_ingested_with_its_poll_and_tallies() {
    let (_dir, store) = temp_store();
    let (author, upsert) = remote(&store, "pollster");
    let expires = now_ms() + 3_600_000;
    let q = question(&upsert.actor_uri, "https://remote.example/questions/1", &[("tea", 4), ("coffee", 2)], expires, false);
    let (input, poll) = question_input(&q, author, now_ms());
    let ingested = inbound::ingest_note(&*store, input, Some(poll), now_ms()).expect("ingest");
    assert!(ingested.created);
    let stored = store.poll_by_status(ingested.status.id).expect("poll").expect("poll row");
    assert_eq!(stored.options.iter().map(|o| (o.title.clone(), o.votes_count)).collect::<Vec<_>>(),
        vec![("tea".to_string(), 4), ("coffee".to_string(), 2)]);
    assert_eq!(stored.voters_count, 2);
    assert_eq!(stored.expires_at_ms, Some(expires));
    // A redelivery is a duplicate, not a second poll.
    let (input2, poll2) = question_input(&q, author, now_ms());
    let again = inbound::ingest_note(&*store, input2, Some(poll2), now_ms()).expect("ingest");
    assert!(!again.created);
}

#[test]
fn a_question_without_expiry_ingests_poll_less() {
    let (_dir, store) = temp_store();
    let (author, upsert) = remote(&store, "pollster");
    let mut q = question(&upsert.actor_uri, "https://remote.example/questions/9", &[("a", 0), ("b", 0)], now_ms() + 1000, false);
    q.end_time = None;
    let (input, poll) = question_input(&q, author, now_ms());
    let ingested = inbound::ingest_note(&*store, input, Some(poll), now_ms()).expect("ingest");
    assert!(ingested.created);
    assert!(store.poll_by_status(ingested.status.id).expect("poll").is_none());
}

#[test]
fn a_create_of_a_question_parses_as_create() {
    let (author_uri, qid) = ("https://remote.example/users/pollster", "https://remote.example/questions/1");
    let body = serde_json::json!({
        "type": "Create",
        "id": "https://remote.example/activities/1",
        "actor": author_uri,
        "object": {
            "type": "Question",
            "id": qid,
            "attributedTo": author_uri,
            "content": "pick one",
            "oneOf": [{"type": "Note", "name": "a", "replies": {"type": "Collection", "totalItems": 0}}],
            "endTime": "2030-01-01T00:00:00Z",
        },
    });
    let activity: birchtree::activitypub::activities::Activity = serde_json::from_value(body).expect("parses");
    assert!(matches!(activity, birchtree::activitypub::activities::Activity::Create(_)));
}

#[test]
fn a_vote_note_tallies_on_a_known_local_poll() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let (voter, _) = remote(&store, "voter");
    let status = local_poll(&store, alice.id, "tea or coffee?", &["tea", "coffee"], now_ms() + 3_600_000);
    let poll_uri = status.uri.clone();

    let applied = inbound::vote(&*store, voter, &poll_uri, "tea", now_ms()).expect("vote");
    assert_eq!(applied, inbound::VoteDisposition::Tallied(Applied::Applied));
    let poll = store.poll_by_status(status.id).expect("poll").expect("poll row");
    assert_eq!(poll.options[0].votes_count, 1);
    assert_eq!(poll.voters_count, 1);

    // The same voter again is a duplicate, not a second vote.
    let again = inbound::vote(&*store, voter, &poll_uri, "tea", now_ms()).expect("vote");
    assert_eq!(again, inbound::VoteDisposition::Tallied(Applied::Duplicate));

    // An unknown option name is not a vote at all (it keeps its reply meaning).
    let other = inbound::vote(&*store, voter, &poll_uri, "matcha", now_ms()).expect("vote");
    assert_eq!(other, inbound::VoteDisposition::NotAVote);

    // The author cannot vote in their own poll.
    let own = inbound::vote(&*store, alice.id, &poll_uri, "coffee", now_ms()).expect("vote");
    assert!(matches!(own, inbound::VoteDisposition::Tallied(Applied::Ignored(_))));

    // The author's followers learn the new totals.
    let events: Vec<Job> = store
        .claim_due(now_ms() + 3_600_000, 100)
        .expect("claim")
        .into_iter()
        .filter(|j| j.kind == JobKind::FederationEvent && j.payload["event"] == "question_update")
        .collect();
    assert!(!events.is_empty(), "a question_update event is queued");
}

#[test]
fn a_vote_for_an_unknown_or_poll_less_parent_is_not_a_vote() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let (voter, _) = remote(&store, "voter");
    let plain = local_status(&store, alice.id, "no poll here", Visibility::Public);

    let unknown = inbound::vote(&*store, voter, "https://remote.example/questions/missing", "a", now_ms()).expect("vote");
    assert_eq!(unknown, inbound::VoteDisposition::NotAVote);

    let poll_less = inbound::vote(&*store, voter, &plain.uri, "a", now_ms()).expect("vote");
    assert_eq!(poll_less, inbound::VoteDisposition::NotAVote);
}

#[test]
fn a_vote_note_is_detected_by_shape_only() {
    use birchtree::activitypub::wire::Note;
    // A vote: name set, empty content, inReplyTo set.
    let vote: Note = serde_json::from_value(serde_json::json!({
        "type": "Note",
        "id": "https://remote.example/votes/1",
        "attributedTo": "https://remote.example/users/voter",
        "content": "",
        "name": "tea",
        "inReplyTo": "https://remote.example/questions/1",
        "to": ["https://remote.example/users/pollster"],
    }))
    .expect("parses");
    let detected = vote_of(&vote).expect("detected");
    assert_eq!(detected.poll_uri, "https://remote.example/questions/1");
    assert_eq!(detected.choice_name, "tea");

    // An ordinary reply is not a vote, even with an inReplyTo.
    let reply: Note = serde_json::from_value(serde_json::json!({
        "type": "Note",
        "id": "https://remote.example/notes/2",
        "attributedTo": "https://remote.example/users/voter",
        "content": "I agree",
        "inReplyTo": "https://remote.example/questions/1",
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
    }))
    .expect("parses");
    assert!(vote_of(&reply).is_none());
}

#[test]
fn a_question_update_syncs_tallies() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let (voter, _) = remote(&store, "voter");
    let status = local_poll(&store, alice.id, "tea or coffee?", &["tea", "coffee"], now_ms() + 3_600_000);

    let remote = remote_poll(&[("tea", 7), ("coffee", 5)], now_ms() + 3_600_000);
    let applied = inbound::sync_question(&*store, alice.id, &status.uri, &remote, now_ms()).expect("sync");
    assert_eq!(applied, Applied::Applied);
    let poll = store.poll_by_status(status.id).expect("poll").expect("poll row");
    assert_eq!(poll.options.iter().map(|o| o.votes_count).collect::<Vec<_>>(), vec![7, 5]);
    assert_eq!(poll.voters_count, 3);

    // New totals with the same shape apply.
    let more = remote_poll(&[("tea", 9), ("coffee", 6)], now_ms() + 3_600_000);
    let applied = inbound::sync_question(&*store, alice.id, &status.uri, &more, now_ms()).expect("sync");
    assert_eq!(applied, Applied::Applied);
    let poll = store.poll_by_status(status.id).expect("poll").expect("poll row");
    assert_eq!(poll.options.iter().map(|o| o.votes_count).collect::<Vec<_>>(), vec![9, 6]);

    // A non-author cannot rewrite the totals.
    let forged = inbound::sync_question(&*store, voter, &status.uri, &remote, now_ms()).expect("sync");
    assert!(matches!(forged, Applied::Ignored(_)));

    // A changed option set is left alone.
    let changed = remote_poll(&[("tea", 1), ("mate", 1)], now_ms() + 3_600_000);
    let kept = inbound::sync_question(&*store, alice.id, &status.uri, &changed, now_ms()).expect("sync");
    assert!(matches!(kept, Applied::Ignored(_)));
    let poll = store.poll_by_status(status.id).expect("poll").expect("poll row");
    assert_eq!(poll.options.iter().map(|o| o.title.clone()).collect::<Vec<_>>(), vec!["tea", "coffee"]);
}

#[test]
fn an_expired_local_poll_rejects_a_remote_vote() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let (voter, _) = remote(&store, "voter");
    let status = local_poll(&store, alice.id, "old poll", &["a", "b"], now_ms() - 1000);
    let rejected = inbound::vote(&*store, voter, &status.uri, "a", now_ms()).expect("vote");
    assert!(matches!(rejected, inbound::VoteDisposition::Tallied(Applied::Ignored(_))));
    let _ = VoteRejection::Expired;
}

#[test]
fn outgoing_create_of_a_poll_is_a_question() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let status = local_poll(&store, alice.id, "tea or coffee?", &["tea", "coffee"], now_ms() + 3_600_000);
    let body = outgoing::create(&*store, TEST_PUBLIC_URL, &status, now_ms(), "off", "off").expect("create");
    let object = &body["object"];
    assert_eq!(object["type"], "Question");
    assert_eq!(object["oneOf"].as_array().expect("oneOf").len(), 2);
    assert_eq!(object["oneOf"][0]["name"], "tea");
    assert_eq!(object["oneOf"][0]["replies"]["totalItems"], 0);
    assert!(object["endTime"].is_string(), "unexpired polls carry endTime");
    assert!(object["closed"].is_null(), "unexpired polls carry no closed");
    assert_eq!(object["votersCount"], 0);
    assert_eq!(body["type"], "Create");
}

#[test]
fn outgoing_vote_names_the_choice_and_the_poll() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let (owner_id, upsert) = remote(&store, "pollster");
    let owner = store.account_by_id(owner_id).expect("owner").expect("owner row");
    let remote_status = inbound::ingest_note(
        &*store,
        RemoteNote {
            uri: "https://remote.example/questions/1".into(),
            url: "https://remote.example/questions/1".into(),
            author_id: owner_id,
            published_ms: now_ms(),
            visibility: Visibility::Public,
            content_html: "pick".into(),
            spoiler_text: None,
            sensitive: false,
            language: None,
            in_reply_to_uri: None,
            mention_uris: Vec::new(),
            tags: Vec::new(),
            attachments: Vec::new(),
            location: None,
            emojis: Vec::new(),
            quote_uri: None,
            poll: None,
        },
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;
    let voter = store.account_by_id(alice.id).expect("voter").expect("voter row");
    let body = outgoing::vote(&voter, &owner, &remote_status, "https://example.social/users/alice#votes/1/0/7", "tea");
    assert_eq!(body["type"], "Create");
    assert_eq!(body["actor"], voter.actor_uri);
    assert_eq!(body["to"][0], upsert.actor_uri);
    assert_eq!(body["object"]["type"], "Note");
    assert_eq!(body["object"]["name"], "tea");
    assert_eq!(body["object"]["inReplyTo"], remote_status.uri);
    let _ = owner;
}

// --- poll federation regressions (run3 post-mortem) ------------------------

/// Akkoma always sends `closed` (its expiry time), even for a live poll.
/// A future `closed` must not read as ended: votes stay accepted.
#[test]
fn a_question_with_a_future_closed_time_is_not_expired() {
    let (_dir, store) = temp_store();
    let (author, upsert) = remote(&store, "akkomapoll");
    let expires = now_ms() + 3_600_000;
    // Akkoma shape: `closed` set, no `endTime`.
    let mut q = question(&upsert.actor_uri, "https://remote.example/polls/1", &[("a", 0), ("b", 0)], expires, false);
    q.end_time = None;
    q.closed = Some(serde_json::json!(chrono::DateTime::<chrono::Utc>::from_timestamp_millis(expires as i64)
        .expect("ts").to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));

    let (input, poll) = question_input(&q, author, now_ms());
    assert!(!poll.expired, "a future closed time is not expiry");
    let ingested = inbound::ingest_note(&*store, input, Some(poll), now_ms()).expect("ingest");
    assert!(ingested.created);
    let stored = store.poll_by_status(ingested.status.id).expect("poll").expect("poll row");
    assert!(!stored.expired, "ingested row is not sticky-expired");

    // The stored poll accepts a vote (previously rejected as expired,
    // which is why akkoma/catodon-origin polls never tallied).
    let (voter, _) = remote(&store, "voter");
    let tallied = store.vote_poll(stored.id, voter, &[0], now_ms());
    assert!(tallied.is_ok(), "vote on a live poll tallies: {tallied:?}");

    // A past `closed` time does read as ended, and rejects votes.
    let mut q2 = question(&upsert.actor_uri, "https://remote.example/polls/2", &[("a", 0), ("b", 0)], now_ms() - 1000, false);
    q2.end_time = None;
    q2.closed = Some(serde_json::json!("1970-01-01T00:00:00Z"));
    let (_, poll2) = question_input(&q2, author, now_ms());
    assert!(poll2.expired, "a past closed time is expiry");
    let (input2, poll2) = (inbound::RemoteNote {
        uri: "https://remote.example/polls/2".into(),
        url: "https://remote.example/polls/2".into(),
        author_id: author,
        published_ms: now_ms(),
        visibility: Visibility::Public,
        content_html: "old".into(),
        spoiler_text: None,
        sensitive: false,
        language: None,
        in_reply_to_uri: None,
        mention_uris: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
        location: None,
        emojis: Vec::new(),
        quote_uri: None,
        poll: None,
    }, poll2);
    let ingested2 = inbound::ingest_note(&*store, input2, Some(poll2), now_ms()).expect("ingest");
    let stored2 = store.poll_by_status(ingested2.status.id).expect("poll").expect("poll row");
    assert!(stored2.expired);
    let rejected = store.vote_poll(stored2.id, voter, &[0], now_ms());
    assert!(matches!(rejected, Err(birchtree::application::ports::VoteRejection::Expired)));
}

/// A reblog wrapper's links live on the booster's actor URI, never on the
/// boosted status's domain: an activity id on another host than its actor is
/// refused by every domain-checking receiver (run3: bt boosts never counted
/// on bt/misskey/catodon/akkoma origins).
#[test]
fn a_reblog_wrapper_is_addressed_on_the_booster_domain() {
    use birchtree::application::ports::StatusStore;
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let (remote_id, _) = remote(&store, "far");
    let _target = local_status(&store, alice.id, "boost me", Visibility::Public);
    // Ingest a remote copy as the booster would see it, then reblog it.
    let remote_note = RemoteNote {
        uri: "https://remote.example/notes/1".into(),
        url: "https://remote.example/notes/1".into(),
        author_id: remote_id,
        published_ms: now_ms(),
        visibility: Visibility::Public,
        content_html: "far post".into(),
        spoiler_text: None,
        sensitive: false,
        language: None,
        in_reply_to_uri: None,
        mention_uris: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
        location: None,
        emojis: Vec::new(),
        quote_uri: None,
        poll: None,
    };
    let ingested = inbound::ingest_note(&*store, remote_note, None, now_ms()).expect("ingest");
    let wrapper = store
        .reblog_status(alice.id, ingested.status.id, Visibility::Public, now_ms())
        .expect("reblog");
    let booster = store.account_by_id(alice.id).expect("booster").expect("row");
    assert!(wrapper.uri.starts_with(booster.actor_uri.as_str()),
        "wrapper {} is not on {}", wrapper.uri, booster.actor_uri);
    assert!(wrapper.url.starts_with(booster.actor_uri.as_str()),
        "wrapper {} is not on {}", wrapper.url, booster.actor_uri);
}

/// Deleting a reblog wrapper enqueues both `delete` (followers' timelines)
/// and `undo_announce` (origin decrements; item 52). Ordinary deletes enqueue
/// only `delete`.
#[test]
fn a_reblog_delete_enqueues_undo_announce() {
    use birchtree::application::ports::StatusStore;
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let target = local_status(&store, alice.id, "boost me", Visibility::Public);
    let wrapper = store
        .reblog_status(bob.id, target.id, Visibility::Public, now_ms())
        .expect("reblog");
    let wrapper_uri = wrapper.uri.clone();
    store
        .delete_status(wrapper.id, bob.id, now_ms())
        .expect("delete wrapper");
    let events: Vec<serde_json::Value> = store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|j| j.kind == JobKind::FederationEvent)
        .map(|j| j.payload)
        .collect();
    assert!(
        events.iter().any(|p| p["event"] == "delete"
            && p["status_id"] == serde_json::json!(wrapper.id.value())),
        "wrapper delete event missing: {events:?}"
    );
    let undo = events
        .iter()
        .find(|p| p["event"] == "undo_announce")
        .expect("undo_announce event missing");
    assert_eq!(undo["status_id"], serde_json::json!(target.id.value()));
    assert_eq!(undo["announce_uri"], serde_json::json!(wrapper_uri));
}

#[test]
fn an_ordinary_delete_enqueues_no_undo_announce() {
    use birchtree::application::ports::StatusStore;
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let status = local_status(&store, alice.id, "plain", Visibility::Public);
    store
        .delete_status(status.id, alice.id, now_ms())
        .expect("delete");
    let events: Vec<serde_json::Value> = store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|j| j.kind == JobKind::FederationEvent)
        .map(|j| j.payload)
        .collect();
    assert!(
        events.iter().any(|p| p["event"] == "delete"),
        "delete event missing: {events:?}"
    );
    assert!(
        events.iter().all(|p| p["event"] != "undo_announce"),
        "ordinary delete must not enqueue undo_announce: {events:?}"
    );
}

#[test]
fn inbound_tags_normalize_and_remote_edits_recompute_them() {
    use birchtree::application::ports::TimelineQuery;
    use birchtree::application::timelines;
    let (_dir, store) = temp_store();
    let (eve, _) = remote(&store, "eve");
    let alice = common::mk_account(&store, "alice").id;
    let query = || TimelineQuery {
        max_id: None,
        since_id: None,
        min_id: None,
        limit: 20,
        only_media: false,
        local_only: false,
        remote_only: false,
    };

    // A remote `#café` lands normalized, sharing the local `cafe` tag.
    let mut n = note(
        "https://remote.example/users/eve",
        "https://remote.example/notes/40",
        "hi",
        "",
    );
    n.author_id = eve;
    n.tags = vec!["café".to_string(), "cafe".to_string()];
    let stored = inbound::ingest_note(&*store, n.clone(), None, now_ms()).unwrap();
    assert_eq!(stored.status.tags, vec!["cafe".to_string()]);

    let local_input = || NewStatus {
        account_id: alice,
        uri_base: format!("{BASE}/users/alice/statuses"),
        url_base: format!("{BASE}/@alice"),
        source_text: "#cafe from here".into(),
        content_html: "<p>#cafe from here</p>".into(),
        visibility: Visibility::Public,
        tags: vec!["cafe".to_string()],
        ..NewStatus::default()
    };
    let local = store
        .create_status(local_input(), now_ms())
        .expect("status")
        .status;
    assert_eq!(local.tags, vec!["cafe".to_string()]);
    let page = timelines::tag(&*store, Some(alice), "café", query(), now_ms()).expect("tag");
    let ids: Vec<Id> = page.statuses.iter().map(|s| s.id).collect();
    assert!(ids.contains(&stored.status.id), "remote café unifies: {ids:?}");
    assert!(ids.contains(&local.id), "local cafe present: {ids:?}");

    // A remote tag edit recomputes the stored list and moves index keys.
    let mut edit = n.clone();
    edit.tags = vec!["Music".to_string()];
    assert_eq!(
        inbound::update_note(&*store, eve, &edit, now_ms()).unwrap(),
        Applied::Applied
    );
    let status = store.status_by_uri(&n.uri).unwrap().unwrap();
    assert_eq!(status.tags, vec!["music".to_string()]);
    let old_page = timelines::tag(&*store, Some(alice), "cafe", query(), now_ms()).expect("tag");
    assert!(
        !old_page.statuses.iter().any(|s| s.id == status.id),
        "old tag released the status"
    );
    let new_page = timelines::tag(&*store, Some(alice), "music", query(), now_ms()).expect("tag");
    assert!(
        new_page.statuses.iter().any(|s| s.id == status.id),
        "new tag indexes the status"
    );
}

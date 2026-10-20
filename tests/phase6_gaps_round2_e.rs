//! Subagent E (gap-closing round 2): per-destination delivery cooldown +
//! failure budget, Delete-beats-Create tombstones (both directions), and
//! response size + content validation for remote documents.
//!
//! Lab-Mastodon media flakiness is VERIFY-ONLY here (verdicts in
//! `docs/federation-contract.md`): no test, no code.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use activitypub_federation::config::{Data, FederationConfig};
use activitypub_federation::fetch::fetch_object_http;
use activitypub_federation::http_signatures::generate_actor_keypair;
use activitypub_federation::traits::ActivityHandler;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use birchtree::activitypub::activities::{Activity, Create, Delete};
use birchtree::activitypub::FedContext;
use birchtree::application::context::Context;
use birchtree::application::federation_inbound::{self as inbound, Applied, RemoteNote};
use birchtree::application::federation_ports::{FederationContentStore, FederationStore};
use birchtree::application::ports::{JobQueue, StatusStore};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator, Visibility};
use birchtree::federation::delivery::{self, DeliveryError, DEST_FAILURE_BUDGET, DEST_MAX_DEFERRALS};
use birchtree::federation::net::SafeHttpConfig;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};
use common::{mk_account, temp_store, TEST_PUBLIC_URL};
use serde_json::json;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_millis() as u64
}

fn unique_job_id() -> Id {
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
fn remote(store: &RedbStore, name: &str) -> Id {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://remote.example/users/{name}");
    store
        .upsert_remote(
            birchtree::application::federation_ports::RemoteAccountUpsert {
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
            },
            now_ms(),
        )
        .expect("remote")
        .id
}

fn remote_note(author: Id, uri: &str, text: &str) -> RemoteNote {
    RemoteNote {
        uri: uri.into(),
        url: uri.into(),
        author_id: author,
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
        emojis: Vec::new(),
        quote_uri: None,
        poll: None,
    }
}

fn federation(
    store: &Arc<RedbStore>,
    dir: &std::path::Path,
    http: &SafeHttpConfig,
) -> FederationConfig<FedContext> {
    common::federation_config_with(context(store, dir), TEST_PUBLIC_URL, http, true)
}

/// Loopback is allowed by the operator allowlist, as a lab network is.
fn loopback_policy() -> SafeHttpConfig {
    SafeHttpConfig {
        allow_http: true,
        allowlist: ["127.0.0.0/8", "::1/128"]
            .iter()
            .map(|c| birchtree::federation::policy::Cidr::parse(c).unwrap())
            .collect(),
        ..SafeHttpConfig::default()
    }
}

fn delete_activity(actor: &str, activity_id: &str, object_uri: &str) -> Delete {
    serde_json::from_value(json!({
        "type": "Delete",
        "id": activity_id,
        "actor": actor,
        "object": object_uri,
    }))
    .expect("delete parses")
}

fn create_note_activity(actor: &str, activity_id: &str, object_uri: &str, text: &str) -> Create {
    serde_json::from_value(json!({
        "type": "Create",
        "id": activity_id,
        "actor": actor,
        "object": {
            "type": "Note",
            "id": object_uri,
            "attributedTo": actor,
            "content": format!("<p>{text}</p>"),
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
        },
    }))
    .expect("create parses")
}

// --- tombstones: inbound Delete before Create --------------------------------

#[tokio::test]
async fn a_delete_before_its_create_files_a_tombstone_and_the_create_stays_gone() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &SafeHttpConfig::default());
    let data = config.to_request_data();
    let eve = remote(&store, "eve");
    let actor = "https://remote.example/users/eve";
    let object = "https://remote.example/notes/ghost";

    // The Delete arrives first, for an object never seen here.
    let delete = delete_activity(actor, "https://remote.example/activities/del1", object);
    Activity::Delete(delete).receive(&data).await.expect("delete applies");
    assert!(store.status_by_uri(object).unwrap().is_none());
    assert!(store.remote_tombstone(object).unwrap().is_some(), "the Delete files a tombstone");
    let _ = eve;

    // The late Create is a duplicate, not a resurrection.
    let create = create_note_activity(actor, "https://remote.example/activities/c1", object, "late");
    Activity::Create(create).receive(&data).await.expect("create handled");
    assert!(store.status_by_uri(object).unwrap().is_none(), "the tombstoned URI stays gone");
    assert!(
        store.activity_recorded("https://remote.example/activities/c1").unwrap(),
        "the dropped Create is recorded, so a redelivery is a duplicate too"
    );
}

#[test]
fn a_delete_from_another_host_files_no_tombstone() {
    let (_dir, store) = temp_store();
    let _ = remote(&store, "eve");
    let object = "https://victim.example/notes/1";

    // Anyone could name any URI: only the object's own host may tombstone it.
    // (The inbox's `Delete` handler files the tombstone; here the eligibility
    // rule is exercised through the same helper.)
    let outcome = birchtree::federation::tombstone::file_remote_tombstone(
        &*store,
        object,
        &url::Url::parse("https://remote.example/users/eve").unwrap(),
        Applied::Duplicate,
        now_ms(),
    )
    .unwrap();
    assert!(matches!(outcome, Applied::Ignored(_)));
    assert!(store.remote_tombstone(object).unwrap().is_none(), "no cross-host tombstone");

    // ... so the real object still ingests once it arrives.
    let vic = remote(&store, "vic");
    let ingested = inbound::ingest_note(&*store, remote_note(vic, object, "real"), None, now_ms()).unwrap();
    assert!(ingested.created);
}

#[test]
fn an_update_of_a_tombstoned_uri_stays_gone() {
    let (_dir, store) = temp_store();
    let eve = remote(&store, "eve");
    let object = "https://remote.example/notes/ghost2";
    assert!(store.record_remote_tombstone(object, now_ms()).unwrap());

    let mut edit = remote_note(eve, object, "edited");
    edit.content_html = "<p>edited</p>".into();
    assert!(
        matches!(inbound::update_note(&*store, eve, &edit, now_ms()).unwrap(), Applied::Ignored(_)),
        "an edit cannot revive a tombstoned URI"
    );
    assert!(store.status_by_uri(object).unwrap().is_none());
}

// --- tombstones: outbound direction (our Delete wins) ------------------------

#[tokio::test]
async fn a_create_for_a_locally_deleted_status_stays_deleted() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &SafeHttpConfig::default());
    let data = config.to_request_data();
    let eve = remote(&store, "eve");
    let actor = "https://remote.example/users/eve";
    let object = "https://remote.example/notes/doomed";

    // Create, then Delete: the row tombstones but keeps its URI mapping.
    let create = create_note_activity(actor, "https://remote.example/activities/c2", object, "first");
    Activity::Create(create).receive(&data).await.expect("create applies");
    assert_eq!(inbound::delete_note(&*store, eve, object, now_ms()).unwrap(), Applied::Applied);
    assert!(store.status_by_uri(object).unwrap().unwrap().is_deleted());

    // A redelivered (or relayed) Create for the same URI is a duplicate.
    let create = create_note_activity(actor, "https://remote.example/activities/c3", object, "again");
    Activity::Create(create).receive(&data).await.expect("create handled");
    let row = store.status_by_uri(object).unwrap().unwrap();
    assert!(row.is_deleted(), "the Delete wins over the late Create");
    assert_eq!(row.content_html, "<p>first</p>", "the tombstoned content is untouched");
}

// --- delivery: per-destination cooldown + failure budget ----------------------

/// A local sender (`alice`) with a signing key.
fn sender_with_keys(store: &RedbStore) -> Id {
    let alice = mk_account(store, "alice").id;
    let pair = generate_actor_keypair().expect("keypair");
    let actor = format!("{TEST_PUBLIC_URL}/users/alice");
    store
        .store_local_keys(alice, &format!("{actor}#main-key"), &pair.public_key, &pair.private_key)
        .expect("keys");
    alice
}

fn follow_body(object: &str) -> serde_json::Value {
    serde_json::to_value(activitypub_federation::protocol::context::WithContext::new_default(
        Activity::Follow(serde_json::from_value(json!({
            "type": "Follow",
            "id": "https://example.social/users/alice/follows/1",
            "actor": format!("{TEST_PUBLIC_URL}/users/alice"),
            "object": object,
        })).unwrap()),
    ))
    .unwrap()
}

fn delivery_job(sender: Id, inbox: &str, body: serde_json::Value) -> Job {
    Job::new(
        unique_job_id(),
        JobKind::DeliverActivity,
        json!({"sender_id": sender.value(), "inbox": inbox, "activity_uri": "x", "body": body}),
        now_ms(),
        now_ms(),
    )
}

/// A loopback inbox answering every POST with `status`, counting hits.
async fn counting_inbox(status: u16, hits: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/inbox",
        post(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                StatusCode::from_u16(status).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://localhost:{port}/inbox")
}

fn dest_of(store: &RedbStore, inbox: &str) -> Option<birchtree::domain::DeliveryDestState> {
    let url = url::Url::parse(inbox).unwrap();
    store.delivery_dest(&delivery::dest_host(&url)).unwrap()
}

#[tokio::test]
async fn a_failing_host_cools_down_while_an_unrelated_delivery_is_unaffected() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &loopback_policy());
    let alice = sender_with_keys(&store);

    let failing_hits = Arc::new(AtomicUsize::new(0));
    let failing = counting_inbox(503, failing_hits.clone()).await;
    let ok_hits = Arc::new(AtomicUsize::new(0));
    let ok = counting_inbox(202, ok_hits.clone()).await;

    // First failure: retried, and the host is now on record with a cooldown.
    let job = delivery_job(alice, &failing, follow_body("https://remote.example/users/eve"));
    let outcome = delivery::run(&config, &job).await;
    assert!(matches!(outcome, Err(DeliveryError::Retry(_))), "{outcome:?}");
    assert_eq!(failing_hits.load(Ordering::SeqCst), 1);
    let state = dest_of(&store, &failing).expect("failing host on record");
    assert_eq!(state.failures, 1);
    assert!(state.cooldown_until_ms > now_ms(), "the host cools down");

    // Second job to the same host: parked, not sent.
    let job = delivery_job(alice, &failing, follow_body("https://remote.example/users/eve"));
    assert_eq!(delivery::run(&config, &job).await, Ok(()));
    assert_eq!(failing_hits.load(Ordering::SeqCst), 1, "no second hit while cooling down");
    let parked: Vec<Job> = store
        .claim_due(now_ms() + 3_600_000 + 3_600_000, 100)
        .unwrap()
        .into_iter()
        .filter(|j| j.kind == JobKind::DeliverActivity && j.payload.get("deferrals").is_some())
        .collect();
    assert_eq!(parked.len(), 1, "one deferred copy waits out the cooldown");
    assert_eq!(parked[0].payload["deferrals"], 1);
    assert!(parked[0].due_at_ms >= state.cooldown_until_ms);

    // An unrelated host delivers at once, with no budget charged.
    let job = delivery_job(alice, &ok, follow_body("https://remote.example/users/eve"));
    assert_eq!(delivery::run(&config, &job).await, Ok(()));
    assert_eq!(ok_hits.load(Ordering::SeqCst), 1);
    assert!(dest_of(&store, &ok).is_none(), "a healthy host leaves no record");
}

#[tokio::test]
async fn a_success_resets_the_host_and_an_exhausted_budget_quarantines() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &loopback_policy());
    let alice = sender_with_keys(&store);

    // A flap: the first hit fails, later hits succeed.
    let hits = Arc::new(AtomicUsize::new(0));
    let flap = {
        let hits = hits.clone();
        let app = Router::new().route(
            "/inbox",
            post(move || {
                let hits = hits.clone();
                async move {
                    if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::ACCEPTED
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        format!("http://localhost:{port}/inbox")
    };
    let host = delivery::dest_host(&url::Url::parse(&flap).unwrap());

    let job = delivery_job(alice, &flap, follow_body("https://remote.example/users/eve"));
    assert!(matches!(delivery::run(&config, &job).await, Err(DeliveryError::Retry(_))));
    assert_eq!(dest_of(&store, &flap).unwrap().failures, 1);

    // The cooldown has not expired, but a test clock cannot wait it out: mark
    // it expired and deliver again. The 2xx resets the host.
    store
        .set_delivery_dest(&host, birchtree::domain::DeliveryDestState { failures: 1, cooldown_until_ms: 0 })
        .unwrap();
    let job = delivery_job(alice, &flap, follow_body("https://remote.example/users/eve"));
    assert_eq!(delivery::run(&config, &job).await, Ok(()));
    assert_eq!(dest_of(&store, &flap).unwrap().failures, 0, "a success resets the host");

    // Past the budget, one more failure quarantines instead of retrying.
    let failing = counting_inbox(503, Arc::new(AtomicUsize::new(0))).await;
    let failing_host = delivery::dest_host(&url::Url::parse(&failing).unwrap());
    store
        .set_delivery_dest(
            &failing_host,
            birchtree::domain::DeliveryDestState { failures: DEST_FAILURE_BUDGET, cooldown_until_ms: 0 },
        )
        .unwrap();
    let job = delivery_job(alice, &failing, follow_body("https://remote.example/users/eve"));
    let outcome = delivery::run(&config, &job).await;
    assert!(matches!(outcome, Err(DeliveryError::Quarantine(_))), "{outcome:?}");
}

#[tokio::test]
async fn parking_past_the_deferral_cap_quarantines() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &loopback_policy());
    let alice = sender_with_keys(&store);
    let failing = counting_inbox(503, Arc::new(AtomicUsize::new(0))).await;
    let host = delivery::dest_host(&url::Url::parse(&failing).unwrap());
    store
        .set_delivery_dest(
            &host,
            birchtree::domain::DeliveryDestState { failures: 1, cooldown_until_ms: now_ms() + 600_000 },
        )
        .unwrap();

    let mut payload = json!({"sender_id": alice.value(), "inbox": failing, "activity_uri": "x", "body": follow_body("https://remote.example/users/eve")});
    payload["deferrals"] = json!(DEST_MAX_DEFERRALS);
    let job = Job::new(unique_job_id(), JobKind::DeliverActivity, payload, now_ms(), now_ms());
    let outcome = delivery::run(&config, &job).await;
    assert!(matches!(outcome, Err(DeliveryError::Quarantine(_))), "{outcome:?}");
}

#[test]
fn destination_cooldowns_back_off_exponentially_to_a_cap() {
    assert_eq!(delivery::dest_cooldown_ms(0), 60_000);
    assert_eq!(delivery::dest_cooldown_ms(1), 60_000);
    assert_eq!(delivery::dest_cooldown_ms(2), 120_000);
    assert_eq!(delivery::dest_cooldown_ms(3), 240_000);
    assert_eq!(delivery::dest_cooldown_ms(100), 3_600_000);
}

#[test]
fn destination_hosts_are_lowercased_with_their_explicit_port() {
    let url = url::Url::parse("HTTP://Example.COM:8080/inbox").unwrap();
    assert_eq!(delivery::dest_host(&url), "example.com:8080");
    let url = url::Url::parse("https://example.com/inbox").unwrap();
    assert_eq!(delivery::dest_host(&url), "example.com");
}

// --- remote documents: size + content validation ------------------------------

/// A loopback document server: a valid doc, an oversize doc, an HTML doc, a
/// valid-JSON doc under a wrong content type, a 410 and a 404.
async fn doc_server() -> String {
    let app = Router::new()
        .route(
            "/ok/1",
            get(|req: axum::http::Request<axum::body::Body>| async move {
                let base = format!("http://{}", req.headers().get("host").unwrap().to_str().unwrap());
                (
                    [(axum::http::header::CONTENT_TYPE, "application/activity+json")],
                    json!({"id": format!("{base}/ok/1"), "type": "Note", "content": "fine"}).to_string(),
                )
            }),
        )
        .route(
            "/big/1",
            get(|req: axum::http::Request<axum::body::Body>| async move {
                let base = format!("http://{}", req.headers().get("host").unwrap().to_str().unwrap());
                let padding = "x".repeat(300 * 1024);
                (
                    [(axum::http::header::CONTENT_TYPE, "application/activity+json")],
                    json!({"id": format!("{base}/big/1"), "type": "Note", "content": padding}).to_string(),
                )
            }),
        )
        .route(
            "/html/1",
            get(|req: axum::http::Request<axum::body::Body>| async move {
                let base = format!("http://{}", req.headers().get("host").unwrap().to_str().unwrap());
                (
                    [(axum::http::header::CONTENT_TYPE, "text/html")],
                    format!("<html><body>not activity at {base}/html/1</body></html>"),
                )
            }),
        )
        // Valid JSON under a wrong content type: isolates the content-type gate
        // (an HTML body fails earlier, at JSON parse — also terminal).
        .route(
            "/jsonct/1",
            get(|req: axum::http::Request<axum::body::Body>| async move {
                let base = format!("http://{}", req.headers().get("host").unwrap().to_str().unwrap());
                (
                    [(axum::http::header::CONTENT_TYPE, "text/plain")],
                    json!({"id": format!("{base}/jsonct/1"), "type": "Note", "content": "fine"}).to_string(),
                )
            }),
        )
        .route("/gone/1", get(|| async { (StatusCode::GONE, "") }))
        .route("/missing/1", get(|| async { (StatusCode::NOT_FOUND, "") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://localhost:{port}")
}

async fn fetch_doc(
    data: &Data<FedContext>,
    base: &str,
    path: &str,
) -> Result<
    activitypub_federation::fetch::FetchObjectResponse<serde_json::Value>,
    activitypub_federation::error::Error,
> {
    let url = url::Url::parse(&format!("{base}{path}")).unwrap();
    fetch_object_http::<_, serde_json::Value>(&url, data).await
}

#[tokio::test]
async fn remote_documents_are_size_and_content_checked() {
    use activitypub_federation::error::Error as ProtocolError;
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &loopback_policy());
    let data = config.to_request_data();
    let base = doc_server().await;

    let ok = fetch_doc(&data, &base, "/ok/1").await.expect("a small valid document fetches");
    assert_eq!(ok.object["id"], format!("{base}/ok/1"));

    assert!(
        matches!(fetch_doc(&data, &base, "/big/1").await, Err(ProtocolError::ResponseBodyLimit)),
        "an oversize document is rejected, not buffered"
    );
    let html = fetch_doc(&data, &base, "/html/1").await;
    assert!(
        matches!(
            html,
            Err(ProtocolError::ParseFetchedObject(..)) | Err(ProtocolError::FetchInvalidContentType(_))
        ),
        "an HTML body is terminally rejected (it fails JSON parse before the content-type gate)"
    );
    assert!(
        matches!(
            fetch_doc(&data, &base, "/jsonct/1").await,
            Err(ProtocolError::FetchInvalidContentType(_))
        ),
        "valid JSON under a wrong content type is rejected by the content-type gate"
    );
    assert!(
        matches!(fetch_doc(&data, &base, "/gone/1").await, Err(ProtocolError::ObjectDeleted(_))),
        "a 410 is terminal, not a retried middleware error"
    );
    let missing = fetch_doc(&data, &base, "/missing/1").await;
    assert!(missing.is_err(), "a 404 is an error");
    assert!(
        !matches!(missing, Err(ProtocolError::ReqwestMiddleware(_))),
        "a 404 fetch passes its status through (no 4xx-to-error conversion on GET)"
    );
}

#[tokio::test]
async fn a_parent_fetch_with_a_wrong_content_type_is_terminal_for_the_reply() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &loopback_policy());
    let eve = remote(&store, "eve");
    let base = doc_server().await;
    let parent_uri = format!("{base}/html/1");

    let mut reply = remote_note(eve, "https://remote.example/notes/reply-1", "reply");
    reply.in_reply_to_uri = Some(parent_uri.clone());
    let stored = inbound::ingest_note(&*store, reply, None, now_ms()).unwrap();
    assert_eq!(stored.parent_unknown.as_deref(), Some(parent_uri.as_str()));

    let job = Job::new(
        unique_job_id(),
        JobKind::ResolveParent,
        json!({"child_status_id": stored.status.id.value(), "parent_uri": parent_uri, "hop": 0}),
        now_ms(),
        now_ms(),
    );
    // Terminal (Ok): the reply stays unresolved, nothing re-enqueues.
    assert_eq!(birchtree::federation::threads::resolve_parent(&config, &job).await, Ok(()));
    assert!(store.status_by_id(stored.status.id).unwrap().unwrap().in_reply_to_id.is_none());
}

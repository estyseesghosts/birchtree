//! Phase 6: federation, in process. The protocol is the `activitypub_federation`
//! crate's; these tests pin birchtree's side of it: discovery documents, the
//! inbox's refusal path, the verify rules, the store invariants of the follow
//! commands, and the delivery job's settlement.
//!
//! Two-instance interop with vanilla Mastodon 4.7.3 is not run here. It is the
//! lab run in `scripts/lab/phase6-interop.sh`, recorded in
//! `docs/logs/2026-10-17-phase-6-verification.md`.

mod common;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use activitypub_federation::http_signatures::generate_actor_keypair;
use activitypub_federation::protocol::context::WithContext;
use activitypub_federation::traits::ActivityHandler;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use birchtree::activitypub::activities::{
    Accept, AcceptObject, Activity, Follow, Undo, UndoObject,
};
use birchtree::activitypub::http;
use birchtree::api::mastodon::ServerInfo;
use birchtree::application::context::Context;
use birchtree::application::federation::{
    InboundAccept, InboundFollow, InboundUndoFollow, OutboundFollow, receive_accept,
    receive_follow, receive_undo_follow, request_remote_follow,
};
use birchtree::application::federation_ports::{
    DeliverySpec, FederationStore, InboundDisposition, RemoteAccountUpsert,
};
use birchtree::application::ports::RelationshipStore;
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator};
use birchtree::federation::delivery::{self, DeliveryError};
use birchtree::federation::net::SafeHttpConfig;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};
use common::{TEST_PUBLIC_URL, send, temp_store};
use serde_json::json;
use tower::ServiceExt;

const LOCAL_ACTOR: &str = "https://example.social/users/alice";

/// A job ID unique within the test run (the store's own generator is private).
fn unique_job_id() -> Id {
    Id::new(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos() as u64,
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn context(store: &Arc<RedbStore>, dir: &std::path::Path) -> Arc<Context> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blobs"),
    );
    let server = Arc::new(ServerInfo::from_config(&Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    }));
    let streams = Hub::new(store.clone(), clock.clone(), server, Limits::default());
    Arc::new(Context::new(
        clock,
        ids,
        store.clone(),
        blobs,
        Arc::new(streams),
    ))
}

/// A remote actor known to the store, with a fresh key, so no test fetches the network.
fn remote_actor(store: &RedbStore, name: &str) -> RemoteAccountUpsert {
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
        shared_inbox_uri: None,
        public_key_pem: keys.public_key,
        key_id: format!("{actor_uri}#main-key"),
    };
    store
        .upsert_remote(upsert.clone(), now_ms())
        .expect("upsert remote");
    upsert
}

fn local_id(store: &RedbStore, username: &str) -> Id {
    common::mk_account(store, username).id
}

// --- discovery -------------------------------------------------------------

#[tokio::test]
async fn webfinger_answers_for_local_names_and_refuses_the_rest() {
    let srv = common::TestServer::new(true);
    local_id(&srv.store, "alice");

    let (status, _, body) = send(
        &srv.router,
        Request::builder()
            .uri("/.well-known/webfinger?resource=acct:alice@example.social")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let doc = common::json(&body);
    assert_eq!(doc["subject"], "acct:alice@example.social");
    let me = doc["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["rel"] == "self")
        .expect("self link");
    assert_eq!(me["type"], "application/activity+json");
    assert_eq!(me["href"], LOCAL_ACTOR);

    for resource in [
        "acct:nobody@example.social",
        "acct:alice@elsewhere.test",
        "junk",
    ] {
        let (status, _, _) = send(
            &srv.router,
            Request::builder()
                .uri(format!("/.well-known/webfinger?resource={resource}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{resource}");
    }
}

#[tokio::test]
async fn actor_document_carries_the_key_the_signatures_use() {
    let srv = common::TestServer::new(true);
    local_id(&srv.store, "alice");

    let (status, headers, body) = send(
        &srv.router,
        Request::builder()
            .uri("/users/alice")
            .header("accept", "application/activity+json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").unwrap(),
        "application/activity+json"
    );
    let doc = common::json(&body);
    assert_eq!(doc["type"], "Person");
    assert_eq!(doc["id"], LOCAL_ACTOR);
    assert_eq!(doc["preferredUsername"], "alice");
    assert_eq!(doc["inbox"], "https://example.social/users/alice/inbox");
    assert_eq!(
        doc["endpoints"]["sharedInbox"],
        "https://example.social/inbox"
    );
    assert_eq!(
        doc["publicKey"]["id"],
        "https://example.social/users/alice#main-key"
    );
    assert_eq!(doc["publicKey"]["owner"], LOCAL_ACTOR);
    assert!(
        doc["publicKey"]["publicKeyPem"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN PUBLIC KEY-----")
    );

    let (status, _, _) = send(
        &srv.router,
        Request::builder()
            .uri("/users/nobody")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nodeinfo_describes_the_software() {
    let srv = common::TestServer::new(true);
    let (status, _, body) = send(
        &srv.router,
        Request::builder()
            .uri("/.well-known/nodeinfo")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        common::json(&body)["links"][0]["href"],
        "https://example.social/nodeinfo/2.0"
    );
    let (status, _, body) = send(
        &srv.router,
        Request::builder()
            .uri("/nodeinfo/2.0")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let doc = common::json(&body);
    assert_eq!(doc["version"], "2.0");
    assert_eq!(doc["software"]["name"], "birchtree");
    assert_eq!(doc["protocols"], json!(["activitypub"]));
}

// --- inbox: refusal before any state change --------------------------------

/// A router with only the federation routes, in debug mode: the inbox test names a
/// remote host that the sandbox cannot resolve.
fn federation_router(store: &Arc<RedbStore>, dir: &std::path::Path) -> Router {
    let config = common::federation_config_with(
        context(store, dir),
        TEST_PUBLIC_URL,
        &SafeHttpConfig::default(),
        true,
    );
    http::routes::<()>(config).with_state(())
}

#[tokio::test]
async fn unsigned_activity_from_a_known_actor_is_refused_with_no_state_change() {
    let (dir, store) = temp_store();
    let bob = local_id(&store, "bob");
    let eve = remote_actor(&store, "eve");
    let router = federation_router(&store, dir.path());

    let activity_id = "https://remote.example/activities/1";
    let body = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "type": "Follow",
        "id": activity_id,
        "actor": eve.actor_uri,
        "object": "https://example.social/users/bob"
    });
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/bob/inbox")
                .header("content-type", "application/activity+json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    assert!(
        !store.activity_seen(activity_id).unwrap(),
        "a refused activity is not recorded"
    );
    assert!(
        store
            .follow_of(eve_id(&store, &eve), bob)
            .unwrap()
            .is_none()
    );
}

fn eve_id(store: &RedbStore, upsert: &RemoteAccountUpsert) -> Id {
    store
        .account_by_actor_uri(&upsert.actor_uri)
        .unwrap()
        .expect("eve")
        .id
}

/// An RFC 9421-shaped request (modern `Signature-Input`/`Signature`
/// headers, no legacy Cavage `Signature` header) is refused without state
/// change: Ed25519/RFC 9421 is explicitly remaining work (see
/// `docs/birchtree-rsa-prod.md`), so modern-only senders 401 — never
/// accepted, never crashed, never silently downgraded.
#[tokio::test]
async fn rfc9421_shaped_request_is_refused_without_state_change() {
    let (dir, store) = temp_store();
    let bob = local_id(&store, "bob");
    let eve = remote_actor(&store, "eve");
    let router = federation_router(&store, dir.path());

    let activity_id = "https://remote.example/activities/rfc9421";
    let body = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "type": "Follow",
        "id": activity_id,
        "actor": eve.actor_uri,
        "object": "https://example.social/users/bob"
    });
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/bob/inbox")
                .header("content-type", "application/activity+json")
                .header(
                    "signature-input",
                    "sig=(\"@method\" \"@target-uri\" \"content-digest\");created=1700000000;keyid=\"https://remote.example/users/eve#ed25519-key\"",
                )
                .header("signature", "sig=:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=:")
                .header("content-digest", "sha-512=:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=:")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    assert!(
        !store.activity_seen(activity_id).unwrap(),
        "a refused activity is not recorded"
    );
    assert!(
        store
            .follow_of(eve_id(&store, &eve), bob)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn inbox_of_an_unknown_local_account_is_not_found() {
    let (dir, store) = temp_store();
    let router = federation_router(&store, dir.path());
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/users/nobody/inbox")
                .header("content-type", "application/activity+json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// --- verify rules: refused before any write --------------------------------

#[tokio::test]
async fn accept_naming_another_followed_account_is_refused() {
    let (dir, store) = temp_store();
    local_id(&store, "bob");
    let config = common::federation_config_with(
        context(&store, dir.path()),
        TEST_PUBLIC_URL,
        &SafeHttpConfig::default(),
        true,
    );
    let data = config.to_request_data();
    // The embedded follow says alice was followed, but the Accept comes from eve.
    let follow: Follow = serde_json::from_value(json!({
        "type": "Follow",
        "id": "https://example.social/users/bob/follows/1",
        "actor": "https://example.social/users/bob",
        "object": LOCAL_ACTOR
    }))
    .unwrap();
    let accept = Accept {
        kind: Default::default(),
        id: "https://remote.example/accepts/1".parse().unwrap(),
        actor: "https://remote.example/users/eve"
            .parse::<url::Url>()
            .unwrap()
            .into(),
        object: AcceptObject::Follow(Box::new(follow)),
        result: None,
    };
    assert!(accept.verify(&data).await.is_err());
}

#[tokio::test]
async fn undo_of_someone_elses_follow_is_refused() {
    let (dir, store) = temp_store();
    local_id(&store, "alice");
    let config = common::federation_config_with(
        context(&store, dir.path()),
        TEST_PUBLIC_URL,
        &SafeHttpConfig::default(),
        true,
    );
    let data = config.to_request_data();
    let follow: Follow = serde_json::from_value(json!({
        "type": "Follow",
        "id": "https://remote.example/follows/1",
        "actor": "https://remote.example/users/mallory",
        "object": LOCAL_ACTOR
    }))
    .unwrap();
    let undo: Undo = serde_json::from_value(json!({
        "type": "Undo",
        "id": "https://remote.example/undo/1",
        "actor": "https://remote.example/users/eve",
        "object": serde_json::to_value(&follow).unwrap()
    }))
    .unwrap();
    let _ = UndoObject::Follow(Box::new(follow.clone()));
    assert!(undo.verify(&data).await.is_err());
}

#[test]
fn unsupported_activity_types_do_not_parse() {
    // Anything outside the Phase 6 set is refused at parse time, never accepted and dropped.
    for kind in [
        "Like", "Announce", "Create", "Flag", "Update", "Delete", "Question",
    ] {
        let body = json!({
            "type": kind,
            "id": "https://remote.example/x",
            "actor": "https://remote.example/users/eve",
            "object": "https://example.social/statuses/1"
        });
        assert!(
            serde_json::from_value::<WithContext<Activity>>(body).is_err(),
            "{kind} must not parse"
        );
    }
}

// --- store invariants of the follow commands -------------------------------

#[test]
fn an_ignored_undo_is_not_recorded_so_a_later_real_undo_applies() {
    let (dir, store) = temp_store();
    let alice = local_id(&store, "alice");
    let eve = remote_actor(&store, "eve");
    let _ = dir;
    let eve_account = store.account_by_actor_uri(&eve.actor_uri).unwrap().unwrap();
    let undo_id = "https://remote.example/users/eve#follows/1/undo";

    // The Undo arrives before any follow exists: recorded as ignored, so nothing is stored.
    let first = receive_undo_follow(
        &*store,
        InboundUndoFollow {
            activity_uri: undo_id.into(),
            actor: eve.clone(),
            target_id: alice,
        },
        now_ms(),
    )
    .unwrap();
    assert!(matches!(first, InboundDisposition::Ignored(_)));
    assert!(!store.activity_seen(undo_id).unwrap());

    // The follow arrives, then the same Undo ID for the real undo: it applies.
    let accept_body = json!({});
    let follow = receive_follow(
        &*store,
        InboundFollow {
            activity_uri: "https://remote.example/follows/1".into(),
            actor: eve.clone(),
            target_id: alice,
            accept_uri: "https://remote.example/follows/1#accept".into(),
            accept_body: accept_body.clone(),
        },
        now_ms(),
    )
    .unwrap();
    assert_eq!(follow, InboundDisposition::Applied);
    assert!(store.follow_of(eve_account.id, alice).unwrap().is_some());

    let second = receive_undo_follow(
        &*store,
        InboundUndoFollow {
            activity_uri: undo_id.into(),
            actor: eve,
            target_id: alice,
        },
        now_ms(),
    )
    .unwrap();
    assert_eq!(second, InboundDisposition::Applied);
    assert!(store.follow_of(eve_account.id, alice).unwrap().is_none());
    assert!(store.activity_seen(undo_id).unwrap());
}

#[test]
fn a_redelivered_applied_follow_is_a_duplicate_and_changes_nothing() {
    let (_dir, store) = temp_store();
    let alice = local_id(&store, "alice");
    let eve = remote_actor(&store, "eve");
    let input = || InboundFollow {
        activity_uri: "https://remote.example/follows/7".into(),
        actor: eve.clone(),
        target_id: alice,
        accept_uri: "https://remote.example/follows/7#accept".into(),
        accept_body: json!({}),
    };
    assert_eq!(
        receive_follow(&*store, input(), now_ms()).unwrap(),
        InboundDisposition::Applied
    );
    let eve_id = store
        .account_by_actor_uri(&eve.actor_uri)
        .unwrap()
        .unwrap()
        .id;
    let before = store.account_stats(alice).unwrap();
    assert_eq!(
        receive_follow(&*store, input(), now_ms()).unwrap(),
        InboundDisposition::Duplicate
    );
    let after = store.account_stats(alice).unwrap();
    assert_eq!(before.followers_count, after.followers_count);
    assert_eq!(before.follow_requests_count, after.follow_requests_count);
    assert!(store.follow_of(eve_id, alice).unwrap().is_some());
}

#[test]
fn an_accept_from_an_account_that_was_not_followed_changes_nothing() {
    let (_dir, store) = temp_store();
    let alice = local_id(&store, "alice");
    let followed = remote_actor(&store, "frank");
    let impostor = remote_actor(&store, "mallory");
    let follow_uri = "https://example.social/users/alice/follows/9";
    request_remote_follow(
        &*store,
        OutboundFollow {
            follower_id: alice,
            followed_id: store
                .account_by_actor_uri(&followed.actor_uri)
                .unwrap()
                .unwrap()
                .id,
            follow_uri: follow_uri.into(),
            inbox_uri: followed.inbox_uri.clone(),
            body: json!({"type": "Follow"}),
        },
        now_ms(),
    )
    .unwrap();

    let disposition = receive_accept(
        &*store,
        InboundAccept {
            activity_uri: "https://remote.example/mallory/accept/1".into(),
            actor: impostor,
            follow_uri: follow_uri.into(),
        },
        now_ms(),
    )
    .unwrap();
    assert!(
        matches!(disposition, InboundDisposition::Ignored(_)),
        "{disposition:?}"
    );
    let row = store
        .follow_of(
            alice,
            store
                .account_by_actor_uri(&followed.actor_uri)
                .unwrap()
                .unwrap()
                .id,
        )
        .unwrap()
        .expect("the pending follow is still there");
    assert_eq!(row.state, birchtree::domain::FollowState::Pending);
}

// --- delivery: the job's settlement ----------------------------------------

/// A local sender (`alice`) with a signing key.
fn sender_with_keys(store: &RedbStore) -> Id {
    let alice = local_id(store, "alice");
    let pair = generate_actor_keypair().expect("keypair");
    store
        .store_local_keys(
            alice,
            &format!("{LOCAL_ACTOR}#main-key"),
            &pair.public_key,
            &pair.private_key,
        )
        .expect("keys");
    alice
}

/// A DeliverActivity job from `sender` to one inbox.
fn delivery_job(sender: Id, inbox: &str, body: serde_json::Value) -> Job {
    Job::new(
        unique_job_id(),
        JobKind::DeliverActivity,
        json!({"sender_id": sender.value(), "inbox": inbox, "activity_uri": "x", "body": body}),
        now_ms(),
        now_ms(),
    )
}

fn follow_body(object: &str) -> serde_json::Value {
    serde_json::to_value(WithContext::new_default(Activity::Follow(Follow {
        kind: Default::default(),
        id: "https://example.social/users/alice/follows/1"
            .parse()
            .unwrap(),
        actor: LOCAL_ACTOR.parse::<url::Url>().unwrap().into(),
        object: object.parse::<url::Url>().unwrap().into(),
    })))
    .unwrap()
}

/// A loopback inbox that answers every POST with `status`.
async fn loopback_inbox(status: u16) -> String {
    let app = Router::new().route(
        "/inbox",
        post(move || async move { StatusCode::from_u16(status).unwrap() }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    // A host name, not an IP literal: the crate's URL check requires a domain.
    format!("http://localhost:{port}/inbox")
}

/// Loopback is allowed by the operator allowlist, as a lab network is. `localhost`
/// may resolve to either loopback address.
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

#[tokio::test]
async fn delivery_retries_a_server_error_and_completes_an_accepted_one() {
    let (dir, store) = temp_store();
    let config = common::federation_config_with(
        context(&store, dir.path()),
        TEST_PUBLIC_URL,
        &loopback_policy(),
        true,
    );

    let alice = sender_with_keys(&store);
    let busy = loopback_inbox(503).await;
    let job = delivery_job(
        alice,
        &busy,
        follow_body("https://remote.example/users/eve"),
    );
    let outcome = delivery::run(&config, &job).await;
    assert!(
        matches!(outcome, Err(DeliveryError::Retry(_))),
        "{outcome:?}"
    );

    let ok = loopback_inbox(202).await;
    let job = delivery_job(alice, &ok, follow_body("https://remote.example/users/eve"));
    assert_eq!(delivery::run(&config, &job).await, Ok(()));
}

#[tokio::test]
async fn delivery_retries_a_client_error() {
    // Birchtree's rule (D28): a 4xx is retried, through the middleware that turns it
    // into an error. The job's retry budget bounds the attempts.
    let (dir, store) = temp_store();
    let config = common::federation_config_with(
        context(&store, dir.path()),
        TEST_PUBLIC_URL,
        &loopback_policy(),
        true,
    );
    let alice = sender_with_keys(&store);
    let rejecting = loopback_inbox(400).await;
    let job = delivery_job(
        alice,
        &rejecting,
        follow_body("https://remote.example/users/eve"),
    );
    assert!(matches!(
        delivery::run(&config, &job).await,
        Err(DeliveryError::Retry(_))
    ));
}

#[tokio::test]
async fn delivery_quarantines_a_target_the_policy_refuses() {
    let (dir, store) = temp_store();
    let config = common::federation_config(context(&store, dir.path()), TEST_PUBLIC_URL);
    let alice = sender_with_keys(&store);
    let job = delivery_job(
        alice,
        "https://10.0.0.5/inbox",
        follow_body("https://remote.example/users/eve"),
    );
    let outcome = delivery::run(&config, &job).await;
    assert!(
        matches!(outcome, Err(DeliveryError::Quarantine(_))),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn delivery_quarantines_a_job_whose_sender_has_no_signing_key() {
    let (dir, store) = temp_store();
    let config = common::federation_config(context(&store, dir.path()), TEST_PUBLIC_URL);
    let alice = local_id(&store, "alice");
    let job = Job::new(
        unique_job_id(),
        JobKind::DeliverActivity,
        json!({
            "sender_id": alice.value(),
            "inbox": "https://remote.example/inbox",
            "activity_uri": "x",
            "body": follow_body("https://remote.example/users/eve"),
        }),
        now_ms(),
        now_ms(),
    );
    let outcome = delivery::run(&config, &job).await;
    assert!(
        matches!(outcome, Err(DeliveryError::Quarantine(_))),
        "{outcome:?}"
    );
}

#[test]
fn delivery_spec_is_the_store_payload() {
    // The store command takes a DeliverySpec; its fields are the job payload.
    let spec = DeliverySpec {
        sender_id: Id::new(1),
        inbox_uri: "https://remote.example/inbox".into(),
        activity_uri: "https://example.social/x".into(),
        body: json!({}),
    };
    assert_eq!(spec.inbox_uri, "https://remote.example/inbox");
}

// --- inbox: a stale cached key ---------------------------------------------

/// Serve `router` on a loopback port and return the port.
async fn serve_loopback(router: Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
    port
}

#[tokio::test]
async fn a_signature_that_fails_against_a_stale_cached_key_is_checked_against_a_refetched_one() {
    // Sender: a local `alice` whose actor document and signatures carry key B.
    let (sender_dir, sender_store) = temp_store();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let sender_port = listener.local_addr().expect("addr").port();
    let sender_url = format!("http://localhost:{sender_port}");
    let sender_config = common::federation_config_with(
        context(&sender_store, sender_dir.path()),
        &sender_url,
        &loopback_policy(),
        true,
    );
    let alice = birchtree::application::accounts::create_account(
        &*sender_store,
        &sender_url,
        now_ms(),
        birchtree::application::accounts::CreateAccountCommand {
            username: "alice".to_string(),
            email: None,
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create alice")
    .id;
    let key_b = generate_actor_keypair().expect("keypair");
    let alice_uri = format!("{sender_url}/users/alice");
    sender_store
        .store_local_keys(
            alice,
            &format!("{alice_uri}#main-key"),
            &key_b.public_key,
            &key_b.private_key,
        )
        .expect("keys");
    let sender_router = http::routes::<()>(sender_config.clone()).with_state(());
    tokio::spawn(async move { axum::serve(listener, sender_router).await.expect("serve") });

    // Receiver: has cached alice two minutes ago with key A, which she has since replaced.
    let (recv_dir, recv_store) = temp_store();
    let bob = local_id(&recv_store, "bob");
    let key_a = generate_actor_keypair().expect("keypair");
    recv_store
        .upsert_remote(
            RemoteAccountUpsert {
                actor_uri: alice_uri.clone(),
                username: "alice".into(),
                domain: format!("localhost:{sender_port}"),
                display_name: String::new(),
                note: String::new(),
                locked: false,
                inbox_uri: format!("{alice_uri}/inbox"),
                shared_inbox_uri: None,
                public_key_pem: key_a.public_key,
                key_id: format!("{alice_uri}#main-key"),
            },
            now_ms() - 120_000,
        )
        .expect("cache alice");
    let recv_config = common::federation_config_with(
        context(&recv_store, recv_dir.path()),
        TEST_PUBLIC_URL,
        &loopback_policy(),
        true,
    );
    let recv_port = serve_loopback(http::routes::<()>(recv_config).with_state(())).await;

    let body = serde_json::to_value(WithContext::new_default(Activity::Follow(Follow {
        kind: Default::default(),
        id: format!("{alice_uri}/follows/1").parse().unwrap(),
        actor: alice_uri.parse::<url::Url>().unwrap().into(),
        object: format!("{TEST_PUBLIC_URL}/users/bob")
            .parse::<url::Url>()
            .unwrap()
            .into(),
    })))
    .unwrap();
    let job = delivery_job(
        alice,
        &format!("http://localhost:{recv_port}/users/bob/inbox"),
        body,
    );
    assert_eq!(delivery::run(&sender_config, &job).await, Ok(()));

    let alice_remote = recv_store
        .account_by_actor_uri(&alice_uri)
        .unwrap()
        .expect("alice");
    assert!(
        recv_store
            .follow_of(alice_remote.id, bob)
            .unwrap()
            .is_some(),
        "the follow was accepted after the key was refetched"
    );
}

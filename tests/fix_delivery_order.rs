//! Per-destination delivery order (prod-2 item 9).
//!
//! Every delivery payload carries its destination host's monotonic `seq`
//! (minted at enqueue, durable across restarts). The send path holds the
//! per-host lock and then parks sequence N+1 while N is still unfinished,
//! so a `Follow` and its `Undo` to one inbox can never go out on the wire —
//! even when the worker runs them concurrently or a restart reorders the
//! claims. Terminal rows never block (a permanently failed earlier send
//! must not wedge its host).

mod common;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use serde_json::json;

use activitypub_federation::http_signatures::generate_actor_keypair;
use birchtree::activitypub::FedContext;
use birchtree::application::context::Context;
use birchtree::application::federation_ports::{FederationContentStore, FederationStore};
use birchtree::application::ports::JobQueue;
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator};
use birchtree::federation::delivery::{self, DeliveryError};
use birchtree::federation::net::SafeHttpConfig;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};

use common::{TEST_PUBLIC_URL, mk_account};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

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

fn context(store: &Arc<RedbStore>, dir: &std::path::Path) -> Arc<Context> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blobs"),
    );
    let server = Arc::new(birchtree::api::mastodon::ServerInfo::from_config(&Config {
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

fn federation(
    store: &Arc<RedbStore>,
    dir: &std::path::Path,
) -> activitypub_federation::config::FederationConfig<FedContext> {
    common::federation_config_with(context(store, dir), TEST_PUBLIC_URL, &loopback_policy(), true)
}

/// A local sender with signing keys (no fetch is made for it).
fn sender_with_keys(store: &RedbStore, name: &str) -> Id {
    let account = mk_account(store, name);
    let pair = generate_actor_keypair().expect("keypair");
    let actor = format!("{TEST_PUBLIC_URL}/users/{name}");
    store
        .store_local_keys(
            account.id,
            &format!("{actor}#main-key"),
            &pair.public_key,
            &pair.private_key,
        )
        .expect("keys");
    account.id
}

/// An inbox server recording activity ids in arrival order.
async fn recording_inbox(
    arrivals: Arc<std::sync::Mutex<Vec<String>>>,
) -> String {
    let app = Router::new().route(
        "/inbox",
        post(move |body: axum::body::Bytes| {
            let arrivals = arrivals.clone();
            async move {
                let id = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(str::to_string))
                    .unwrap_or_default();
                arrivals.lock().expect("arrivals").push(id);
                StatusCode::ACCEPTED
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://127.0.0.1:{port}/inbox")
}

fn follow_body(actor: &str, id: &str) -> serde_json::Value {
    json!({
        "type": "Follow",
        "id": id,
        "actor": actor,
        "object": "https://remote.example/users/eve",
    })
}

fn undo_body(actor: &str, id: &str, follow_id: &str) -> serde_json::Value {
    json!({
        "type": "Undo",
        "id": id,
        "actor": actor,
        "object": {"type": "Follow", "id": follow_id},
    })
}

/// Enqueue a `Follow` then its `Undo` to one inbox; returns their job ids in
/// plan order.
fn enqueue_pair(store: &RedbStore, sender: Id, inbox: &str, now: u64) -> (Job, Job) {
    let actor = format!("{TEST_PUBLIC_URL}/users/alice");
    let follow_id = format!("{TEST_PUBLIC_URL}/activities/follow-1");
    let undo_id = format!("{TEST_PUBLIC_URL}/activities/undo-1");
    let follow = store
        .enqueue(
            JobKind::DeliverActivity,
            delivery::stamp_delivery_seq(
                &*store,
                inbox,
                json!({
                    "sender_id": sender.value(),
                    "inbox": inbox,
                    "activity_uri": follow_id,
                    "body": follow_body(&actor, &follow_id),
                }),
            )
            .expect("stamp"),
            now,
        )
        .expect("enqueue follow");
    let undo = store
        .enqueue(
            JobKind::DeliverActivity,
            delivery::stamp_delivery_seq(
                &*store,
                inbox,
                json!({
                    "sender_id": sender.value(),
                    "inbox": inbox,
                    "activity_uri": undo_id,
                    "body": undo_body(&actor, &undo_id, &follow_id),
                }),
            )
            .expect("stamp"),
            now,
        )
        .expect("enqueue undo");
    let follow_seq = follow.payload["seq"].as_u64().expect("follow seq");
    let undo_seq = undo.payload["seq"].as_u64().expect("undo seq");
    assert!(follow_seq < undo_seq, "plan order is sequence order");
    (follow, undo)
}

fn claimed_pair(store: &RedbStore, now: u64, follow_id: Id, undo_id: Id) -> (Job, Job) {
    let claimed = store.claim_due(now + 1, 10).expect("claim");
    let find = |id: Id| {
        claimed
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .expect("claimed")
    };
    (find(follow_id), find(undo_id))
}

/// The second send parks while the first is still leased: nothing reaches
/// the inbox until the first send settles, and the pair arrives in order.
#[tokio::test]
async fn undo_waits_for_follow_on_one_inbox() {
    let (dir, store) = common::temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let sender = sender_with_keys(&store, "alice");
    let arrivals = Arc::new(std::sync::Mutex::new(Vec::new()));
    let inbox = recording_inbox(arrivals.clone()).await;

    let (follow, undo) = enqueue_pair(&store, sender, &inbox, now);
    let (follow_job, undo_job) = claimed_pair(&store, now, follow.id, undo.id);

    // The first send is delayed (leased but never run): the second parks
    // instead of jumping the queue — no bytes reach the inbox.
    delivery::run(&config, &undo_job).await.expect("parks");
    assert!(
        arrivals.lock().expect("arrivals").is_empty(),
        "nothing delivered while the first send is unfinished"
    );
    // A parked copy carries the same sequence for the next attempt.
    let parked: Vec<Job> = store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|job| {
            job.kind == JobKind::DeliverActivity
                && job.id != undo_job.id
                && job.payload.get("body").and_then(|b| b.get("id"))
                    == undo_job.payload.get("body").and_then(|b| b.get("id"))
        })
        .collect();
    assert_eq!(parked.len(), 1, "one deferred copy parked");
    assert_eq!(parked[0].payload["seq"], undo_job.payload["seq"]);

    // The first send goes out; only then does the parked copy follow.
    delivery::run(&config, &follow_job).await.expect("follow sends");
    store.complete(follow_job.id).expect("settle follow");
    store.complete(undo_job.id).expect("retire parked original");
    let copy = parked[0].clone();
    let claimed = store.claim_due(now + 30_000, 10).expect("claim copy");
    assert!(claimed.iter().any(|job| job.id == copy.id));
    delivery::run(&config, &copy).await.expect("undo sends");
    store.complete(copy.id).expect("settle undo");

    let arrivals = arrivals.lock().expect("arrivals").clone();
    assert_eq!(arrivals.len(), 2);
    assert!(arrivals[0].ends_with("/follow-1"), "{arrivals:?}");
    assert!(arrivals[1].ends_with("/undo-1"), "{arrivals:?}");
}

/// Restart mid-queue: sequences survive in the payloads and the counter, so
/// the pair still goes out in plan order.
#[tokio::test]
async fn order_survives_restart_mid_queue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sender;
    let inbox;
    let arrivals = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (follow_id, undo_id);
    {
        let store = Arc::new(common::open_store(dir.path()));
        sender = sender_with_keys(&store, "alice");
        inbox = recording_inbox(arrivals.clone()).await;
        let now = now_ms();
        let (follow, undo) = enqueue_pair(&store, sender, &inbox, now);
        follow_id = follow.id;
        undo_id = undo.id;
        // Crash before anything runs: leases, locks and counters all drop.
    }
    // Reopen (the only durable state is the jobs, their seqs and the
    // counter) and run the second job first: it must still park.
    let store = Arc::new(common::open_store(dir.path()));
    let config = federation(&store, dir.path());
    let now = now_ms();
    let claimed = store.claim_due(now + 1, 10).expect("claim");
    // (`mk_account` also enqueues its `UserCreated` job; the pair is found
    // by id among the claimed batch.)
    let find = |id: Id| {
        claimed
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .expect("reclaimed")
    };
    let (follow_job, undo_job) = (find(follow_id), find(undo_id));

    delivery::run(&config, &undo_job).await.expect("parks");
    assert!(
        arrivals.lock().expect("arrivals").is_empty(),
        "restarted queue still holds the second send"
    );

    delivery::run(&config, &follow_job).await.expect("follow sends");
    store.complete(follow_job.id).expect("settle follow");
    store.complete(undo_job.id).expect("retire parked original");
    let parked: Vec<Job> = store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|job| {
            job.kind == JobKind::DeliverActivity
                && job.id != undo_job.id
                && job.id != follow_job.id
        })
        .collect();
    assert_eq!(parked.len(), 1);
    let claimed = store.claim_due(now + 30_000, 10).expect("claim copy");
    assert!(claimed.iter().any(|job| job.id == parked[0].id));
    delivery::run(&config, &parked[0]).await.expect("undo sends");

    let arrivals = arrivals.lock().expect("arrivals").clone();
    assert_eq!(arrivals.len(), 2);
    assert!(arrivals[0].ends_with("/follow-1"), "{arrivals:?}");
    assert!(arrivals[1].ends_with("/undo-1"), "{arrivals:?}");

    // The counter survived too: the next mint continues the sequence.
    let host = delivery::dest_host(
        &url::Url::parse(&inbox).expect("inbox parses"),
    );
    let next = store.delivery_seq_next(&host).expect("counter");
    assert_eq!(next, 3, "two mints before the restart, third after");
}

/// A permanently failed earlier send never wedges its host: terminal rows
/// do not block, so the later sequence still goes out.
#[tokio::test]
async fn quarantined_earlier_sequence_does_not_wedge_host() {
    let (dir, store) = common::temp_store();
    let config = federation(&store, dir.path());
    let now = now_ms();
    let sender = sender_with_keys(&store, "alice");
    let arrivals = Arc::new(std::sync::Mutex::new(Vec::new()));
    let inbox = recording_inbox(arrivals.clone()).await;

    let (follow, undo) = enqueue_pair(&store, sender, &inbox, now);
    // The first send fails terminally (quarantined, never to be sent).
    store
        .quarantine(follow.id, "inbox gone for good")
        .expect("quarantine");
    let claimed = store.claim_due(now + 1, 10).expect("claim");
    let undo_job = claimed
        .iter()
        .find(|job| job.id == undo.id)
        .cloned()
        .expect("undo claimed");

    // The later sequence is not held behind the dead one.
    delivery::run(&config, &undo_job)
        .await
        .expect("undo sends despite the quarantined earlier send");
    let arrivals = arrivals.lock().expect("arrivals").clone();
    assert_eq!(arrivals.len(), 1);
    assert!(arrivals[0].ends_with("/undo-1"), "{arrivals:?}");
}

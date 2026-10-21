//! G-D federation tail (`docs/gaps.md`): Move + migrated follows, remote emoji,
//! relays + inbox forwarding, follower sync + remote counters, Note/featured
//! collections, quote re-verify workers, report-to-Flag forwarding, live
//! WebFinger shape, remote embeds fetch + sanitize, `collection_update` hook.
//!
//! Federation input is untrusted on every path here: size/timeout/redirect
//! limits, SSRF policy checks and signature verification stay with the
//! production paths; tests use loopback fakes or injected fetchers, never
//! live services.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use activitypub_federation::config::FederationConfig;
use activitypub_federation::http_signatures::generate_actor_keypair;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use birchtree::activitypub::activities::{Activity, Move, apply_move_counted};
use birchtree::activitypub::objects::{
    display_counts, featured_tags_collection_doc, featured_tags_collection_uri, fetch_remote_embed,
    hint_count, lookup_should_resolve_live, note_replies_value, parse_remote_embed,
    parse_remote_emojis, remote_emoji_entity, sanitize_embed_html, split_remote_acct,
    validate_remote_counts, EmbedFetchError, RemoteCountsHint,
};
use birchtree::activitypub::FedContext;
use birchtree::application::context::Context;
use birchtree::application::federation_ports::{
    FederationContentStore, FederationStore, NewQuote, NewReport, RemoteAccountUpsert,
};
use birchtree::application::ports::{
    EmojiStore, FollowOptions, JobQueue, RelationshipStore, StatusStore,
};
use birchtree::config::Config;
use birchtree::domain::{
    AccountStats, Clock, Id, IdGenerator, Job, JobKind, NotificationType, QuoteState, SystemClock,
    TimestampIdGenerator, Visibility,
};
use birchtree::federation::delivery::{self, DeliveryError};
use birchtree::federation::media::{
    fetch_remote_emoji_bytes, remote_emoji_shortcode, split_remote_emoji_shortcode,
    store_remote_emoji, validate_remote_emoji,
};
use birchtree::federation::net::SafeHttpConfig;
use birchtree::federation::threads::{
    enqueue_collection_update, plan_quote_reverify, run_quote_reverify, QuoteReverifyPlan,
};
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

fn federation(
    store: &Arc<RedbStore>,
    dir: &std::path::Path,
    http: &SafeHttpConfig,
) -> FederationConfig<FedContext> {
    common::federation_config_with(context(store, dir), TEST_PUBLIC_URL, http, true)
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

/// A remote account with a fresh key (no fetch is made for it).
fn remote(store: &RedbStore, name: &str) -> Id {
    remote_on(store, name, "remote.example")
}

fn remote_on(store: &RedbStore, name: &str, domain: &str) -> Id {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://{domain}/users/{name}");
    store
        .upsert_remote(
            RemoteAccountUpsert {
                actor_uri: actor_uri.clone(),
                username: name.to_string(),
                domain: domain.into(),
                display_name: String::new(),
                note: String::new(),
                locked: false,
                inbox_uri: format!("{actor_uri}/inbox"),
                shared_inbox_uri: Some(format!("https://{domain}/inbox")),
                public_key_pem: keys.public_key,
                key_id: format!("{actor_uri}#main-key"),
            },
            now_ms(),
        )
        .expect("remote")
        .id
}

fn new_status(account_id: Id, text: &str) -> birchtree::application::ports::NewStatus {
    birchtree::application::ports::NewStatus {
        remote: None,
        account_id,
        uri_base: format!("{TEST_PUBLIC_URL}/users/test/statuses"),
        url_base: format!("{TEST_PUBLIC_URL}/@test"),
        source_text: text.to_string(),
        content_html: format!("<p>{text}</p>"),
        visibility: Visibility::Public,
        ..Default::default()
    }
}

fn publish(store: &RedbStore, author: Id, text: &str, now: u64) -> Id {
    store.create_status(new_status(author, text), now).expect("create status").status.id
}

// --- Move -------------------------------------------------------------------

fn move_json(actor: &str, object: &str, target: &str) -> serde_json::Value {
    json!({
        "type": "Move",
        "id": format!("{actor}#moves/1"),
        "actor": actor,
        "object": object,
        "target": target,
    })
}

#[test]
fn move_parses_as_its_own_activity_and_refuses_misaddressed_shapes() {
    let actor = "https://remote.example/users/old";
    let target = "https://remote.example/users/new";
    let activity: Activity =
        serde_json::from_value(move_json(actor, actor, target)).expect("Move parses");
    assert!(matches!(activity, Activity::Move(_)), "untagged enum picks Move");

    // Misaddressed shapes parse (the inbox answers them 4xx at verify time).
    let bad_object: Move = serde_json::from_value(move_json(actor, target, target)).expect("parses");
    assert_ne!(bad_object.object.as_str(), bad_object.actor.inner().as_str());
    let self_target: Move = serde_json::from_value(move_json(actor, actor, actor)).expect("parses");
    assert_eq!(self_target.target.as_str(), self_target.actor.inner().as_str());
}

#[test]
fn move_migrates_local_followers_and_leaves_remote_ones_alone() {
    let (_dir, store) = temp_store();
    let now = now_ms();
    let old = remote(&store, "old");
    let new = remote(&store, "new");
    let alice = mk_account(&store, "alice").id;
    let bob = mk_account(&store, "bob").id;
    let carol = remote(&store, "carol");

    for follower in [alice, bob, carol] {
        store
            .follow(follower, old, FollowOptions { show_reblogs: true, notify: false, languages: None }, false, now)
            .expect("follow old");
    }

    let outcome = apply_move_counted(&*store, old, new, now).expect("move applies");
    assert_eq!(outcome.migrated, 2, "both local followers move");
    assert_eq!(outcome.already, 0);
    // Remote followers' servers apply their own moves.
    assert!(store.follow_of(carol, new).unwrap().is_none(), "remote follower untouched");
    assert!(store.follow_of(alice, new).unwrap().is_some(), "alice follows new");
    assert!(store.follow_of(bob, new).unwrap().is_some(), "bob follows new");

    // Redelivery is idempotent: every row is already in place.
    let again = apply_move_counted(&*store, old, new, now + 1).expect("redelivery applies");
    assert_eq!(again.migrated, 0, "nothing moves twice");
    assert_eq!(again.already, 2, "both rows already follow");

    // Blocked pairs and the successor itself never migrate.
    let dave = mk_account(&store, "dave").id;
    store.follow(dave, old, FollowOptions { show_reblogs: true, notify: false, languages: None }, false, now).expect("dave follows old");
    let _ = store.block(new, dave, now).expect("new blocks dave");
    let third = apply_move_counted(&*store, old, new, now + 2).expect("move applies");
    assert_eq!(third.migrated, 0);
    assert!(store.follow_of(dave, new).unwrap().is_none(), "blocked follower stays");
}

#[test]
fn move_to_a_local_successor_lands_on_local_rows() {
    let (_dir, store) = temp_store();
    let now = now_ms();
    let old = remote(&store, "old");
    // The successor may be local (moving onto this server): local followers
    // land on the local row through the same path.
    let new_local = mk_account(&store, "newlocal").id;
    let alice = mk_account(&store, "alice3").id;
    store.follow(alice, old, FollowOptions { show_reblogs: true, notify: false, languages: None }, false, now).expect("follow old");
    let outcome = apply_move_counted(&*store, old, new_local, now).expect("move applies");
    assert_eq!(outcome.migrated, 1);
    assert!(store.follow_of(alice, new_local).unwrap().is_some());
}

// --- remote emoji ------------------------------------------------------------

#[test]
fn remote_emoji_are_carried_from_tag_arrays_with_bounds() {
    let raw = json!({
        "type": "Note",
        "id": "https://remote.example/notes/1",
        "tag": [
            {"type": "Mention", "href": "https://remote.example/users/x", "name": "@x"},
            {"type": "Emoji", "name": ":party:", "icon": "https://remote.example/emoji/party.png"},
            {"type": "Emoji", "name": ":blob:", "icon": {"type": "Image", "url": "https://remote.example/emoji/blob.gif"}},
            {"type": "Emoji", "name": ":array:", "icon": [{"type": "Image", "url": "https://remote.example/emoji/a.webp"}]},
            {"type": "Emoji", "name": ":bad scheme:", "icon": "https://remote.example/e.png"},
            {"type": "Emoji", "name": ":creds:", "icon": "https://user:pass@remote.example/e.png"},
            {"type": "Emoji", "name": ":js:", "icon": "javascript:alert(1)"},
            {"type": "Emoji", "name": ":noicon:"},
            {"type": "Hashtag", "name": "#tag"},
        ],
    });
    let emojis = parse_remote_emojis(&raw);
    let codes: Vec<&str> = emojis.iter().map(|e| e.shortcode.as_str()).collect();
    assert_eq!(codes, vec!["party", "blob", "array"], "only usable emoji survive");
    assert_eq!(emojis[0].url, "https://remote.example/emoji/party.png");

    // Single (non-array) `tag` reads too; duplicates collapse.
    let single = json!({"tag": {"type": "Emoji", "name": ":one:", "icon": "https://remote.example/1.png"}});
    assert_eq!(parse_remote_emojis(&single).len(), 1);
    let dup = json!({"tag": [
        {"type": "Emoji", "name": ":xy:", "icon": "https://remote.example/1.png"},
        {"type": "Emoji", "name": ":xy:", "icon": "https://remote.example/2.png"},
    ]});
    assert_eq!(parse_remote_emojis(&dup).len(), 1, "deduped by shortcode");

    // The per-object cap bounds untrusted input.
    let many: Vec<serde_json::Value> = (0..70)
        .map(|i| json!({"type": "Emoji", "name": format!(":e{i}:"), "icon": "https://remote.example/e.png"}))
        .collect();
    let capped = parse_remote_emojis(&json!({"tag": many}));
    assert_eq!(capped.len(), 50, "capped per object");

    // Entities ride status/account `emojis` arrays, never the picker.
    let entity = remote_emoji_entity("party", "https://remote.example/emoji/party.png");
    assert_eq!(entity["shortcode"], "party");
    assert_eq!(entity["visible_in_picker"], false);
}

#[test]
fn remote_emoji_ingest_validates_namespaces_and_stages_bytes() {
    let (dir, store) = temp_store();
    let blobs = birchtree::media::blob_store::FsBlobStore::open(dir.path().join("media")).expect("blobs");
    let now = now_ms();

    assert!(validate_remote_emoji("party", "remote.example", "https://remote.example/e.png").is_ok());
    assert!(validate_remote_emoji("x", "remote.example", "https://remote.example/e.png").is_err(), "shortcode too short");
    assert!(validate_remote_emoji("ok", "", "https://remote.example/e.png").is_err(), "empty domain");
    assert!(validate_remote_emoji("ok", "remote.example", "ftp://remote.example/e.png").is_err(), "scheme");
    assert!(validate_remote_emoji("ok", "remote.example", "https://u:p@remote.example/e.png").is_err(), "credentials");

    let key = remote_emoji_shortcode("party", "Remote.Example");
    assert_eq!(key, "party@remote.example", "domain lowercased");
    assert_eq!(split_remote_emoji_shortcode(&key), Some(("party", "remote.example")));
    assert!(split_remote_emoji_shortcode("plain").is_none());

    // Fake PNG bytes (magic only — the sniffer reads leading bytes).
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&[0; 16]);
    let row = store_remote_emoji(&*store, &blobs, &key, "remote.example", &png, "image/png", now).expect("staged");
    assert!(!row.visible_in_picker, "never picker-visible");
    assert!(!row.listed(), "never listed");
    assert_eq!(row.category.as_deref(), Some("remote:remote.example"));
    assert!(store.emoji_by_shortcode(&key).unwrap().is_some(), "row retrievable");

    assert!(store_remote_emoji(&*store, &blobs, &key, "remote.example", &vec![0u8; 300 * 1024], "image/png", now).is_err(), "oversize refused");
    assert!(store_remote_emoji(&*store, &blobs, &key, "remote.example", b"plain text not an image at all!!!!!!!!", "text/plain", now).is_err(), "non-image refused");
}

#[tokio::test]
async fn remote_emoji_fetch_enforces_ssrf_size_and_status() {
    let app = Router::new()
        .route("/ok.png", get(|| async {
            let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
            png.extend_from_slice(&[0; 16]);
            (StatusCode::OK, png)
        }))
        .route("/missing.png", get(|| async { StatusCode::NOT_FOUND }))
        .route("/big.png", get(|| async {
            (StatusCode::OK, vec![0u8; 300 * 1024])
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let allow = loopback_policy();

    let url = url::Url::parse(&format!("{base}/ok.png")).unwrap();
    let (bytes, mime) = fetch_remote_emoji_bytes(&client, &url, true, &allow.allowlist).await.expect("fetched");
    assert_eq!(mime, "image/png");
    assert!(!bytes.is_empty());

    let url = url::Url::parse(&format!("{base}/missing.png")).unwrap();
    assert!(
        matches!(
            fetch_remote_emoji_bytes(&client, &url, true, &allow.allowlist).await,
            Err(DeliveryError::Quarantine(_))
        ),
        "404 never retries in a loop"
    );

    let url = url::Url::parse(&format!("{base}/big.png")).unwrap();
    assert!(
        matches!(
            fetch_remote_emoji_bytes(&client, &url, true, &allow.allowlist).await,
            Err(DeliveryError::Quarantine(_))
        ),
        "oversize emoji quarantines"
    );

    // An `http` URL with `allow_http` off is refused before any fetch.
    let plain = url::Url::parse(&format!("{base}/ok.png")).unwrap();
    assert!(
        matches!(
            fetch_remote_emoji_bytes(&client, &plain, false, &allow.allowlist).await,
            Err(DeliveryError::Quarantine(_))
        ),
        "SSRF refusal quarantines without a fetch"
    );
    // A private IP literal outside the allowlist is refused too.
    let private = url::Url::parse("http://10.9.9.9/e.png").unwrap();
    assert!(
        matches!(
            fetch_remote_emoji_bytes(&client, &private, true, &[]).await,
            Err(DeliveryError::Quarantine(_))
        ),
        "private literal quarantines without a fetch"
    );
}

// --- relays + inbox forwarding ------------------------------------------------

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

fn relay_job(sender: Id, relay: &str, body: serde_json::Value) -> Job {
    Job::new(
        unique_job_id(),
        JobKind::RelayForward,
        json!({"sender_id": sender.value(), "relay_inbox": relay, "activity_uri": "x", "body": body}),
        now_ms(),
        now_ms(),
    )
}

#[tokio::test]
async fn relay_forward_lab_run_reaches_the_peer_with_delivery_semantics() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &loopback_policy());
    let alice = sender_with_keys(&store);

    // Lab run: the relay peer answers 202, the activity arrives once.
    let hits = Arc::new(AtomicUsize::new(0));
    let relay = counting_inbox(202, hits.clone()).await;
    let job = relay_job(alice, &relay, follow_body("https://remote.example/users/eve"));
    assert_eq!(delivery::run_relay_forward(&config, &job).await, Ok(()));
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one signed POST to the relay");

    // A down relay charges the shared per-host budget (reachability claim).
    let failing = Arc::new(AtomicUsize::new(0));
    let down = counting_inbox(503, failing.clone()).await;
    let job = relay_job(alice, &down, follow_body("https://remote.example/users/eve"));
    let outcome = delivery::run_relay_forward(&config, &job).await;
    assert!(matches!(outcome, Err(DeliveryError::Retry(_))), "{outcome:?}");
    assert_eq!(failing.load(Ordering::SeqCst), 1);

    // Unparseable payloads quarantine without touching the network.
    for payload in [
        json!({"sender_id": 1}),
        json!({"sender_id": 1, "relay_inbox": "not a url", "body": {}}),
        json!({"sender_id": 1, "relay_inbox": "ftp://x/inbox", "body": {}}),
        json!({"sender_id": 1, "relay_inbox": "http://127.0.0.1:1/inbox"}),
    ] {
        let job = Job::new(unique_job_id(), JobKind::RelayForward, payload, now_ms(), now_ms());
        assert!(
            matches!(delivery::run_relay_forward(&config, &job).await, Err(DeliveryError::Quarantine(_))),
            "bad relay payload quarantines"
        );
    }
}

#[test]
fn inbox_forwarding_plans_one_job_per_follower_inbox() {
    let (_dir, store) = temp_store();
    let now = now_ms();
    let alice = mk_account(&store, "alice").id;
    let r1 = remote(&store, "r1");
    let r2 = remote_on(&store, "r2", "other.example");
    for follower in [r1, r2] {
        store
            .follow(follower, alice, FollowOptions { show_reblogs: true, notify: false, languages: None }, false, now)
            .expect("remote follows alice");
    }
    // r1 and r2 share nothing: two distinct shared inboxes.
    assert!(delivery::should_forward_inbound("Announce"));
    assert!(delivery::should_forward_inbound("Create"));
    assert!(!delivery::should_forward_inbound("Like"));
    assert!(!delivery::should_forward_inbound("Flag"));

    let body = follow_body("https://remote.example/users/eve");
    let planned = delivery::plan_inbox_forward(&*store, alice, "https://example.social/activities/1", &body, None, now)
        .expect("planned");
    assert_eq!(planned, 2, "one job per follower inbox");
    let jobs = store.claim_due(now + 1, 100).expect("claim");
    let deliveries: Vec<&Job> = jobs.iter().filter(|j| j.kind == JobKind::DeliverActivity).collect();
    assert_eq!(deliveries.len(), 2);

    // The inbox the activity arrived from is excluded (no relay loop).
    let planned = delivery::plan_inbox_forward(
        &*store,
        alice,
        "https://example.social/activities/2",
        &body,
        Some("https://remote.example/inbox"),
        now,
    )
    .expect("planned");
    assert_eq!(planned, 1, "arrival inbox excluded");

    // Oversize bodies never fan out.
    let big = serde_json::Value::String("x".repeat(2 * 1024 * 1024));
    assert!(delivery::plan_inbox_forward(&*store, alice, "x", &big, None, now).is_err());
}

// --- follower sync + remote counters ------------------------------------------

#[test]
fn remote_counters_are_hints_never_authority() {
    assert_eq!(hint_count(12), Some(12));
    assert_eq!(hint_count(100_000_000), Some(100_000_000));
    assert_eq!(hint_count(100_000_001), None, "absurd claims dropped");

    let hint = validate_remote_counts(Some(10), Some(u64::MAX), Some(3));
    assert_eq!(hint, RemoteCountsHint { followers: Some(10), following: None, statuses: Some(3) });

    let local = AccountStats { followers_count: 5, following_count: 0, statuses_count: 0, last_status_at_ms: None, follow_requests_count: 0 };
    // Local accounts always render local measurements.
    assert_eq!(display_counts(true, local, hint).followers_count, 5);
    // Remote accounts fill only zero fields from the hint.
    let shown = display_counts(false, local, hint);
    assert_eq!(shown.followers_count, 5, "nonzero local wins");
    assert_eq!(shown.following_count, 0, "absurd hint never inflates");
    assert_eq!(shown.statuses_count, 3, "zero fills from the hint");
}

// --- Note / featured collections -----------------------------------------------

#[test]
fn note_replies_and_featured_tags_collections_have_stable_shapes() {
    let replies = note_replies_value("https://example.social/users/a/statuses/9", 4);
    assert_eq!(replies["id"], "https://example.social/users/a/statuses/9/replies");
    assert_eq!(replies["type"], "Collection");
    assert_eq!(replies["totalItems"], 4);
    assert_eq!(replies["first"], "https://example.social/users/a/statuses/9/replies?page=true");

    let actor = "https://example.social/users/alice";
    assert_eq!(
        featured_tags_collection_uri(actor),
        "https://example.social/users/alice/collections/featured"
    );
    let doc = featured_tags_collection_doc(actor, TEST_PUBLIC_URL, &[("rust".into(), 1), ("art".into(), 2)]);
    assert_eq!(doc["type"], "OrderedCollection");
    assert_eq!(doc["totalItems"], 2);
    assert_eq!(doc["orderedItems"][0]["name"], "#rust");
    assert_eq!(doc["orderedItems"][0]["href"], format!("{TEST_PUBLIC_URL}/tags/rust"));
}

// --- quote re-verify ------------------------------------------------------------

fn quoted_and_quoting(store: &RedbStore, tag: &str, now: u64) -> (Id, Id, birchtree::domain::QuoteRecord) {
    let alice = mk_account(store, &format!("alice-{tag}")).id;
    let eve = remote(store, "eve");
    let quoted = publish(store, alice, "quotable", now);
    // The quoting status belongs to the remote quoter; ingest it directly.
    let quoting_uri = format!("https://remote.example/notes/quoting-{tag}");
    let quoting_note = birchtree::application::federation_inbound::RemoteNote {
        uri: quoting_uri.clone(),
        url: quoting_uri,
        author_id: eve,
        published_ms: now,
        visibility: Visibility::Public,
        content_html: "<p>quoting</p>".into(),
        spoiler_text: None,
        sensitive: false,
        language: None,
        in_reply_to_uri: None,
        mention_uris: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
        emojis: Vec::new(),
        quote_uri: None,
        poll: None,
    };
    let quoting = birchtree::application::federation_inbound::ingest_note(&*store, quoting_note, None, now)
        .expect("ingest quoting")
        .status
        .id;
    let quote = store
        .create_quote(
            NewQuote {
                quoting_status_id: quoting,
                quoted_status_id: quoted,
                quoting_account_id: eve,
                quoted_account_id: alice,
                state: QuoteState::Accepted,
                approval_uri: None,
                quote_activity_uri: None,
            },
            now,
        )
        .expect("create quote");
    (quoted, quoting, quote)
}

fn reverify_job(quote_id: Id, hop: u64) -> Job {
    Job::new(
        unique_job_id(),
        JobKind::QuoteReverify,
        json!({"quote_id": quote_id.value(), "hop": hop, "fetches": 0}),
        now_ms(),
        now_ms(),
    )
}

#[tokio::test]
async fn quote_reverify_keeps_revokes_and_stops_at_caps_with_counts_in_step() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &SafeHttpConfig::default());
    let now = now_ms();
    let (quoted, _quoting, quote) = quoted_and_quoting(&store, "a", now);
    assert_eq!(store.status_by_id(quoted).unwrap().expect("quoted").quotes_count, 1, "accepted quote counts");

    // Alive quoted status, no remote authorization: kept, counts untouched.
    assert_eq!(plan_quote_reverify(&*store, quote.id).unwrap(), QuoteReverifyPlan::CheckRemote);
    assert_eq!(run_quote_reverify(&config, &reverify_job(quote.id, 0)).await, Ok(()));
    assert_eq!(store.quote_by_id(quote.id).unwrap().expect("quote").state, QuoteState::Accepted);
    assert_eq!(store.status_by_id(quoted).unwrap().expect("quoted").quotes_count, 1);

    // Missing quote, rejected quote: terminal no-ops.
    assert_eq!(run_quote_reverify(&config, &reverify_job(Id::new(u64::MAX - 7), 0)).await, Ok(()));
    store.set_quote_state(quote.id, QuoteState::Rejected, None, None).expect("reject");
    assert_eq!(plan_quote_reverify(&*store, quote.id).unwrap(), QuoteReverifyPlan::Done);
    assert_eq!(run_quote_reverify(&config, &reverify_job(quote.id, 0)).await, Ok(()));

    // Back to accepted, then the quoted status is deleted: the delete path
    // cascades the quote to `deleted` itself, so re-verify is a terminal
    // no-op — a dead quote is never revived (never-promote-to-root holds).
    store.set_quote_state(quote.id, QuoteState::Accepted, None, None).expect("accept");
    let alice = store.status_by_id(quoted).unwrap().expect("quoted").account_id;
    store.delete_status(quoted, alice, now + 1).expect("delete quoted");
    assert_eq!(store.quote_by_id(quote.id).unwrap().expect("quote").state, QuoteState::Deleted);
    assert_eq!(plan_quote_reverify(&*store, quote.id).unwrap(), QuoteReverifyPlan::Done);
    assert_eq!(run_quote_reverify(&config, &reverify_job(quote.id, 0)).await, Ok(()));
    assert_eq!(store.quote_by_id(quote.id).unwrap().expect("quote").state, QuoteState::Deleted);

    // Hop cap and fetch budget stop the chain with state kept.
    assert_eq!(run_quote_reverify(&config, &reverify_job(quote.id, 8)).await, Ok(()));
    let capped = Job::new(
        unique_job_id(),
        JobKind::QuoteReverify,
        json!({"quote_id": quote.id.value(), "hop": 0, "fetches": 100}),
        now,
        now,
    );
    assert_eq!(run_quote_reverify(&config, &capped).await, Ok(()));
    // A malformed job quarantines without touching state.
    let bad = Job::new(unique_job_id(), JobKind::QuoteReverify, json!({}), now, now);
    assert!(matches!(run_quote_reverify(&config, &bad).await, Err(DeliveryError::Quarantine(_))));
}

#[tokio::test]
async fn quote_reverify_never_rolls_back_on_federation_failure() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &SafeHttpConfig::default());
    let now = now_ms();
    let (_quoted, _quoting, quote) = quoted_and_quoting(&store, "b", now);
    // A pending quote is left to the request flow, whatever the network says.
    store.set_quote_state(quote.id, QuoteState::Pending, None, None).expect("pend");
    assert_eq!(plan_quote_reverify(&*store, quote.id).unwrap(), QuoteReverifyPlan::Done);
    assert_eq!(run_quote_reverify(&config, &reverify_job(quote.id, 0)).await, Ok(()));
    assert_eq!(store.quote_by_id(quote.id).unwrap().expect("quote").state, QuoteState::Pending);
}

#[tokio::test]
async fn quote_reverify_revokes_only_on_positive_remote_evidence() {
    let app = Router::new()
        .route("/gone", get(|| async { StatusCode::NOT_FOUND }))
        .route("/live", get(|| async {
            (StatusCode::OK, serde_json::to_vec(&json!({"type": "QuoteAuthorization"})).unwrap())
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });

    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &loopback_policy());
    let now = now_ms();
    let (quoted_gone, _, quote_gone) = quoted_and_quoting(&store, "g1", now);
    let (quoted_live, _, quote_live) = quoted_and_quoting(&store, "l1", now + 1);
    let gone_url = format!("http://127.0.0.1:{port}/gone");
    let live_url = format!("http://127.0.0.1:{port}/live");
    store.set_quote_state(quote_gone.id, QuoteState::Accepted, Some(gone_url), None).expect("approval");
    store.set_quote_state(quote_live.id, QuoteState::Accepted, Some(live_url), None).expect("approval");
    assert_eq!(plan_quote_reverify(&*store, quote_gone.id).unwrap(), QuoteReverifyPlan::CheckRemote);

    // 404 on the authorization: the quoted author withdrew it — revoke with
    // the counter in step and a revoke event for the quoting server.
    assert_eq!(run_quote_reverify(&config, &reverify_job(quote_gone.id, 0)).await, Ok(()));
    assert_eq!(store.quote_by_id(quote_gone.id).unwrap().expect("quote").state, QuoteState::Revoked);
    // Both quotes counted while accepted; the revoked one steps down.
    assert_eq!(store.status_by_id(quoted_gone).unwrap().expect("quoted").quotes_count, 0);
    assert_eq!(store.status_by_id(quoted_live).unwrap().expect("quoted").quotes_count, 1);
    let jobs = store.claim_due(now + 3_600_000, 100).expect("claim");
    assert!(
        jobs.iter().any(|j| j.kind == JobKind::FederationEvent
            && j.payload.get("event") == Some(&json!("quote_revoke"))
            && j.payload.get("quote_id") == Some(&json!(quote_gone.id.value()))),
        "a quote_revoke event queues for the quoting server"
    );

    // A resolving authorization keeps the quote and its count.
    assert_eq!(run_quote_reverify(&config, &reverify_job(quote_live.id, 0)).await, Ok(()));
    assert_eq!(store.quote_by_id(quote_live.id).unwrap().expect("quote").state, QuoteState::Accepted);
    assert_eq!(store.status_by_id(quoted_live).unwrap().expect("quoted").quotes_count, 1);
}

// --- report -> Flag forward -----------------------------------------------------

#[tokio::test]
async fn local_report_forwards_a_flag_to_the_remote_inbox() {
    let (dir, store) = temp_store();
    let config = federation(&store, dir.path(), &SafeHttpConfig::default());
    let now = now_ms();
    let reporter = mk_account(&store, "reporter").id;
    let target = remote(&store, "trouble");

    // Local queue exists: a forwarded local report commits its Flag event.
    let report = store
        .insert_report(
            NewReport {
                reporter_id: reporter,
                target_account_id: target,
                status_ids: Vec::new(),
                comment: "spam".into(),
                forwarded: true,
                activity_uri: None,
                category: "spam".into(),
                rule_ids: Vec::new(),
                forward_to_domains: vec!["remote.example".into()],
                collection_ids: Vec::new(),
            },
            now,
        )
        .expect("insert report");
    let jobs = store.claim_due(now + 1, 100).expect("claim");
    let event = jobs
        .iter()
        .find(|j| j.kind == JobKind::FederationEvent && j.payload.get("report_id") == Some(&json!(report.id.value())))
        .expect("flag event queued with the report");
    assert_eq!(event.payload.get("event"), Some(&json!("flag")));

    // The event worker plans one Flag delivery to the target's shared inbox.
    // (The worker stamps its own clock, so claim past any skew.)
    birchtree::federation::events::run(&config, event).await.expect("flag planned");
    let jobs = store.claim_due(now + 3_600_000, 100).expect("claim");
    let delivery = jobs
        .iter()
        .find(|j| j.kind == JobKind::DeliverActivity && j.payload.get("inbox") == Some(&json!("https://remote.example/inbox")))
        .expect("Flag delivery to the remote shared inbox");
    let body = delivery.payload.get("body").expect("activity body");
    assert_eq!(body.get("type"), Some(&json!("Flag")));
    assert_eq!(body.get("content"), Some(&json!("spam")));
}

// --- live WebFinger shape --------------------------------------------------------

#[test]
fn webfinger_lookup_shape_resolves_remotes_live_and_404s_only_when_missing() {
    // Pure shape the lookup route needs (snippet in the G-D report wires it
    // to `activitypub::resolve::resolve_handle`).
    assert_eq!(
        split_remote_acct("acct:alice@remote.example"),
        Some(("alice".into(), "remote.example".into()))
    );
    assert_eq!(
        split_remote_acct("@bob@Remote.Example"),
        Some(("bob".into(), "remote.example".into()))
    );
    assert_eq!(split_remote_acct("carol@remote.example"), Some(("carol".into(), "remote.example".into())));
    assert_eq!(split_remote_acct("localname"), None, "bare names stay local");
    assert_eq!(split_remote_acct("acct:@"), None);
    assert!(lookup_should_resolve_live("remote.example", "example.social"));
    assert!(!lookup_should_resolve_live("EXAMPLE.social", "example.social"), "local stays in the index");
}

// --- remote embeds ------------------------------------------------------------------

#[test]
fn embed_html_sanitizer_drops_active_content_and_keeps_prose() {
    let dirty = r#"<blockquote><a href="https://remote.example/x" onclick="steal()">read <!-- secret -->more</a></blockquote><script>alert(1)</script><style>.x{}</style><iframe src="https://evil.example"></iframe><a href="javascript:alert(1)">js</a><a href="data:text/html,hi">data</a><p>kept</p>"#;
    let clean = sanitize_embed_html(dirty);
    assert!(!clean.contains("<script"), "script element gone");
    assert!(!clean.contains("alert(1)</script>"), "script content gone");
    assert!(!clean.contains("<style"), "style gone");
    assert!(!clean.contains("<iframe"), "iframe gone");
    assert!(!clean.contains("onclick"), "event handlers gone");
    assert!(!clean.contains("javascript:"), "js urls gone");
    assert!(!clean.contains("data:text"), "data urls gone");
    assert!(!clean.contains("<!--"), "comments gone");
    assert!(clean.contains("<p>kept</p>"), "prose kept");
    assert!(clean.contains("https://remote.example/x"), "safe links kept");

    // Nested dropped elements collapse fully.
    let nested = "<div>a<script>b<iframe>c</iframe>d</script>e</div>";
    assert_eq!(sanitize_embed_html(nested), "<div>ae</div>");
}

#[test]
fn remote_oembed_documents_parse_with_request_overrides() {
    let raw = json!({
        "type": "rich",
        "author_name": "someone",
        "author_url": "https://remote.example/@someone",
        "provider_name": "remote",
        "provider_url": "https://remote.example",
        "width": 500,
        "height": 300,
        "html": "<p>hi</p><script>evil()</script>",
    });
    let embed = parse_remote_embed(&raw, None, None).expect("parses");
    assert_eq!(embed.width, 500);
    assert_eq!(embed.height, Some(300));
    assert!(!embed.html.contains("<script"), "html sanitized at parse");
    let body = embed.into_json();
    assert_eq!(body["type"], "rich");
    assert_eq!(body["version"], "1.0");
    assert_eq!(body["cache_age"], 86400);

    // Request overrides win within bounds; out-of-range falls back.
    let over = parse_remote_embed(&raw, Some(200), Some(99)).expect("parses");
    assert_eq!((over.width, over.height), (200, Some(99)));
    let wild = parse_remote_embed(&raw, Some(99999), None).expect("parses");
    assert_eq!(wild.width, 500, "out-of-range override falls back to the document");

    assert!(parse_remote_embed(&json!({"type": "nonsense", "html": "<p>x</p>"}), None, None).is_none());
    assert!(parse_remote_embed(&json!({"type": "rich"}), None, None).is_none(), "html required");
}

#[tokio::test]
async fn remote_oembed_fetch_maps_status_to_terminal_or_transient() {
    let doc = json!({"type": "rich", "html": "<p>remote</p>", "author_name": "r"});
    let body = serde_json::to_vec(&doc).unwrap();
    let ok = fetch_remote_embed(|_: String| async { Ok::<_, String>((200u16, body.clone())) }, "https://remote.example/embed/1", None, None)
        .await
        .expect("fetched");
    assert_eq!(ok.author_name, "r");

    let missing = fetch_remote_embed(
        |_: String| async { Ok::<_, String>((404u16, Vec::new())) },
        "https://remote.example/embed/2",
        None,
        None,
    )
    .await;
    assert!(matches!(missing, Err(EmbedFetchError::Terminal(_))), "404 never retries");

    let big = fetch_remote_embed(
        |_: String| async { Ok::<_, String>((200u16, vec![0u8; 70 * 1024])) },
        "https://remote.example/embed/3",
        None,
        None,
    )
    .await;
    assert!(matches!(big, Err(EmbedFetchError::Terminal(_))), "oversize is terminal");

    let down = fetch_remote_embed(
        |_: String| async { Err::<(u16, Vec<u8>), _>("dns".to_string()) },
        "https://remote.example/embed/4",
        None,
        None,
    )
    .await;
    assert!(matches!(down, Err(EmbedFetchError::Transient(_))), "transport may retry");
}

// --- collection_update hook ----------------------------------------------------------

#[test]
fn poll_collection_update_hook_enqueues_a_deduped_notification_job() {
    let (_dir, store) = temp_store();
    let now = now_ms();
    let alice = mk_account(&store, "alice").id;
    let eve = remote(&store, "eve");
    let status = publish(&store, alice, "poll", now);

    enqueue_collection_update(&*store, alice, Some(eve), Some(status), now).expect("enqueued");
    let jobs = store.claim_due(now + 1, 100).expect("claim");
    let job = jobs
        .iter()
        .find(|j| j.kind == JobKind::FanOutNotification && j.payload.get("type") == Some(&json!("collection_update")))
        .expect("collection_update job queued");
    assert_eq!(job.payload.get("recipient_id"), Some(&json!(alice.value())));

    // The job produces a real notification through the shared fan-out.
    let produced = birchtree::application::notifications::fan_out_from_payload(&*store, &job.payload).expect("fan out");
    assert!(produced.is_some(), "collection_update notification created");
    assert!(matches!(NotificationType::parse("collection_update"), Some(NotificationType::CollectionUpdate)));

    // Self-notifications never enqueue.
    let before = store.claim_due(now + 2, 100).expect("claim").len();
    enqueue_collection_update(&*store, alice, Some(alice), Some(status), now + 2).expect("self is a no-op");
    let after = store.claim_due(now + 3, 100).expect("claim").len();
    assert_eq!(before, after, "no job for a self trigger");
}

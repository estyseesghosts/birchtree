//! Round 3 area D: remote public timeline proof.
//!
//! Verdict under test: WORKING already. Remote-ingested statuses land in the
//! public index but not the local index
//! (`src/persistence/redb/statuses.rs` `maintain_timeline_index`), the
//! `remote=true` read path serves them
//! (`src/persistence/redb/timelines.rs` `remote_only` branch), and the
//! `public:remote` (+media) streams emit them
//! (`src/streaming/fanout.rs` `public_selected` + `status_frames`).
//! Upstream equivalents: `app/models/public_feed.rb` (`remote_only_scope`),
//! `app/services/fan_out_on_write_service.rb` (`broadcast_to_public_streams!`
//! publishes `timeline:public:remote` / `:remote:media` for non-local
//! statuses), `streaming/index.js` (`public:remote`, `public:remote:media`).

mod common;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use activitypub_federation::http_signatures::generate_actor_keypair;
use birchtree::application::federation_inbound::{self as inbound, RemoteNote};
use birchtree::application::federation_ports::{FederationStore, RemoteAccountUpsert};
use birchtree::application::ports::{
    NewStatus, RelationshipStore, StatusStore, StreamingSink, TimelineQuery,
};
use birchtree::application::streaming::StreamEvent;
use birchtree::application::timelines;
use birchtree::domain::{Clock, Id, IdGenerator, Status, SystemClock, TimestampIdGenerator, Visibility};
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::channels::{Locality, StreamKind};
use birchtree::streaming::{Hub, Limits};
use common::{mk_account, temp_store};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn query() -> TimelineQuery {
    TimelineQuery {
        limit: 20,
        ..TimelineQuery::default()
    }
}

/// A remote account with a fresh key (no network fetch is made for it).
fn remote_account(store: &RedbStore, name: &str) -> Id {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://remote.example/users/{name}");
    store
        .upsert_remote(
            RemoteAccountUpsert {
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

fn remote_note(author: Id, uri: &str, visibility: Visibility) -> RemoteNote {
    RemoteNote {
        uri: uri.into(),
        url: uri.into(),
        author_id: author,
        published_ms: now_ms(),
        visibility,
        content_html: format!("<p>{uri}</p>"),
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

fn local_status(store: &RedbStore, author: Id, text: &str) -> Status {
    store
        .create_status(
            NewStatus {
                account_id: author,
                uri_base: "https://example.social/users/alice/statuses".into(),
                url_base: "https://example.social/@alice".into(),
                source_text: text.into(),
                content_html: format!("<p>{text}</p>"),
                visibility: Visibility::Public,
                ..NewStatus::default()
            },
            now_ms(),
        )
        .expect("status")
        .status
}

fn ids(page: &[Status]) -> Vec<Id> {
    page.iter().map(|s| s.id).collect()
}

#[test]
fn remote_ingest_lands_on_remote_public_but_not_on_local() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let local = local_status(&store, alice.id, "local hello");

    let eve = remote_account(&store, "eve");
    let ingested = inbound::ingest_note(
        &*store,
        remote_note(eve, "https://remote.example/notes/1", Visibility::Public),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;

    // `remote=true` serves the remote status and hides the local one.
    let remote_page = timelines::public(
        &*store,
        None,
        TimelineQuery { remote_only: true, ..query() },
        now_ms(),
    )
    .expect("remote page");
    assert!(
        ids(&remote_page.statuses).contains(&ingested.id),
        "remote-ingested status is on public?remote=true: {:?}",
        ids(&remote_page.statuses)
    );
    assert!(
        !ids(&remote_page.statuses).contains(&local.id),
        "local status is not on public?remote=true"
    );

    // `local=true` is unaffected: local present, remote absent.
    let local_page = timelines::public(
        &*store,
        None,
        TimelineQuery { local_only: true, ..query() },
        now_ms(),
    )
    .expect("local page");
    assert!(ids(&local_page.statuses).contains(&local.id));
    assert!(!ids(&local_page.statuses).contains(&ingested.id));

    // The unscoped public timeline carries both.
    let all = timelines::public(&*store, None, query(), now_ms()).expect("all");
    assert!(ids(&all.statuses).contains(&local.id));
    assert!(ids(&all.statuses).contains(&ingested.id));
}

#[test]
fn remote_public_keeps_only_media_and_visibility_gating() {
    let (_dir, store) = temp_store();
    let eve = remote_account(&store, "eve");
    let plain = inbound::ingest_note(
        &*store,
        remote_note(eve, "https://remote.example/notes/10", Visibility::Public),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;

    // A remote status with a staged attachment (bytes fetch later; the row
    // exists now, which is what the media variant keys on).
    let mut with_media_note =
        remote_note(eve, "https://remote.example/notes/11", Visibility::Public);
    with_media_note.attachments.push(birchtree::application::federation_inbound::RemoteAttachment {
        source_url: "https://remote.example/media/1.png".into(),
        media_type: "image/png".into(),
        description: String::new(),
        license: None,
    });
    let with_media =
        inbound::ingest_note(&*store, with_media_note, None, now_ms()).expect("ingest").status;
    assert!(!with_media.media_ids.is_empty(), "attachment row staged");

    let media_page = timelines::public(
        &*store,
        None,
        TimelineQuery { remote_only: true, only_media: true, ..query() },
        now_ms(),
    )
    .expect("media page");
    assert!(ids(&media_page.statuses).contains(&with_media.id));
    assert!(!ids(&media_page.statuses).contains(&plain.id));

    // A non-public remote status never reaches the remote public timeline.
    let followers_only = inbound::ingest_note(
        &*store,
        remote_note(eve, "https://remote.example/notes/12", Visibility::Private),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;
    let remote_page = timelines::public(
        &*store,
        None,
        TimelineQuery { remote_only: true, ..query() },
        now_ms(),
    )
    .expect("remote page");
    assert!(!ids(&remote_page.statuses).contains(&followers_only.id));
}

#[test]
fn remote_public_preserves_block_mute_and_domain_block_gating() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let eve = remote_account(&store, "eve");
    let mallory = remote_account(&store, "mallory");
    let nancy = remote_account(&store, "nancy");

    let from_eve = inbound::ingest_note(
        &*store,
        remote_note(eve, "https://remote.example/notes/20", Visibility::Public),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;
    let from_mallory = inbound::ingest_note(
        &*store,
        remote_note(mallory, "https://remote.example/notes/21", Visibility::Public),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;
    let from_nancy = inbound::ingest_note(
        &*store,
        remote_note(nancy, "https://remote.example/notes/22", Visibility::Public),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;
    let _ = bob;

    store.block(alice.id, eve, now_ms()).expect("block");
    store.mute(alice.id, mallory, 0, false, now_ms()).expect("mute");
    store.domain_block(alice.id, "remote.example", now_ms()).expect("domain block");
    // Domain block covers nancy's domain too, but lift the per-account gates
    // off nancy's row is impossible; instead unblock the domain and check the
    // per-account gates individually below. First: with the domain block on,
    // everything remote is gone for alice.
    let gated = timelines::public(
        &*store,
        Some(alice.id),
        TimelineQuery { remote_only: true, ..query() },
        now_ms(),
    )
    .expect("page");
    assert!(gated.statuses.is_empty(), "domain block hides all remote: {:?}", ids(&gated.statuses));

    store.unblock_domain(alice.id, "remote.example").expect("unblock domain");
    let gated = timelines::public(
        &*store,
        Some(alice.id),
        TimelineQuery { remote_only: true, ..query() },
        now_ms(),
    )
    .expect("page");
    let got = ids(&gated.statuses);
    assert!(!got.contains(&from_eve.id), "blocked author hidden");
    assert!(!got.contains(&from_mallory.id), "muted author hidden");
    assert!(got.contains(&from_nancy.id), "unrelated remote author kept");

    // Anonymous viewers are unaffected by alice's gates.
    let anon = timelines::public(
        &*store,
        None,
        TimelineQuery { remote_only: true, ..query() },
        now_ms(),
    )
    .expect("page");
    let anon_ids = ids(&anon.statuses);
    assert!(anon_ids.contains(&from_eve.id));
    assert!(anon_ids.contains(&from_mallory.id));

    // The streaming gate agrees with the REST gate for the blocked viewer.
    assert!(
        !timelines::public_stream_includes(&*store, Some(alice.id), &from_eve, now_ms())
            .expect("gate"),
        "stream gate hides blocked author's remote status"
    );
    assert!(
        timelines::public_stream_includes(&*store, Some(alice.id), &from_nancy, now_ms())
            .expect("gate"),
        "stream gate keeps unrelated remote status"
    );
    assert!(
        timelines::public_stream_includes(&*store, None, &from_eve, now_ms()).expect("gate"),
        "anonymous stream gate keeps it"
    );
}

#[tokio::test]
async fn streaming_public_remote_emits_remote_statuses_only() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let eve = remote_account(&store, "eve");
    let remote_status = inbound::ingest_note(
        &*store,
        remote_note(eve, "https://remote.example/notes/30", Visibility::Public),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;
    let local_status = local_status(&store, alice.id, "local hello");

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> =
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let _ = ids;
    let hub = Hub::new(
        store.clone() as Arc<dyn birchtree::application::ports::Store>,
        clock,
        common::server_info(),
        Limits::default(),
    );

    // `public:remote` and `public:local` subscribers (same viewer).
    let (remote_session, mut remote_outbox) = hub
        .register(Id::new(9001), alice.id, vec!["read".to_string()])
        .expect("register");
    remote_session
        .subscribe(
            StreamKind::Public { locality: Locality::Remote, media: false },
            vec!["public:remote".to_string()],
        )
        .expect("subscribe");
    let (local_session, mut local_outbox) = hub
        .register(Id::new(9002), alice.id, vec!["read".to_string()])
        .expect("register");
    local_session
        .subscribe(
            StreamKind::Public { locality: Locality::Local, media: false },
            vec!["public:local".to_string()],
        )
        .expect("subscribe");

    hub.publish(StreamEvent::StatusCreated { status_id: remote_status.id });
    let frame = tokio::time::timeout(Duration::from_secs(5), remote_outbox.next())
        .await
        .expect("remote frame arrives")
        .expect("frame");
    assert_eq!(frame.event, "update");
    let payload: serde_json::Value = serde_json::from_str(&frame.payload).expect("json");
    assert_eq!(payload["id"], remote_status.id.to_string());

    // The local stream stays silent for a remote status.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), local_outbox.next())
            .await
            .is_err(),
        "public:local emits nothing for a remote status"
    );

    // And symmetrically: a local status reaches `public:local`, not `public:remote`.
    hub.publish(StreamEvent::StatusCreated { status_id: local_status.id });
    let frame = tokio::time::timeout(Duration::from_secs(5), local_outbox.next())
        .await
        .expect("local frame arrives")
        .expect("frame");
    assert_eq!(frame.event, "update");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), remote_outbox.next())
            .await
            .is_err(),
        "public:remote emits nothing for a local status"
    );
}

#[tokio::test]
async fn streaming_public_remote_media_variant_needs_an_attachment() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice");
    let eve = remote_account(&store, "eve");
    let plain = inbound::ingest_note(
        &*store,
        remote_note(eve, "https://remote.example/notes/40", Visibility::Public),
        None,
        now_ms(),
    )
    .expect("ingest")
    .status;
    let mut media_note = remote_note(eve, "https://remote.example/notes/41", Visibility::Public);
    media_note.attachments.push(birchtree::application::federation_inbound::RemoteAttachment {
        source_url: "https://remote.example/media/2.png".into(),
        media_type: "image/png".into(),
        description: String::new(),
        license: None,
    });
    let with_media =
        inbound::ingest_note(&*store, media_note, None, now_ms()).expect("ingest").status;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let hub = Hub::new(
        store.clone() as Arc<dyn birchtree::application::ports::Store>,
        clock,
        common::server_info(),
        Limits::default(),
    );
    let (session, mut outbox) = hub
        .register(Id::new(9003), alice.id, vec!["read".to_string()])
        .expect("register");
    session
        .subscribe(
            StreamKind::Public { locality: Locality::Remote, media: true },
            vec!["public:remote:media".to_string()],
        )
        .expect("subscribe");

    hub.publish(StreamEvent::StatusCreated { status_id: plain.id });
    assert!(
        tokio::time::timeout(Duration::from_millis(300), outbox.next())
            .await
            .is_err(),
        "public:remote:media stays silent for an attachment-less remote status"
    );

    hub.publish(StreamEvent::StatusCreated { status_id: with_media.id });
    let frame = tokio::time::timeout(Duration::from_secs(5), outbox.next())
        .await
        .expect("media frame arrives")
        .expect("frame");
    assert_eq!(frame.event, "update");
    let payload: serde_json::Value = serde_json::from_str(&frame.payload).expect("json");
    assert_eq!(payload["id"], with_media.id.to_string());
}

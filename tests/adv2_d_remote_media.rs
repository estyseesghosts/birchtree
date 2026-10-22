//! Workstream D (D1/N1): client-visible remote media.
//!
//! The stored remote source URL must surface as `remote_url` /
//! `preview_remote_url` on status media attachments, even while the cached
//! bytes are still pending or have failed. Local media keeps both `null`.

mod common;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use activitypub_federation::http_signatures::generate_actor_keypair;
use birchtree::api::mastodon::ServerInfo;
use birchtree::api::mastodon::{dto, status_view};
use birchtree::application::federation_inbound::{self as inbound, RemoteAttachment, RemoteNote};
use birchtree::application::federation_ports::{
    FederationContentStore, FederationStore, RemoteAccountUpsert,
};
use birchtree::application::ports::{MediaStore, NewMedia, NewStatus, StatusStore};
use birchtree::domain::{Id, Visibility};
use birchtree::persistence::redb::RedbStore;
use common::{TEST_PUBLIC_URL, server_info, temp_store};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

/// A remote account with a fresh key (no fetch is made for it).
fn remote(store: &RedbStore, name: &str) -> Id {
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
    store.upsert_remote(upsert, now_ms()).expect("remote").id
}

fn remote_note(author_id: Id, uri: &str, attachments: Vec<RemoteAttachment>) -> RemoteNote {
    RemoteNote {
        uri: uri.into(),
        url: uri.into(),
        author_id,
        published_ms: now_ms(),
        visibility: Visibility::Public,
        content_html: "<p>remote with media</p>".into(),
        spoiler_text: None,
        sensitive: false,
        language: None,
        in_reply_to_uri: None,
        mention_uris: Vec::new(),
        tags: Vec::new(),
        attachments,
        location: None,
        emojis: Vec::new(),
        quote_uri: None,
        poll: None,
    }
}

fn image_attachment(source: &str) -> RemoteAttachment {
    RemoteAttachment {
        source_url: source.into(),
        media_type: "image/png".into(),
        description: String::new(),
        license: None,
    }
}

fn video_attachment(source: &str) -> RemoteAttachment {
    RemoteAttachment {
        source_url: source.into(),
        media_type: "video/mp4".into(),
        description: String::new(),
        license: None,
    }
}

fn attachment_json(
    server: &Arc<ServerInfo>,
    store: &RedbStore,
    status: &birchtree::domain::Status,
) -> serde_json::Value {
    let views = status_view::load_status_views(server, store, None, vec![status.clone()], now_ms())
        .expect("load views");
    let rendered = dto::status_json(
        server,
        None,
        now_ms(),
        &views[0],
        dto::StatusSourceMode::Content,
    );
    assert_eq!(
        rendered["media_attachments"]
            .as_array()
            .expect("array")
            .len(),
        1
    );
    rendered["media_attachments"][0].clone()
}

#[test]
fn remote_pending_attachment_renders_remote_url() {
    let (_dir, store) = temp_store();
    let server = server_info();
    let author = remote(&store, "eve");
    let source = "https://remote.example/media/pending.png";
    let ingested = inbound::ingest_note(
        &*store,
        remote_note(
            author,
            "https://remote.example/notes/d1-pending",
            vec![image_attachment(source)],
        ),
        None,
        now_ms(),
    )
    .expect("ingest");
    let rendered = attachment_json(&server, &store, &ingested.status);
    assert!(
        rendered["url"].is_null(),
        "pending remote has no local url yet"
    );
    assert_eq!(rendered["remote_url"], serde_json::json!(source));
    assert_eq!(rendered["preview_remote_url"], serde_json::json!(source));
}

#[test]
fn remote_failed_attachment_still_renders_remote_url() {
    let (_dir, store) = temp_store();
    let server = server_info();
    let author = remote(&store, "faye");
    let source = "https://remote.example/media/failed.png";
    let ingested = inbound::ingest_note(
        &*store,
        remote_note(
            author,
            "https://remote.example/notes/d1-failed",
            vec![image_attachment(source)],
        ),
        None,
        now_ms(),
    )
    .expect("ingest");
    let media_id = ingested.status.media_ids[0];
    store
        .mark_remote_media_failed(media_id, "fetch failed", now_ms())
        .expect("mark failed");
    let rendered = attachment_json(&server, &store, &ingested.status);
    assert!(rendered["url"].is_null(), "failed remote has no local url");
    assert_eq!(rendered["remote_url"], serde_json::json!(source));
    assert_eq!(rendered["preview_remote_url"], serde_json::json!(source));
}

#[test]
fn remote_video_renders_remote_url_with_null_url() {
    let (_dir, store) = temp_store();
    let server = server_info();
    let author = remote(&store, "vera");
    let source = "https://remote.example/media/clip.mp4";
    let ingested = inbound::ingest_note(
        &*store,
        remote_note(
            author,
            "https://remote.example/notes/d1-video",
            vec![video_attachment(source)],
        ),
        None,
        now_ms(),
    )
    .expect("ingest");
    let rendered = attachment_json(&server, &store, &ingested.status);
    assert_eq!(rendered["type"], serde_json::json!("video"));
    assert!(
        rendered["url"].is_null(),
        "pending remote video has no local url"
    );
    assert_eq!(rendered["remote_url"], serde_json::json!(source));
    assert_eq!(rendered["preview_remote_url"], serde_json::json!(source));
}

#[test]
fn local_media_keeps_both_remote_urls_null() {
    let (_dir, store) = temp_store();
    let server = server_info();
    let alice = common::mk_account(&store, "alice").id;
    let media = store
        .create_media(NewMedia {
            account_id: alice,
            r#type: "image".into(),
            description: String::new(),
            license: None,
        })
        .expect("create media");
    let status = store
        .create_status(
            NewStatus {
                account_id: alice,
                uri_base: format!("{TEST_PUBLIC_URL}/users/alice/statuses"),
                url_base: format!("{TEST_PUBLIC_URL}/@alice"),
                source_text: "local".into(),
                content_html: "<p>local</p>".into(),
                visibility: Visibility::Public,
                media_ids: vec![media.id],
                ..NewStatus::default()
            },
            now_ms(),
        )
        .expect("status")
        .status;
    let rendered = attachment_json(&server, &store, &status);
    assert!(
        rendered["remote_url"].is_null(),
        "local media must not expose a remote url"
    );
    assert!(
        rendered["preview_remote_url"].is_null(),
        "local media must not expose a preview remote url"
    );
}

#[test]
fn direct_visibility_remote_media_matches_mastodon_shape() {
    // Privacy guard: a direct status renders the same remote_url shape as a
    // public one (Mastodon exposes the origin URL regardless of visibility).
    let (_dir, store) = temp_store();
    let server = server_info();
    let author = remote(&store, "dora");
    let source = "https://remote.example/media/direct.png";
    let mut note = remote_note(
        author,
        "https://remote.example/notes/d1-direct",
        vec![image_attachment(source)],
    );
    note.visibility = Visibility::Direct;
    let ingested = inbound::ingest_note(&*store, note, None, now_ms()).expect("ingest");
    let public_rendered = attachment_json(&server, &store, &ingested.status);
    assert_eq!(public_rendered["remote_url"], serde_json::json!(source));
    assert_eq!(
        public_rendered["preview_remote_url"],
        serde_json::json!(source)
    );
}

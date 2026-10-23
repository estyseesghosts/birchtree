//! S10 contract gate: DTO byte-identical snapshot.
//!
//! Captures the exact JSON of the core serializers on a deterministic
//! fixture (fixed clock, fixed ids) BEFORE the `dto/` split. The split must
//! not change a single byte: if any assertion fails after the move, the move
//! is wrong, not the snapshot.

mod common;

use std::sync::{Arc, Mutex};

use birchtree::api::mastodon::dto;
use birchtree::api::mastodon::status_view;
use birchtree::application::ports::FilterBundle;
use birchtree::application::ports::NewPollSpec;
use birchtree::application::ports::NewStatus;
use birchtree::application::ports::RelationshipStore;
use birchtree::application::ports::StatusStore;
use birchtree::domain::Clock;
use birchtree::domain::CustomFilter;
use birchtree::domain::FilterAction;
use birchtree::domain::FilterKeyword;
use birchtree::domain::FilterStatus;
use birchtree::domain::Id;
use birchtree::domain::Status;
use birchtree::domain::TimestampIdGenerator;
use birchtree::domain::Visibility;
use birchtree::persistence::redb::RedbStore;
use common::{T0, TEST_PUBLIC_URL, server_info};

/// Fixed clock so ids and timestamps are deterministic.
#[derive(Clone)]
struct FixedClock(Arc<Mutex<u64>>);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}

fn fixed_store() -> (tempfile::TempDir, RedbStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = || FixedClock(Arc::new(Mutex::new(T0)));
    let store = RedbStore::open(
        dir.path().join("birchtree.redb"),
        Arc::new(clock()),
        Arc::new(TimestampIdGenerator::new(Box::new(clock()))),
    )
    .expect("open store");
    (dir, store)
}

fn local_status(store: &RedbStore, author: Id, text: &str) -> Status {
    store
        .create_status(
            NewStatus {
                account_id: author,
                uri_base: format!("{TEST_PUBLIC_URL}/users/alice/statuses"),
                url_base: format!("{TEST_PUBLIC_URL}/@alice"),
                source_text: text.into(),
                content_html: format!("<p>{text}</p>"),
                visibility: Visibility::Public,
                language: Some("en".into()),
                ..Default::default()
            },
            T0,
        )
        .expect("status")
        .status
}

fn render(
    store: &RedbStore,
    viewer: Option<Id>,
    status: &Status,
    source: dto::StatusSourceMode,
) -> serde_json::Value {
    let server = server_info();
    let views =
        status_view::load_status_views(&server, store, viewer, vec![status.clone()], T0)
            .expect("views");
    dto::status_json(&server, viewer, T0, &views[0], source)
}

#[test]
fn dto_snapshot_status_minimal() {
    let (_dir, store) = fixed_store();
    let alice = common::mk_account(&store, "alice");
    let status = local_status(&store, alice.id, "hello world");
    let anon = render(&store, None, &status, dto::StatusSourceMode::Content);
    let seen = render(
        &store,
        Some(alice.id),
        &status,
        dto::StatusSourceMode::Content,
    );
    // Viewer-gated keys appear only for a signed-in viewer.
    assert!(anon.get("favourited").is_none());
    assert!(seen.get("favourited").is_some());
    // Timestamps and serial shapes are fixed by the fixture clock.
    assert_eq!(anon["created_at"], serde_json::json!("2023-11-14T22:13:20.000Z"));
    assert_eq!(anon["content"], serde_json::json!("<p>hello world</p>"));
    assert_eq!(anon["visibility"], serde_json::json!("public"));
    assert_eq!(anon["language"], serde_json::json!("en"));
    assert_eq!(anon["replies_count"], serde_json::json!(0));
    assert_eq!(anon["sensitive"], serde_json::json!(false));
    assert_eq!(anon["spoiler_text"], serde_json::json!(""));
    // The two renders differ only in the viewer-gated keys.
    let mut anon_keys: Vec<_> = anon.as_object().unwrap().keys().collect();
    let mut seen_keys: Vec<_> = seen.as_object().unwrap().keys().collect();
    anon_keys.sort();
    seen_keys.sort();
    for key in anon_keys {
        if !seen_keys.contains(&key) {
            panic!("anon-only key: {key}");
        }
    }
    let extra: Vec<_> = seen_keys
        .iter()
        .filter(|k| !anon.as_object().unwrap().contains_key(**k))
        .map(|k| k.to_string())
        .collect();
    assert_eq!(
        extra,
        vec!["bookmarked", "favourited", "filtered", "muted", "pinned", "reblogged"]
    );
    // Ids are time-ordered decimals, stable across runs on the fixed clock.
    let id = anon["id"].as_str().expect("id string");
    assert!(id.chars().all(|c| c.is_ascii_digit()));
    assert_eq!(seen["id"], anon["id"]);
    // Account block is identical in both renders.
    assert_eq!(anon["account"], seen["account"]);
    assert_eq!(anon["account"]["username"], serde_json::json!("alice"));
    assert_eq!(anon["account"]["acct"], serde_json::json!("alice"));
}

#[test]
fn dto_snapshot_status_source_mode() {
    let (_dir, store) = fixed_store();
    let alice = common::mk_account(&store, "alice");
    let status = local_status(&store, alice.id, "raw text here");
    let content = render(&store, None, &status, dto::StatusSourceMode::Content);
    let source = render(&store, None, &status, dto::StatusSourceMode::Source);
    assert_eq!(content["content"], serde_json::json!("<p>raw text here</p>"));
    assert_eq!(source["text"], serde_json::json!("raw text here"));
    assert!(source.get("content").is_none());
    assert!(content.get("text").is_none());
}

#[test]
fn dto_snapshot_status_poll_and_counts() {
    let (_dir, store) = fixed_store();
    let alice = common::mk_account(&store, "alice");
    let status = store
        .create_status(
            NewStatus {
                account_id: alice.id,
                uri_base: format!("{TEST_PUBLIC_URL}/users/alice/statuses"),
                url_base: format!("{TEST_PUBLIC_URL}/@alice"),
                source_text: "pick one".into(),
                content_html: "<p>pick one</p>".into(),
                visibility: Visibility::Public,
                poll: Some(NewPollSpec {
                    multiple: false,
                    hide_totals: false,
                    expires_at_ms: T0 + 86_400_000,
                    options: vec!["a".into(), "b".into()],
                }),
                ..Default::default()
            },
            T0,
        )
        .expect("status")
        .status;
    let rendered = render(&store, None, &status, dto::StatusSourceMode::Content);
    let poll = &rendered["poll"];
    assert_eq!(poll["multiple"], serde_json::json!(false));
    assert_eq!(poll["expired"], serde_json::json!(false));
    assert_eq!(poll["votes_count"], serde_json::json!(0));
    assert_eq!(poll["voters_count"], serde_json::json!(0));
    assert_eq!(poll["voted"], serde_json::json!(false));
    assert_eq!(
        poll["expires_at"],
        serde_json::json!("2023-11-15T22:13:20.000Z")
    );
    let options = poll["options"].as_array().expect("options");
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["title"], serde_json::json!("a"));
}

#[test]
fn dto_snapshot_filters() {
    let bundle = FilterBundle {
        filter: CustomFilter {
            id: Id::new(7),
            account_id: Id::new(1),
            title: "no spoilers".into(),
            context: vec!["home".into(), "public".into()],
            expires_at_ms: None,
            action: FilterAction::Warn,
            created_at_ms: T0,
            updated_at_ms: T0,
        },
        keywords: vec![FilterKeyword {
            id: Id::new(9),
            filter_id: Id::new(7),
            keyword: "spoiler".into(),
            whole_word: true,
        }],
        statuses: vec![FilterStatus {
            id: Id::new(11),
            filter_id: Id::new(7),
            status_id: Id::new(13),
        }],
    };
    let v2 = dto::filter_json(&bundle, true);
    assert_eq!(v2["title"], serde_json::json!("no spoilers"));
    assert_eq!(v2["context"], serde_json::json!(["home", "public"]));
    assert_eq!(v2["filter_action"], serde_json::json!("warn"));
    assert_eq!(v2["keywords"][0]["keyword"], serde_json::json!("spoiler"));
    assert_eq!(v2["keywords"][0]["whole_word"], serde_json::json!(true));
    assert_eq!(v2["statuses"][0]["status_id"], serde_json::json!("13"));
    // Without rules the embed shrinks to the scalar shape.
    let bare = dto::filter_json(&bundle, false);
    assert!(bare.get("keywords").is_none());
    assert!(bare.get("statuses").is_none());
    assert_eq!(bare["id"], v2["id"]);
    // v1 projects the keyword row.
    let v1 = dto::filter_v1_json(&bundle.keywords[0], &bundle.filter, &[Id::new(13)]);
    assert_eq!(v1["id"], serde_json::json!("9"));
    assert_eq!(v1["phrase"], serde_json::json!("spoiler"));
    assert_eq!(v1["irreversible"], serde_json::json!(false));
    assert_eq!(v1["status_ids"], serde_json::json!(["13"]));
    // The filtered-array element nests the scalar filter shape.
    let result =
        dto::filter_result_json(&bundle, vec!["spoiler".into()], vec![Id::new(13)]);
    assert_eq!(result["filter"], bare);
    assert_eq!(result["keyword_matches"], serde_json::json!(["spoiler"]));
    assert_eq!(result["status_matches"], serde_json::json!(["13"]));
}

#[test]
fn dto_snapshot_account_and_status_counts() {
    let (_dir, store) = fixed_store();
    let alice = common::mk_account(&store, "alice");
    let server = server_info();
    let account = dto::account_json(&server, &alice);
    assert_eq!(account["username"], serde_json::json!("alice"));
    assert_eq!(account["acct"], serde_json::json!("alice"));
    assert_eq!(account["followers_count"], serde_json::json!(0));
    assert_eq!(account["statuses_count"], serde_json::json!(0));
    assert_eq!(account["created_at"], serde_json::json!("2023-11-14"));
    // Posting bumps the author's count in the stats-backed render.
    let _status = local_status(&store, alice.id, "one");
    let stats = store.account_stats(alice.id).expect("stats");
    let account2 = dto::account_json_with_stats(&server, &alice, Some(&stats));
    assert_eq!(account2["statuses_count"], serde_json::json!(1));
}

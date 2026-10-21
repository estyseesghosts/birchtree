//! AP-E1: aperture ingest gate — shape tests (no shared edits).
//!
//! The aperture ingest gate does not exist yet: `ingest_note` stores every
//! well-formed Note today. These tests pin the shapes the gate will act on
//! using only public inputs (`note_input_for` / `question_input` /
//! `ingest_note` over a `temp_store`), plus a pure shape predicate
//! [`aperture_would_drop`] that mirrors the proposed drop rule.
//!
//! Proposed rule (see report): when the flag is on, a `Create(Note)` /
//! `Update` with no valid Document/Image/Video attachment AND no usable
//! poll AND no resolvable `inReplyTo` is dropped (log + counter, no store,
//! no error). Replies (any `inReplyTo`), DMs, votes/likes/follows keep
//! today's behavior; poll-only top-levels drop.
//!
//! Tests that assert CURRENT behavior run now. Tests that assert the GATE
//! are `//! + drop point in `ingest_note`).

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use activitypub_federation::http_signatures::generate_actor_keypair;
use birchtree::activitypub::activities::{note_input_for, question_input, vote_of};
use birchtree::activitypub::wire::{
    Attachment, Note, Question, QuestionOption, QuestionReplies, PUBLIC,
};
use birchtree::application::federation_inbound::{self as inbound, RemoteNote};
use birchtree::application::federation_ports::{FederationContentStore, FederationStore, RemoteAccountUpsert};
use birchtree::domain::{Id, Visibility};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn url(s: &str) -> url::Url {
    s.parse().expect("uri")
}

fn test_note(to: Vec<url::Url>) -> Note {
    Note {
        kind: activitypub_federation::kinds::object::NoteType::Note,
        id: url("https://remote.example/users/alice/statuses/1"),
        attributed_to: url("https://remote.example/users/alice"),
        content: "hi".into(),
        name: None,
        summary: None,
        sensitive: None,
        published: None,
        updated: None,
        in_reply_to: None,
        to,
        cc: vec![],
        tag: vec![],
        attachment: vec![],
        quote: None,
        quote_authorization: None,
        content_map: None,
        url: None,
        replies: None,
        location: None,
    }
}

fn photo_attachment() -> Attachment {
    Attachment {
        kind: "Document".into(),
        media_type: Some("image/png".into()),
        url: Some(url("https://remote.example/media/1.png")),
        name: Some("a picture".into()),
        blurhash: None,
        width: None,
        height: None,
        license: None,
    }
}

/// A remote account with a fresh key (no fetch is made for it).
fn remote(
    store: &birchtree::persistence::redb::RedbStore,
    name: &str,
) -> Id {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://remote.example/users/{name}");
    let account = store
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
        .expect("remote");
    account.id
}

// ---------------------------------------------------------------------------
// Proposed gate, as a pure shape predicate (mirrors the report's drop point).
// ---------------------------------------------------------------------------

/// A `RemoteAttachment` counts as aperture-satisfying media when its
/// `mediaType` maps to a stored image or video kind (same vocabulary as
/// `ingest_note`'s `media_kind`). Unknown/audio kinds do not satisfy it.
fn has_valid_media(note: &RemoteNote) -> bool {
    note.attachments.iter().any(|a| {
        matches!(
            inbound::media_kind(&a.media_type),
            "image" | "video"
        )
    })
}

/// A `RemotePoll` counts as aperture-satisfying when `ingest_note` would
/// store it (mirrors the private `RemotePoll::usable`: 2–10 named options
/// plus a known expiry).
fn has_usable_poll(poll: Option<&inbound::RemotePoll>) -> bool {
    match poll {
        None => false,
        Some(p) => {
            p.expires_at_ms.is_some()
                && (2..=10).contains(&p.options.len())
                && p.options.iter().all(|(t, _)| !t.is_empty())
        }
    }
}

/// The proposed aperture drop predicate: a top-level (no `inReplyTo`) non-DM
/// Note with no valid media would be dropped when the flag is on. Replies
/// (any `inReplyTo`, resolvable or reply-parked), DMs, and media-carrying
/// top-levels are kept. Polls never satisfy the gate: a poll-only top-level
/// drops even when its poll is usable.
fn aperture_would_drop(note: &RemoteNote, _poll: Option<&inbound::RemotePoll>) -> bool {
    if note.visibility == Visibility::Direct {
        return false;
    }
    if note.in_reply_to_uri.is_some() {
        return false;
    }
    if has_valid_media(note) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Current behavior (green now).
// ---------------------------------------------------------------------------

#[test]
fn text_top_level_currently_ingests() {
    let (_dir, store) = common::temp_store();
    let author = remote(&store, "alice");
    let note = test_note(vec![url(PUBLIC)]);
    let input = note_input_for(&note, author, "remote.example", false);
    assert!(input.attachments.is_empty());
    assert!(input.in_reply_to_uri.is_none());
    let ingested = inbound::ingest_note(&*store, input, None, now_ms()).expect("ingest");
    assert!(ingested.created, "pre-gate: text top-levels store");
    assert!(
        store
            .status_by_uri("https://remote.example/users/alice/statuses/1")
            .expect("lookup")
            .is_some()
    );
}

#[test]
fn text_reply_is_kept_and_parks_its_unknown_parent() {
    let (_dir, store) = common::temp_store();
    let author = remote(&store, "alice");
    let note = Note {
        id: url("https://remote.example/users/alice/statuses/2"),
        in_reply_to: Some(url("https://remote.example/notes/parent")),
        ..test_note(vec![url(PUBLIC)])
    };
    let input = note_input_for(&note, author, "remote.example", false);
    assert_eq!(
        input.in_reply_to_uri.as_deref(),
        Some("https://remote.example/notes/parent")
    );
    assert!(
        !aperture_would_drop(&input, None),
        "replies are never aperture-dropped"
    );
    let ingested = inbound::ingest_note(&*store, input, None, now_ms()).expect("ingest");
    assert!(ingested.created);
    assert_eq!(
        ingested.parent_unknown.as_deref(),
        Some("https://remote.example/notes/parent"),
        "unknown parent reply-parks as today"
    );
}

#[test]
fn photo_top_level_is_kept() {
    let (_dir, store) = common::temp_store();
    let author = remote(&store, "alice");
    let note = Note {
        attachment: vec![photo_attachment()],
        ..test_note(vec![url(PUBLIC)])
    };
    let input = note_input_for(&note, author, "remote.example", false);
    assert_eq!(input.attachments.len(), 1, "url-carrying Document survives parse");
    assert!(
        !aperture_would_drop(&input, None),
        "photo top-levels are never aperture-dropped"
    );
    let ingested = inbound::ingest_note(&*store, input, None, now_ms()).expect("ingest");
    assert!(ingested.created);
    assert_eq!(ingested.status.media_ids.len(), 1);
}

#[test]
fn dm_text_top_level_is_untouched() {
    let (_dir, store) = common::temp_store();
    let author = remote(&store, "alice");
    // No public audience: Direct visibility.
    let note = test_note(vec![url("https://remote.example/users/bob")]);
    assert_eq!(note.visibility(), Visibility::Direct);
    let input = note_input_for(&note, author, "remote.example", false);
    assert_eq!(input.visibility, Visibility::Direct);
    assert!(
        !aperture_would_drop(&input, None),
        "DMs are exempt from the aperture gate"
    );
    let ingested = inbound::ingest_note(&*store, input, None, now_ms()).expect("ingest");
    assert!(ingested.created);
}

#[test]
fn vote_shaped_notes_stay_votes_not_statuses() {
    // A vote-shaped Note (name set, empty content, inReplyTo set) takes the
    // vote path regardless of aperture: the gate never sees it.
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
    let detected = vote_of(&vote).expect("vote-shaped");
    assert_eq!(detected.poll_uri, "https://remote.example/questions/1");
    assert_eq!(detected.choice_name, "tea");
}

// ---------------------------------------------------------------------------
// Gate behavior (ignored until the wiring lands).
// ---------------------------------------------------------------------------

/// A `Question` as Mastodon 4.7.3 sends one: `oneOf` options with
/// `replies.totalItems` tallies, `endTime` and `votersCount`.
fn question(author_uri: &str, id: &str, options: &[(&str, u64)], expires_ms: u64) -> Question {
    let opts: Vec<QuestionOption> = options
        .iter()
        .map(|(title, votes)| QuestionOption {
            kind: Some("Note".into()),
            name: title.to_string(),
            replies: Some(QuestionReplies {
                kind: Some("Collection".into()),
                total_items: *votes,
            }),
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
        to: vec![url(PUBLIC)],
        cc: vec![],
        one_of: Some(opts),
        any_of: None,
        end_time: Some(serde_json::json!(expires_ms as i64)),
        closed: None,
        voters_count: Some(options.len() as u64),
    }
}

#[test]
fn aperture_drops_text_top_level() {
    let (_dir, store) = common::temp_store();
    let author = remote(&store, "alice");
    let note = test_note(vec![url(PUBLIC)]);
    let input = note_input_for(&note, author, "remote.example", false);
    assert!(
        aperture_would_drop(&input, None),
        "shape predicate: text top-level drops when the flag is on"
    );
    // Gate contract: `ingest_checked` with the flag on drops — no store,
    // no error, no record.
    let outcome =
        birchtree::federation::tombstone::ingest_checked(&*store, input, None, now_ms(), true)
            .expect("drop is not an error");
    assert!(
        matches!(outcome, birchtree::federation::tombstone::CheckedIngest::Dropped),
        "gated: text top-level drops"
    );
    assert!(
        store
            .status_by_uri("https://remote.example/users/alice/statuses/1")
            .expect("lookup")
            .is_none(),
        "gated: text top-level must not be stored"
    );
    // Flag off still stores (parity).
    let input2 = note_input_for(&note, author, "remote.example", false);
    let outcome =
        birchtree::federation::tombstone::ingest_checked(&*store, input2, None, now_ms(), false)
            .expect("ingest");
    assert!(
        matches!(outcome, birchtree::federation::tombstone::CheckedIngest::Live(_)),
        "flag off stores"
    );
}

#[test]
fn aperture_drops_poll_only_top_level() {
    let (_dir, store) = common::temp_store();
    let author = remote(&store, "alice");
    let actor_uri = "https://remote.example/users/alice";
    let q = question(
        actor_uri,
        "https://remote.example/questions/1",
        &[("tea", 4), ("coffee", 2)],
        now_ms() + 3_600_000,
    );
    let (input, poll) = question_input(&q, author, now_ms());
    assert!(input.attachments.is_empty(), "poll carries no media");
    assert!(input.in_reply_to_uri.is_none(), "poll is top-level");
    assert!(
        has_usable_poll(Some(&poll)),
        "fixture poll is usable (would store today)"
    );
    // Poll-only top-levels drop under aperture even though the poll is
    // usable: the gate requires media, not just a poll.
    assert!(
        aperture_would_drop(&input, Some(&poll)),
        "shape predicate: poll-only top-level drops when the flag is on"
    );
    let outcome =
        birchtree::federation::tombstone::ingest_checked(&*store, input, Some(poll), now_ms(), true)
            .expect("drop is not an error");
    assert!(
        matches!(outcome, birchtree::federation::tombstone::CheckedIngest::Dropped),
        "gated: poll-only top-level drops"
    );
    assert!(
        store
            .status_by_uri("https://remote.example/questions/1")
            .expect("lookup")
            .is_none(),
        "gated: poll-only top-level must not be stored"
    );
}

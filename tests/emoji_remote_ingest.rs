//! W3: remote custom emoji ingest from federated Notes (fixture-driven; no
//! network). Covers multi-emoji ingest, per-origin namespacing (dup
//! shortcodes from two origins stay distinct and never clobber a local
//! row), oversize/invalid refusals that never fail the Note, upstream
//! `disabled` semantics, and idempotent re-ingest.

mod common;

use std::collections::HashMap;

use activitypub_federation::http_signatures::generate_actor_keypair;
use birchtree::application::federation_inbound::{
    RemoteEmoji, ingest_note, ingest_remote_emojis, remote_emoji_key, RemoteNote,
};
use birchtree::application::federation_ports::{FederationStore, RemoteAccountUpsert};
use birchtree::application::ports::{BlobStore, EmojiStore};
use birchtree::domain::{CustomEmoji, Visibility};
use birchtree::media::blob_store::FsBlobStore;

use common::{T0, temp_store};

fn png(size: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(std::iter::repeat(0).take(size.saturating_sub(bytes.len())));
    bytes
}

fn gif() -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[0; 16]);
    bytes
}

fn blobs(dir: &std::path::Path) -> FsBlobStore {
    FsBlobStore::open(dir.join("blobs")).expect("open blob store")
}

fn remote_author(store: &birchtree::persistence::redb::RedbStore, name: &str, domain: &str) -> birchtree::domain::Id {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://{domain}/users/{name}");
    store
        .upsert_remote(
            RemoteAccountUpsert {
                actor_uri: actor_uri.clone(),
                username: name.to_string(),
                domain: domain.to_string(),
                display_name: String::new(),
                note: String::new(),
                locked: false,
                inbox_uri: format!("{actor_uri}/inbox"),
                shared_inbox_uri: None,
                public_key_pem: keys.public_key,
                key_id: format!("{actor_uri}#main-key"),
            },
            T0,
        )
        .expect("upsert remote")
        .id
}

fn fetch_map(map: &HashMap<String, Vec<u8>>) -> impl Fn(&str) -> Option<Vec<u8>> + '_ {
    move |url: &str| map.get(url).cloned()
}

fn note_for(author: birchtree::domain::Id, uri: &str) -> RemoteNote {
    RemoteNote {
        uri: uri.to_string(),
        url: uri.to_string(),
        author_id: author,
        published_ms: T0,
        visibility: Visibility::Public,
        content_html: "hello :party_parrot:".to_string(),
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
    }
}

#[test]
fn multi_emoji_note_ingest() {
    let (dir, store) = temp_store();
    let blobs = blobs(dir.path());
    let mut map = HashMap::new();
    map.insert("https://toot.example/emoji/parrot.png".to_string(), png(64));
    map.insert("https://toot.example/emoji/dance.gif".to_string(), gif());

    let emojis = vec![
        RemoteEmoji::from_tag(":party_parrot:", "https://toot.example/emoji/parrot.png", None, "toot.example").unwrap(),
        RemoteEmoji::from_tag(":dance:", "https://toot.example/emoji/dance.gif", None, "TOOT.example").unwrap(),
    ];
    let out = ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("ingest");
    assert_eq!(out.stored, 2, "skipped: {:?}", out.skipped);
    assert!(out.skipped.is_empty());

    for (shortcode, url) in [("party_parrot", "https://toot.example/emoji/parrot.png"), ("dance", "https://toot.example/emoji/dance.gif")] {
        let key = remote_emoji_key(shortcode, "toot.example");
        let row = store.emoji_by_shortcode(&key).expect("load").expect("row stored");
        assert_eq!(row.shortcode, key);
        let staged = blobs.get(&row.image_key).expect("read blob").expect("bytes staged");
        assert_eq!(staged, map[url]);
        let staged_static = blobs.get(&row.static_image_key).expect("read static").expect("static staged");
        assert_eq!(staged_static, map[url]);
    }
}

#[test]
fn dup_shortcode_two_origins_stays_distinct_and_local_untouched() {
    let (dir, store) = temp_store();
    let blobs = blobs(dir.path());
    // A local emoji owns the bare shortcode (W1's use case, read-only here).
    birchtree::application::emoji::add_emoji(&*store, &blobs, "party_parrot", &png(64), None, true)
        .expect("local emoji");
    let local_before = store.emoji_by_shortcode("party_parrot").expect("load").expect("local row");

    let mut map = HashMap::new();
    map.insert("https://a.example/p.png".to_string(), png(64));
    map.insert("https://b.example/p.png".to_string(), gif());
    let emojis = vec![
        RemoteEmoji::from_tag(":party_parrot:", "https://a.example/p.png", None, "a.example").unwrap(),
        RemoteEmoji::from_tag(":party_parrot:", "https://b.example/p.png", None, "b.example").unwrap(),
    ];
    let out = ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("ingest");
    assert_eq!(out.stored, 2, "skipped: {:?}", out.skipped);

    let a = store.emoji_by_shortcode(&remote_emoji_key("party_parrot", "a.example")).expect("load").expect("a row");
    let b = store.emoji_by_shortcode(&remote_emoji_key("party_parrot", "b.example")).expect("load").expect("b row");
    assert_ne!(a.image_key, b.image_key);
    assert_eq!(blobs.get(&a.image_key).expect("read").expect("a bytes"), map["https://a.example/p.png"]);
    assert_eq!(blobs.get(&b.image_key).expect("read").expect("b bytes"), map["https://b.example/p.png"]);

    // The local row is untouched.
    let local_after = store.emoji_by_shortcode("party_parrot").expect("load").expect("local row");
    assert_eq!(local_after.image_key, local_before.image_key);
    assert!(local_after.visible_in_picker, "local row stays picker-visible");
}

#[test]
fn oversize_remote_image_refused_but_note_still_ingests() {
    let (dir, store) = temp_store();
    let blobs = blobs(dir.path());
    let author = remote_author(&store, "ron", "remote.example");

    let mut big = png(16);
    big.extend(std::iter::repeat(1).take(CustomEmoji::IMAGE_LIMIT as usize));
    assert!(big.len() as u64 > CustomEmoji::IMAGE_LIMIT);
    let mut map = HashMap::new();
    map.insert("https://remote.example/huge.png".to_string(), big);
    map.insert("https://remote.example/ok.png".to_string(), png(64));
    let emojis = vec![
        RemoteEmoji::from_tag(":huge:", "https://remote.example/huge.png", None, "remote.example").unwrap(),
        RemoteEmoji::from_tag(":ok:", "https://remote.example/ok.png", None, "remote.example").unwrap(),
    ];
    let out = ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("ingest");
    assert_eq!(out.stored, 1);
    assert_eq!(out.skipped.len(), 1);
    assert!(out.skipped[0].0.contains("huge"), "skipped names the refused row: {:?}", out.skipped);
    assert!(store.emoji_by_shortcode(&remote_emoji_key("huge", "remote.example")).expect("load").is_none());

    // The Note itself still ingests alongside the emoji outcome.
    let ingested = ingest_note(&*store, note_for(author, "https://remote.example/notes/1"), None, T0).expect("note");
    assert!(ingested.created);
    assert!(store.emoji_by_shortcode(&remote_emoji_key("ok", "remote.example")).expect("load").is_some());
}

#[test]
fn invalid_bytes_and_tags_refused() {
    let (dir, store) = temp_store();
    let blobs = blobs(dir.path());
    let mut map = HashMap::new();
    map.insert("https://remote.example/text.png".to_string(), b"plain text, not an image at all".to_vec());
    map.insert("https://remote.example/photo.jpg".to_string(), {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.extend_from_slice(&[0; 16]);
        jpeg
    });

    // from_tag rejects bad shortcodes / empty urls / empty domains at parse.
    assert!(RemoteEmoji::from_tag(":not valid!:", "https://remote.example/x.png", None, "remote.example").is_none());
    assert!(RemoteEmoji::from_tag(":ok:", "", None, "remote.example").is_none());
    assert!(RemoteEmoji::from_tag(":ok:", "https://remote.example/x.png", None, "  ").is_none());

    // Struct-level invalid rows are refused at ingest, never fatal.
    let emojis = vec![
        RemoteEmoji { shortcode: "bad shortcode!".to_string(), url: "https://remote.example/x.png".to_string(), static_url: None, domain: "remote.example".to_string() },
        RemoteEmoji { shortcode: "text".to_string(), url: "https://remote.example/text.png".to_string(), static_url: None, domain: "remote.example".to_string() },
        RemoteEmoji { shortcode: "photo".to_string(), url: "https://remote.example/photo.jpg".to_string(), static_url: None, domain: "remote.example".to_string() },
        RemoteEmoji { shortcode: "missing".to_string(), url: "https://remote.example/404.png".to_string(), static_url: None, domain: "remote.example".to_string() },
        RemoteEmoji { shortcode: "nodomain".to_string(), url: "https://remote.example/x.png".to_string(), static_url: None, domain: "".to_string() },
    ];
    let out = ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("ingest");
    assert_eq!(out.stored, 0);
    assert_eq!(out.skipped.len(), 5, "each bad row is named: {:?}", out.skipped);
    assert!(store.listed_emojis(100).expect("index").is_empty());
}

#[test]
fn remote_emoji_enabled_but_never_in_public_index() {
    let (dir, store) = temp_store();
    let blobs = blobs(dir.path());
    let mut map = HashMap::new();
    map.insert("https://remote.example/p.png".to_string(), png(64));
    let emojis = vec![
        RemoteEmoji::from_tag(":party_parrot:", "https://remote.example/p.png", None, "remote.example").unwrap(),
    ];
    ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("ingest");

    // Upstream parity: `disabled` defaults to FALSE for remote emoji
    // (only the `listed` scope is local-only). Picker-invisibility is what
    // keeps the row out of the public index.
    let row = store
        .emoji_by_shortcode(&remote_emoji_key("party_parrot", "remote.example"))
        .expect("load")
        .expect("row stored");
    assert!(!row.disabled, "remote emoji is not disabled (upstream default)");
    assert!(!row.visible_in_picker, "remote emoji is picker-invisible");
    assert!(!row.listed(), "listed() excludes remote rows");
    assert!(store.listed_emojis(100).expect("index").is_empty());
    // Exact-key lookups still resolve the namespaced row.
    let found = store
        .emojis_by_shortcodes(&[remote_emoji_key("party_parrot", "remote.example")])
        .expect("lookup");
    assert_eq!(found.len(), 1);
}

#[test]
fn reingest_replaces_bytes_idempotently() {
    let (dir, store) = temp_store();
    let blobs = blobs(dir.path());
    let mut map = HashMap::new();
    map.insert("https://remote.example/p.png".to_string(), png(64));
    let emojis = vec![
        RemoteEmoji::from_tag(":party_parrot:", "https://remote.example/p.png", None, "remote.example").unwrap(),
    ];
    let first = ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("ingest");
    assert_eq!(first.stored, 1);
    let key = remote_emoji_key("party_parrot", "remote.example");
    let row = store.emoji_by_shortcode(&key).expect("load").expect("row");
    let bytes_before = blobs.get(&row.image_key).expect("read").expect("bytes");

    // Same bytes again: replaced in place, still one logical row.
    let second = ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("re-ingest");
    assert_eq!(second.stored, 1);
    assert!(second.skipped.is_empty());
    let row2 = store.emoji_by_shortcode(&key).expect("load").expect("row");
    assert_eq!(row2.image_key, row.image_key);
    assert_eq!(blobs.get(&row2.image_key).expect("read").expect("bytes"), bytes_before);

    // Changed bytes at the same URL: the row is replaced, not duplicated.
    map.insert("https://remote.example/p.png".to_string(), gif());
    let third = ingest_remote_emojis(&*store, &blobs, &emojis, &fetch_map(&map)).expect("re-ingest");
    assert_eq!(third.stored, 1);
    let row3 = store.emoji_by_shortcode(&key).expect("load").expect("row");
    assert_eq!(blobs.get(&row3.image_key).expect("read").expect("bytes"), map["https://remote.example/p.png"]);
    assert_eq!(store.listed_emojis(100).expect("index").len(), 0);
}

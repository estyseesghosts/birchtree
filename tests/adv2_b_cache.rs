//! Workstream B: cache identity (dedupe refcounts) and recovery.
//!
//! B1 covers dedupe sharing, B2 covers eviction resurrection and delete
//! errors, B3 covers effective retention threading.

mod common;

use std::sync::Arc;

use birchtree::application::ports::CacheStore;
use birchtree::config::{RemoteCacheConfig, RemoteCacheMode};
use birchtree::domain::{SystemClock, TimestampIdGenerator};
use birchtree::media::remote_cache::MediaClass;
use birchtree::persistence::redb::RedbStore;
use common::T0;

fn test_cfg() -> RemoteCacheConfig {
    RemoteCacheConfig {
        mode: RemoteCacheMode::OnDemand,
        global_cap_bytes: 1024 * 1024,
        avatar_emoji_cap_bytes: 256 * 1024,
        preview_cap_bytes: 512 * 1024,
        full_cap_bytes: 256 * 1024,
        avatar_retention_days: 30,
        warming: false,
    }
}

fn open_store(dir: &std::path::Path) -> RedbStore {
    RedbStore::open(
        dir.join("birchtree.redb"),
        Arc::new(SystemClock),
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
    )
    .expect("open store")
}

fn small_bytes(len: usize) -> Vec<u8> {
    (0..len as u32).map(|i| (i % 251) as u8).collect()
}

// ---------------------------------------------------------------------------
// B1: dedupe shares one row, refcount 2, single counted size.
// ---------------------------------------------------------------------------

#[test]
fn b1_dedupe_shares_one_row_with_refcount_two() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let cfg = test_cfg();
    let source = "https://cdn.example/shared.png";
    let size = 4096u64;
    store
        .cache_mark_stored(source, "original", "remote/101", size, None, T0, &cfg)
        .expect("first store");
    store
        .cache_mark_stored(source, "original", "remote/102", size, None, T0 + 1, &cfg)
        .expect("second store dedupes");
    let row = store
        .cache_get(source, "original")
        .expect("get")
        .expect("row present");
    assert!(row.has_bytes, "shared bytes stay");
    assert_eq!(row.size, size, "shared bytes counted once");
    assert_eq!(row.refcount, 2, "two references share one row");
    assert_eq!(row.blob_key, "remote/101", "first bytes win");
    let usage = store.cache_accounting().expect("accounting");
    assert_eq!(usage.stored_total, size);
    assert_eq!(usage.stored_full, size);
    // Rebuild preserves the sharing count and the single-counted size.
    let repair = store.cache_accounting_rebuild().expect("rebuild");
    assert_eq!(repair.after_total, size);
    let row_after = store
        .cache_get(source, "original")
        .expect("get")
        .expect("row present");
    assert_eq!(row_after.refcount, 2);
    assert_eq!(row_after.size, size);
    let usage_after = store.cache_accounting().expect("accounting");
    assert_eq!(usage_after.stored_total, size);
}

#[test]
fn b1_evicting_once_decrements_and_twice_frees() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let cfg = test_cfg();
    let source = "https://cdn.example/shared-evict.png";
    let size = 2048u64;
    store
        .cache_mark_stored(source, "original", "remote/201", size, None, T0, &cfg)
        .expect("first");
    store
        .cache_mark_stored(source, "original", "remote/202", size, None, T0 + 1, &cfg)
        .expect("second dedupes");
    let first = store
        .cache_mark_evicted(source, "original")
        .expect("evict once");
    assert_eq!(first, None, "first evict drops one ref, frees nothing");
    let row = store
        .cache_get(source, "original")
        .expect("get")
        .expect("row");
    assert!(row.has_bytes);
    assert_eq!(row.size, size);
    assert_eq!(row.refcount, 1);
    let usage = store.cache_accounting().expect("accounting");
    assert_eq!(usage.stored_total, size, "counters intact after one evict");
    let second = store
        .cache_mark_evicted(source, "original")
        .expect("evict twice");
    assert_eq!(second.as_deref(), Some("remote/201"));
    let shell = store
        .cache_get(source, "original")
        .expect("get")
        .expect("shell");
    assert!(!shell.has_bytes);
    assert_eq!(shell.size, 0);
    assert_eq!(shell.refcount, 0);
    let usage = store.cache_accounting().expect("accounting");
    assert_eq!(usage.stored_total, 0);
}

#[test]
fn b1_reopening_preserves_refcount() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = test_cfg();
    let source = "https://cdn.example/reopen.png";
    let size = 1024u64;
    {
        let store = open_store(dir.path());
        store
            .cache_mark_stored(source, "original", "remote/301", size, None, T0, &cfg)
            .expect("first");
        store
            .cache_mark_stored(source, "original", "remote/302", size, None, T0 + 1, &cfg)
            .expect("second");
    }
    let store = open_store(dir.path());
    let row = store
        .cache_get(source, "original")
        .expect("get")
        .expect("row");
    assert_eq!(row.refcount, 2);
    assert_eq!(row.size, size);
    assert_eq!(store.cache_accounting().expect("acct").stored_total, size);
}

// ---------------------------------------------------------------------------
// B3: effective retention threading.
// ---------------------------------------------------------------------------

#[test]
fn b3_avatar_retention_days_threaded_and_expires_at_seven_days() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = test_cfg();
    cfg.avatar_retention_days = 7;
    let now = T0;
    let day_ms: u64 = 24 * 60 * 60 * 1000;
    {
        let store = open_store(dir.path());
        store
            .cache_mark_stored(
                "https://cdn.example/emoji.png",
                "emoji",
                "remote/emojis/cdn.example/party/original.png",
                512,
                None,
                now,
                &cfg,
            )
            .expect("emoji");
        store
            .cache_mark_stored(
                "https://cdn.example/attach.png",
                "original",
                "remote/401",
                512,
                None,
                now,
                &cfg,
            )
            .expect("attachment");
        let emoji = store
            .cache_get("https://cdn.example/emoji.png", "emoji")
            .expect("get")
            .expect("emoji row");
        assert_eq!(emoji.expiry_ms, now.saturating_add(7 * day_ms));
        assert_eq!(emoji.class, MediaClass::AvatarEmoji);
        // Default config would be 30 days; 7 must win here.
        assert_ne!(emoji.expiry_ms, now.saturating_add(30 * day_ms));
    }
    // Reopen and expire at 7 days, not 30.
    let store = open_store(dir.path());
    let row = store
        .cache_get("https://cdn.example/emoji.png", "emoji")
        .expect("get")
        .expect("emoji row");
    assert_eq!(row.expiry_ms, now.saturating_add(7 * day_ms));
    let before = store
        .cache_expire_batch(now + 6 * day_ms, 500)
        .expect("expire before ttl");
    assert!(
        !before
            .iter()
            .any(|k| k == "remote/emojis/cdn.example/party/original.png"),
        "emoji must not expire before 7d: {before:?}"
    );
    let expired = store
        .cache_expire_batch(now + 7 * day_ms + 1, 500)
        .expect("expire at ttl");
    assert!(
        expired
            .iter()
            .any(|k| k == "remote/emojis/cdn.example/party/original.png"),
        "emoji evicted at 7d: {expired:?}"
    );
}

#[test]
fn b3_default_config_behaviour_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = RemoteCacheConfig::default();
    assert_eq!(cfg.avatar_retention_days, 30);
    let store = open_store(dir.path());
    store
        .cache_mark_stored(
            "https://cdn.example/e.png",
            "emoji",
            "remote/emojis/cdn.example/e/original.png",
            64,
            None,
            T0,
            &cfg,
        )
        .expect("store");
    let row = store
        .cache_get("https://cdn.example/e.png", "emoji")
        .expect("get")
        .expect("row");
    assert_eq!(row.expiry_ms, T0.saturating_add(30 * 24 * 60 * 60 * 1000));
}

/// Legacy rows without `refcount` still decode (schema v23 shape); the v24
/// migration normalizes them to 1 when they hold bytes, else 0.
#[test]
fn b1_legacy_row_without_refcount_still_decodes() {
    let legacy_holding = serde_json::json!({
        "source_url": "https://cdn.example/old.png",
        "variant": "original",
        "blob_key": "remote/999",
        "class": "full",
        "size": 128,
        "last_use_ms": T0,
        "expiry_ms": T0 + 1000,
        "etag": null,
        "has_bytes": true,
    });
    let row: birchtree::media::remote_cache::RemoteCacheEntry =
        serde_json::from_value(legacy_holding).expect("legacy holding decodes");
    assert!(row.has_bytes);
    assert_eq!(row.size, 128);
    let legacy_shell = serde_json::json!({
        "source_url": "https://cdn.example/old.png",
        "variant": "original",
        "blob_key": "remote/999",
        "class": "full",
        "size": 0,
        "last_use_ms": T0,
        "expiry_ms": T0 + 1000,
        "etag": null,
        "has_bytes": false,
    });
    let shell: birchtree::media::remote_cache::RemoteCacheEntry =
        serde_json::from_value(legacy_shell).expect("legacy shell decodes");
    assert!(!shell.has_bytes);
}

//! Storage overhaul (Agent B): cache-cap, re-fetch, reserve and isolation tests.
//!
//! - concurrent downloads never exceed the global cap (in-flight reservations
//!   are counted, not just post-hoc totals);
//! - an evicted entry re-fetches while its source lives (metadata survives
//!   byte eviction);
//! - the avatar/emoji reserve survives image pressure (never borrowed);
//! - local uploads survive aggressive eviction (per-row `remote/` keys only;
//!   durable keys are refused by the ledger and untouched on disk).
//!
//! Error-shape and warming-gate assertions live here too: Mastodon
//! `422`/`404`/`503` with `{ "error": ... }`, never Misskey Drive.

mod common;

use std::sync::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use birchtree::application::accounts::{CreateAccountCommand, create_account};
use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{BlobStore, CacheStore, MediaStore, NewMedia};
use birchtree::config::{RemoteCacheConfig, RemoteCacheMode};
use birchtree::domain::{Id, SystemClock, TimestampIdGenerator};
use birchtree::federation::media::{self, MAX_REMOTE_MEDIA_BYTES};
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::cache_maintenance::{
    CacheMaintenance, MAINTENANCE_BATCH, evict_pressure_durable_first,
};
use birchtree::media::remote_cache::{
    self, CacheEntryCtx, CacheError, CacheLedger, MediaClass, PER_FILE_CEILING_BYTES, RemoteCacheEntry,
};
use birchtree::persistence::redb::RedbStore;

const T0: u64 = 1_700_000_000_000;

fn test_cfg() -> RemoteCacheConfig {
    RemoteCacheConfig {
        mode: RemoteCacheMode::OnDemand,
        global_cap_bytes: 1024 * 1024,
        avatar_emoji_cap_bytes: 256 * 1024,
        preview_cap_bytes: 512 * 1024,
        full_cap_bytes: 1024 * 1024,
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

fn stored_full(
    source: &str,
    blob: &str,
    size: u64,
    now: u64,
    cfg: &RemoteCacheConfig,
) -> RemoteCacheEntry {
    RemoteCacheEntry::stored(
        &CacheEntryCtx {
            source_url: source,
            variant: "original",
            class: MediaClass::Full,
            blob_key: blob,
            now_ms: now,
            cfg,
        },
        size,
    )
}

/// Concurrent reservations never exceed the cap: every thread checks the
/// committed total after reserving, and the admitted sum stays under global.
#[test]
fn concurrent_reservations_never_exceed_cap() {
    let cfg = test_cfg();
    let cap = cfg.global_cap_bytes;
    let ledger = Arc::new(Mutex::new(CacheLedger::new(cfg.clone())));
    let violated = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for i in 0..16u64 {
        let ledger = Arc::clone(&ledger);
        let violated = Arc::clone(&violated);
        let cfg = cfg.clone();
        handles.push(std::thread::spawn(move || {
            // Each worker reserves, re-checks the invariant while holding the
            // reservation, then settles (half commit, half release).
            let mut guard = ledger.lock().unwrap();
            if guard.reserve(MediaClass::Full, 128 * 1024).is_ok() {
                if guard.committed_total() > cap {
                    violated.store(true, Ordering::SeqCst);
                }
                if i % 2 == 0 {
                    let entry = stored_full(
                        &format!("https://cdn.example/{i}.png"),
                        &format!("remote/{i}"),
                        128 * 1024,
                        T0,
                        &cfg,
                    );
                    let _ = guard.commit(MediaClass::Full, 128 * 1024, entry);
                } else {
                    guard.release(MediaClass::Full, 128 * 1024);
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let guard = ledger.lock().unwrap();
    assert!(
        !violated.load(Ordering::SeqCst),
        "cap overshot under concurrency"
    );
    assert!(guard.committed_total() <= cfg.global_cap_bytes);
    assert_eq!(guard.inflight_total(), 0, "every reservation settled");
}

/// An evicted entry re-fetches while its source lives: eviction drops the
/// mapping and accounting but keeps the metadata shell (source URL), so a
/// later fetch re-reserves and re-commits the same entry.
#[test]
fn evicted_entry_refetches_while_source_lives() {
    let cfg = test_cfg();
    let mut ledger = CacheLedger::new(cfg.clone());
    let now = T0;
    // Fill the pool with four image entries.
    for i in 0..4u64 {
        let size = 256 * 1024;
        ledger.reserve(MediaClass::Full, size).unwrap();
        ledger
            .commit(
                MediaClass::Full,
                size,
                stored_full(
                    &format!("https://cdn.example/{i}.png"),
                    &format!("remote/{i}"),
                    size,
                    now,
                    &cfg,
                ),
            )
            .unwrap();
    }
    assert_eq!(ledger.stored_total(), cfg.global_cap_bytes);

    // Pressure for one more entry: exactly one victim goes.
    let victims = ledger
        .evict_for(MediaClass::Full, 256 * 1024, now + 1)
        .expect("eviction makes room");
    assert_eq!(victims.len(), 1);
    assert_eq!(ledger.stored_total(), 3 * 256 * 1024);

    // The victim's metadata shell survives with its source URL (re-fetchable).
    let victim_key = birchtree::persistence::redb::schema::remote_cache_key(
        "https://cdn.example/0.png",
        "original",
    );
    let shell = ledger.get(&victim_key).expect("metadata shell survives");
    assert!(!shell.has_bytes);
    assert_eq!(shell.source_url, "https://cdn.example/0.png");

    // The source still lives: re-fetch re-reserves and re-commits.
    ledger.reserve(MediaClass::Full, 256 * 1024).unwrap();
    ledger
        .commit(
            MediaClass::Full,
            256 * 1024,
            stored_full(
                "https://cdn.example/0.png",
                "remote/0",
                256 * 1024,
                now + 2,
                &cfg,
            ),
        )
        .unwrap();
    assert_eq!(ledger.stored_total(), cfg.global_cap_bytes);
}

/// The avatar/emoji reserve survives image pressure: image eviction never
/// offers an avatar entry, and avatar bytes stay counted and intact.
#[test]
fn avatar_reserve_survives_image_pressure() {
    let cfg = test_cfg();
    let mut ledger = CacheLedger::new(cfg.clone());
    // Avatars take their whole reserve.
    ledger.reserve(MediaClass::AvatarEmoji, 256 * 1024).unwrap();
    ledger
        .commit(
            MediaClass::AvatarEmoji,
            256 * 1024,
            RemoteCacheEntry::stored(
                &CacheEntryCtx {
                    source_url: "https://cdn.example/avatar.png",
                    variant: "avatar",
                    class: MediaClass::AvatarEmoji,
                    blob_key: "remote/9",
                    now_ms: T0,
                    cfg: &cfg,
                },
                256 * 1024,
            ),
        )
        .unwrap();
    // Images fill the rest of the pool.
    for i in 0..3u64 {
        ledger.reserve(MediaClass::Full, 256 * 1024).unwrap();
        ledger
            .commit(
                MediaClass::Full,
                256 * 1024,
                stored_full(
                    &format!("https://cdn.example/{i}.png"),
                    &format!("remote/{i}"),
                    256 * 1024,
                    T0,
                    &cfg,
                ),
            )
            .unwrap();
    }
    // Image pressure beyond the cap: victims are image-only.
    let victims = ledger
        .evict_for(MediaClass::Full, 256 * 1024, T0 + 1)
        .expect("image room found without touching avatars");
    assert!(!victims.is_empty());
    assert!(
        !victims.iter().any(|k| k == "remote/9"),
        "avatar key offered to image pressure: {victims:?}"
    );
    assert_eq!(
        ledger.stored_for(MediaClass::AvatarEmoji),
        256 * 1024,
        "avatar bytes intact"
    );
    // And the plan layer never offers avatars to images even unfittingly.
    let plan = ledger.plan_eviction(MediaClass::Preview, 8 * 1024 * 1024, T0 + 1);
    assert!(!plan.iter().any(|k| {
        ledger
            .get(k)
            .is_some_and(|e| e.class == MediaClass::AvatarEmoji)
    }));
}

/// Local uploads survive aggressive eviction: the ledger refuses durable
/// keys, eviction yields only `remote/` keys, and deleting those keys leaves
/// the durable blob byte-identical.
#[test]
fn local_uploads_survive_aggressive_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();
    let durable_key = "000/000/001/original/a.jpg";
    let durable_bytes = b"local upload bytes";
    blobs.put(durable_key, durable_bytes).unwrap();
    let cache_key = "remote/42";
    blobs.put(cache_key, b"remote bytes").unwrap();

    let cfg = test_cfg();
    let mut ledger = CacheLedger::new(cfg.clone());

    // Durable keys are refused by the ledger outright.
    let bad = RemoteCacheEntry::stored(
        &CacheEntryCtx {
            source_url: "https://cdn.example/evil.png",
            variant: "original",
            class: MediaClass::Full,
            blob_key: durable_key,
            now_ms: T0,
            cfg: &cfg,
        },
        12,
    );
    assert!(matches!(
        ledger.commit(MediaClass::Full, 0, bad),
        Err(CacheError::Unsupported(_))
    ));
    assert!(!remote_cache::is_cache_key(durable_key));

    // Record the remote byte, then evict aggressively (more than the pool).
    ledger.reserve(MediaClass::Full, 12).unwrap();
    ledger
        .commit(
            MediaClass::Full,
            12,
            stored_full("https://cdn.example/r.png", cache_key, 12, T0, &cfg),
        )
        .unwrap();
    let victims = ledger
        .evict_for(MediaClass::Full, cfg.global_cap_bytes, T0 + 1)
        .expect("fits after evicting everything evictable");
    assert_eq!(victims, vec![cache_key.to_string()]);

    // Delete exactly the yielded keys.
    for victim in &victims {
        blobs.delete(victim).unwrap();
    }
    assert_eq!(
        blobs.get(durable_key).unwrap().unwrap(),
        durable_bytes,
        "durable upload byte-identical after aggressive eviction"
    );
    assert_eq!(blobs.get(cache_key).unwrap(), None);
}

/// Admission eviction is durable-first: victims are unmapped (rows +
/// counters) in one redb transaction before any blob is deleted, so redb and
/// disk agree without any reconciler pass — including across a restart that
/// drops the in-memory ledger.
#[test]
fn admission_eviction_keeps_redb_and_disk_in_agreement() {
    use std::time::Duration;

    let cfg = RemoteCacheConfig {
        mode: RemoteCacheMode::OnDemand,
        global_cap_bytes: 32 * 1024,
        avatar_emoji_cap_bytes: 256 * 1024,
        preview_cap_bytes: 512 * 1024,
        full_cap_bytes: 16 * 1024,
        avatar_retention_days: 30,
        warming: false,
    };
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("birchtree.redb");
    let cache_dir = dir.path().join("media_cache");

    fn disk_bytes(cache: &FsBlobStore) -> u64 {
        cache
            .list_keys(None, 500, Duration::ZERO)
            .expect("list")
            .iter()
            .map(|key| cache.size_of(key).expect("size").unwrap_or(0))
            .sum()
    }

    let victims: Vec<String> = {
        let store = RedbStore::open(
            &db_path,
            Arc::new(SystemClock),
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
        )
        .expect("open store");
        let cache = FsBlobStore::open(&cache_dir).expect("cache pool");
        let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
        // Four 4 KiB entries fill the 16 KiB class cap, with real files.
        for i in 0..4u64 {
            let source = format!("https://cdn.example/agree-{i}.png");
            let blob_key = format!("remote/{}", 1000 + i);
            cache.put(&blob_key, &vec![i as u8; 4096]).unwrap();
            store
                .cache_mark_stored(&source, "original", &blob_key, 4096, None, T0, &cfg)
                .unwrap();
            let mut guard = ledger.lock().unwrap();
            guard.reserve(MediaClass::Full, 4096).unwrap();
            guard
                .commit(
                    MediaClass::Full,
                    4096,
                    RemoteCacheEntry::stored(
                        &CacheEntryCtx {
                            source_url: &source,
                            variant: "original",
                            class: MediaClass::Full,
                            blob_key: &blob_key,
                            now_ms: T0,
                            cfg: &cfg,
                        },
                        4096,
                    ),
                )
                .unwrap();
        }
        assert_eq!(store.cache_accounting().unwrap().stored_full, 16 * 1024);

        // Admission of 8 KiB more: the shared durable-first primitive frees
        // room (rows + counters first, blobs second, ledger last).
        let outcome = evict_pressure_durable_first(
            &store,
            &cache,
            None,
            &ledger,
            MediaClass::Full,
            8 * 1024,
            T0 + 1,
            MAINTENANCE_BATCH,
        )
        .expect("durable-first eviction");
        assert!(!outcome.victims.is_empty());
        // The freed room admits the new reservation in the ledger.
        ledger
            .lock()
            .unwrap()
            .reserve(MediaClass::Full, 8 * 1024)
            .expect("room was made");

        // Agreement with NO reconciler pass: redb claims exactly the bytes
        // on disk, and every victim is gone from both.
        let usage = store.cache_accounting().unwrap();
        assert_eq!(usage.stored_total, disk_bytes(&cache));
        assert_eq!(usage.stored_full, 8 * 1024);
        for victim in &outcome.victims {
            assert!(!cache.exists(victim).unwrap(), "{victim} deleted");
            let row = store.cache_row_by_blob_key(victim).unwrap();
            assert!(
                row.is_none_or(|(_, row)| !row.has_bytes),
                "{victim} unmapped"
            );
        }
        outcome.victims
    };

    // Restart: the ledger is gone, the store reopens, and agreement still
    // holds with no reconciler pass.
    let store = RedbStore::open(
        &db_path,
        Arc::new(SystemClock),
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
    )
    .expect("reopen store");
    let cache = FsBlobStore::open(&cache_dir).expect("cache pool");
    assert_eq!(
        store.cache_accounting().unwrap().stored_total,
        disk_bytes(&cache)
    );
    for victim in &victims {
        assert!(!cache.exists(victim).unwrap(), "{victim} stays deleted");
    }
}

/// Startup cap enforcement: over-cap rows after a restore (e.g. a shadow
/// migration that adopted more bytes than the caps allow) are evicted with
/// the same durable-first primitive before traffic is accepted.
#[test]
fn startup_enforcement_brings_over_cap_pool_within_caps() {
    use std::time::Duration;

    let cfg = RemoteCacheConfig {
        mode: RemoteCacheMode::OnDemand,
        global_cap_bytes: 32 * 1024,
        avatar_emoji_cap_bytes: 256 * 1024,
        preview_cap_bytes: 512 * 1024,
        full_cap_bytes: 16 * 1024,
        avatar_retention_days: 30,
        warming: false,
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(open_store(dir.path()));
    let cache = FsBlobStore::open(dir.path().join("media_cache")).expect("cache pool");
    // Five 4 KiB entries: 20 KiB over the 16 KiB class cap.
    for i in 0..5u64 {
        let source = format!("https://cdn.example/startup-{i}.png");
        let blob_key = format!("remote/{}", 2000 + i);
        cache.put(&blob_key, &vec![i as u8; 4096]).unwrap();
        store
            .cache_mark_stored(&source, "original", &blob_key, 4096, None, T0, &cfg)
            .unwrap();
    }
    assert_eq!(store.cache_accounting().unwrap().stored_full, 20 * 1024);

    let maint = CacheMaintenance::new(store.clone(), cache.clone(), cfg.clone());
    let report = maint.enforce_caps_at_startup(T0 + 1).expect("enforce");
    assert!(report.pressure > 0);

    let usage = store.cache_accounting().unwrap();
    assert!(usage.stored_full <= cfg.full_cap_bytes);
    assert!(usage.stored_total <= cfg.global_cap_bytes);
    let on_disk: u64 = cache
        .list_keys(None, 500, Duration::ZERO)
        .expect("list")
        .iter()
        .map(|key| cache.size_of(key).expect("size").unwrap_or(0))
        .sum();
    assert_eq!(usage.stored_total, on_disk);
}

/// Oversize / unsupported / missing / exhausted map to Mastodon
/// `422`/`404`/`503` with `{ "error": ... }` — never a Misskey Drive shape.
#[test]
fn refusal_shapes_are_mastodon() {
    let cfg = test_cfg();
    let mut ledger = CacheLedger::new(cfg.clone());

    let oversize = ledger.reserve(MediaClass::Full, PER_FILE_CEILING_BYTES + 1);
    assert!(matches!(oversize, Err(CacheError::Oversize { .. })));
    assert_eq!(MAX_REMOTE_MEDIA_BYTES, PER_FILE_CEILING_BYTES);

    let stream = ledger.reserve(MediaClass::Stream, 1024);
    assert!(matches!(stream, Err(CacheError::Unsupported(_))));

    // Fill the pool, then refuse one more byte with 503.
    ledger
        .reserve(MediaClass::Full, cfg.global_cap_bytes)
        .unwrap();
    ledger
        .commit(
            MediaClass::Full,
            cfg.global_cap_bytes,
            stored_full(
                "https://cdn.example/big.png",
                "remote/1",
                cfg.global_cap_bytes,
                T0,
                &cfg,
            ),
        )
        .unwrap();
    let exhausted = ledger.reserve(MediaClass::Full, 1);
    assert!(matches!(exhausted, Err(CacheError::BudgetExhausted { .. })));

    for err in [
        CacheError::Oversize {
            size: 1,
            ceiling: 1,
        },
        CacheError::Unsupported("bad content".into()),
        CacheError::Missing("attachment has no source".into()),
        CacheError::OriginDown("origin timed out".into()),
        CacheError::BudgetExhausted { need: 1, cap: 1 },
        CacheError::Store("blob store failed".into()),
    ] {
        let status = err.http_status();
        assert!(
            matches!(status, 422 | 404 | 503),
            "Mastodon status, got {status}"
        );
        let body = err.error_body();
        assert!(body.get("error").is_some());
        assert!(!body.to_string().to_lowercase().contains("drive"));
    }
}

/// Warming defaults off and is always relay-excluded.
#[test]
fn warming_stays_off_and_relay_excluded() {
    let cfg = test_cfg();
    assert!(!cfg.warming);
    assert!(!media::should_warm(&cfg, false));
    let on = RemoteCacheConfig {
        warming: true,
        ..test_cfg()
    };
    assert!(media::should_warm(&on, false));
    assert!(!media::should_warm(&on, true), "relay is always excluded");
}

/// Metadata-first ingest records the source with no bytes: the durable
/// source URL is stored, no blob exists under the row key, and the ledger
/// holds a metadata-only shell.
#[test]
fn metadata_first_records_source_without_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let blobs = FsBlobStore::open(dir.path().join("media")).unwrap();

    let account = create_account(
        &store,
        "https://example.social",
        T0,
        CreateAccountCommand {
            username: "local".to_string(),
            email: None,
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("account");
    let attachment = store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "image".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media row");

    let source = "https://cdn.example/attachment.png";
    let entry =
        media::record_metadata_first(&store, attachment.id, source, T0).expect("metadata first");
    assert!(!entry.has_bytes);
    assert_eq!(entry.source_url, source);
    assert_eq!(entry.blob_key, format!("remote/{}", attachment.id.value()));

    // Durable metadata, zero bytes at ingest.
    assert_eq!(
        store.remote_media_source(attachment.id).unwrap().as_deref(),
        Some(source)
    );
    assert_eq!(blobs.get(&entry.blob_key).unwrap(), None);

    // Per-row keys: two attachments never share.
    let other = Id::new(attachment.id.value() + 1);
    assert_ne!(
        media::cache_key_for(attachment.id),
        media::cache_key_for(other)
    );
}

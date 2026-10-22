//! Workstream B2: eviction resurrection, TTL expiry, delete errors.

mod common;

use std::sync::Arc;
use std::sync::Mutex;

use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{BlobStore, CacheStore, MediaStore, NewMedia};
use birchtree::config::{RemoteCacheConfig, RemoteCacheMode};
use birchtree::domain::{SystemClock, TimestampIdGenerator};
use birchtree::media::blob_store::FsBlobStore;
use birchtree::media::cache_maintenance::{CacheMaintenance, evict_pressure_durable_first};
use birchtree::media::remote_cache::MediaClass;
use birchtree::persistence::redb::RedbStore;
use common::{T0, mk_account};

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

fn ledger_for(
    cfg: RemoteCacheConfig,
) -> &'static Mutex<birchtree::media::remote_cache::CacheLedger> {
    Box::leak(Box::new(Mutex::new(
        birchtree::media::remote_cache::CacheLedger::new(cfg),
    )))
}

/// Crash between the metadata commit and the file delete: the row is a
/// shell, the file survives. Restore must delete the file, not re-adopt it.
#[test]
fn b2_crash_between_commit_and_delete_deletes_instead_of_adopting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(open_store(dir.path()));
    let cache = FsBlobStore::open(dir.path().join("media_cache")).expect("cache pool");
    store.attach_cache_pool(cache.clone());
    let cfg = test_cfg();

    let account = mk_account(&store, "crash");
    let media = store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "image".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media");
    let blob_key = format!("remote/{}", media.id.value());
    let source = "https://cdn.example/crash.png";
    store
        .set_remote_media_source(media.id, source)
        .expect("source");
    store
        .stage_remote_media(media.id, &blob_key, "image/png", 8, T0)
        .expect("stage")
        .expect("staged");
    let bytes = b"crash-bytes".to_vec();
    cache.put(&blob_key, &bytes).expect("put");
    store
        .cache_mark_stored(
            source,
            "original",
            &blob_key,
            bytes.len() as u64,
            None,
            T0,
            &cfg,
        )
        .expect("stored");
    assert!(store.cache_accounting().expect("acct").stored_total > 0);

    // Evict in redb (commits), but do NOT delete the file: the crash window.
    let freed = store.cache_mark_evicted(source, "original").expect("evict");
    assert_eq!(freed.as_deref(), Some(blob_key.as_str()));
    assert!(
        cache.exists(&blob_key).expect("exists"),
        "file survives the crash"
    );
    let shell = store
        .cache_get(source, "original")
        .expect("get")
        .expect("shell");
    assert!(!shell.has_bytes);

    // Restore must treat the surviving file as residue, not as lost bytes.
    let maintenance = CacheMaintenance::new(store.clone(), cache.clone(), cfg.clone())
        .with_ledger(ledger_for(cfg.clone()));
    let report = maintenance.restore_at_startup(T0 + 1).expect("restore");
    assert_eq!(
        cache.get(&blob_key).expect("get"),
        None,
        "evicted file deleted, not adopted"
    );
    let after = store
        .cache_get(source, "original")
        .expect("get")
        .expect("shell");
    assert!(!after.has_bytes, "row stays evicted");
    assert_eq!(store.cache_accounting().expect("acct").stored_total, 0);
    assert_eq!(report.adopted, 0, "nothing adopted from an evicted row");
}

/// TTL expiry cannot resurrect: an expired row evicted in redb with its file
/// left behind stays evicted after restore.
#[test]
fn b2_ttl_expiry_cannot_resurrect() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(open_store(dir.path()));
    let cache = FsBlobStore::open(dir.path().join("media_cache")).expect("cache pool");
    store.attach_cache_pool(cache.clone());
    let mut cfg = test_cfg();
    cfg.avatar_retention_days = 7;
    let day_ms: u64 = 24 * 60 * 60 * 1000;

    let account = mk_account(&store, "ttl");
    let media = store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "image".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media");
    let blob_key = format!("remote/emojis/cdn.example/ttl/original.png");
    let source = "https://cdn.example/ttl.png";
    store
        .set_remote_media_source(media.id, source)
        .expect("source");
    // Stage the media row so the blob counts as referenced, then store the
    // emoji-variant ledger row directly (emoji keys are namespaced).
    store
        .stage_remote_media(media.id, &blob_key, "image/png", 4, T0)
        .expect("stage")
        .expect("staged");
    cache.put(&blob_key, b"emoji").expect("put");
    store
        .cache_mark_stored(source, "emoji", &blob_key, 5, None, T0, &cfg)
        .expect("stored");

    // Expire in redb, leave the file: the crash window again.
    let expired = store
        .cache_expire_batch(T0 + 7 * day_ms + 1, 500)
        .expect("expire");
    assert!(
        expired.contains(&blob_key),
        "emoji expired at 7d: {expired:?}"
    );
    assert!(cache.exists(&blob_key).expect("exists"));

    let maintenance = CacheMaintenance::new(store.clone(), cache.clone(), cfg.clone())
        .with_ledger(ledger_for(cfg.clone()));
    let report = maintenance
        .restore_at_startup(T0 + 7 * day_ms + 2)
        .expect("restore");
    assert_eq!(cache.get(&blob_key).expect("get"), None);
    assert_eq!(report.adopted, 0);
    assert_eq!(store.cache_accounting().expect("acct").stored_total, 0);
}

/// A blob delete failure still syncs the ledger for the committed mapping
/// and reports the key for the reconciler.
struct FailingDeleteStore {
    inner: FsBlobStore,
}

impl BlobStore for FailingDeleteStore {
    fn put(&self, key: &str, bytes: &[u8]) -> birchtree::domain::Result<()> {
        self.inner.put(key, bytes)
    }
    fn get(&self, key: &str) -> birchtree::domain::Result<Option<Vec<u8>>> {
        self.inner.get(key)
    }
    fn delete(&self, _key: &str) -> birchtree::domain::Result<()> {
        Err(birchtree::domain::Error::Storage(
            "injected delete failure".into(),
        ))
    }
    fn exists(&self, key: &str) -> birchtree::domain::Result<bool> {
        self.inner.exists(key)
    }
}

#[test]
fn b2_delete_error_leaves_ledger_and_redb_equal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let cache = FsBlobStore::open(dir.path().join("media_cache")).expect("cache pool");
    let cfg = test_cfg();
    for i in 0..2u64 {
        let source = format!("https://cdn.example/del-{i}.png");
        let blob_key = format!("remote/{}", 700 + i);
        cache.put(&blob_key, &vec![i as u8; 1024]).expect("put");
        store
            .cache_mark_stored(&source, "original", &blob_key, 1024, None, T0, &cfg)
            .expect("stored");
    }
    assert_eq!(store.cache_accounting().expect("acct").stored_total, 2048);
    let ledger = Mutex::new(birchtree::media::remote_cache::CacheLedger::new(
        cfg.clone(),
    ));
    {
        let mut guard = ledger.lock().expect("lock");
        for i in 0..2u64 {
            let source = format!("https://cdn.example/del-{i}.png");
            let blob_key = format!("remote/{}", 700 + i);
            let entry = birchtree::media::remote_cache::RemoteCacheEntry::stored(
                &source,
                "original",
                MediaClass::Full,
                &blob_key,
                1024,
                T0,
                &cfg,
            );
            guard.restore_entry(entry).expect("restore");
        }
    }
    let failing = FailingDeleteStore {
        inner: cache.clone(),
    };
    let outcome = evict_pressure_durable_first(
        &store,
        &failing,
        None,
        &ledger,
        MediaClass::Full,
        1024,
        T0 + 1,
        500,
    )
    .expect("eviction commits despite delete failure");
    assert!(!outcome.victims.is_empty());
    assert!(
        !outcome.failed.is_empty(),
        "failed keys reported: {outcome:?}"
    );
    assert_eq!(outcome.failed.len(), outcome.victims.len());
    // Ledger follows the committed redb rows even though the files remain.
    let redb_total = store.cache_accounting().expect("acct").stored_total;
    let ledger_total = ledger.lock().expect("lock").stored_total();
    assert_eq!(
        ledger_total, redb_total,
        "ledger and redb agree after delete failure"
    );
    // Files remain for the reconciler to retry.
    for key in &outcome.failed {
        assert!(
            cache.exists(key).expect("exists"),
            "{key} left for reconciler"
        );
    }
}

/// Guard: an in-use blob whose ledger row was lost (no row at all) is still
/// adopted, and durable local uploads are never touched.
#[test]
fn b2_lost_row_still_adopted_and_durable_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(open_store(dir.path()));
    let cache = FsBlobStore::open(dir.path().join("media_cache")).expect("cache pool");
    let durable = FsBlobStore::open(dir.path().join("media")).expect("durable pool");
    store.attach_cache_pool(cache.clone());
    let cfg = test_cfg();

    let account = mk_account(&store, "adopt");
    let media = store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "image".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media");
    let blob_key = format!("remote/{}", media.id.value());
    let source = "https://cdn.example/adopt.png";
    store
        .set_remote_media_source(media.id, source)
        .expect("source");
    store
        .stage_remote_media(media.id, &blob_key, "image/png", 6, T0)
        .expect("stage")
        .expect("staged");
    let bytes = b"adopt-me".to_vec();
    cache.put(&blob_key, &bytes).expect("put");
    // No ledger row at all: restore must adopt.
    assert!(store.cache_get(source, "original").expect("get").is_none());

    let durable_key = "00/11/22/original/local.jpg";
    durable.put(durable_key, b"local").expect("durable put");

    let maintenance = CacheMaintenance::new(store.clone(), cache.clone(), cfg.clone())
        .with_ledger(ledger_for(cfg.clone()));
    let report = maintenance.restore_at_startup(T0 + 1).expect("restore");
    assert!(report.adopted >= 1, "lost in-use blob adopted");
    let row = store
        .cache_get(source, "original")
        .expect("get")
        .expect("row");
    assert!(row.has_bytes);
    assert_eq!(row.blob_key, blob_key);
    // Durable uploads untouched.
    assert_eq!(
        durable.get(durable_key).expect("get"),
        Some(b"local".to_vec())
    );
    assert_eq!(cache.get(durable_key).expect("get"), None);
}

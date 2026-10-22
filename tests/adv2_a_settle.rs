//! Workstream A (admission + fetch path): settle-step fault injection.
//!
//! The settle step (`federation::media::settle_staged`, shared by the
//! streamed and buffered fetch paths) is driven directly through narrow
//! fault-injecting doubles:
//!
//! - `cache_mark_stored` fails → the published blob is deleted, no row
//!   claims bytes, the reservation is freed;
//! - the ledger commit fails (disabled flip between reserve and settle) →
//!   the blob is deleted *and* the durable row is evicted via
//!   `cache_mark_evicted`;
//! - `stage_remote_media` fails or reports the row gone → blob, durable
//!   row and ledger mapping are all unwound;
//! - blob deletion itself fails → the reservation is still freed and no
//!   row claims bytes (the surviving blob is reconciler residue).
//!
//! A top-up case covers N2 (`staged > reserved` re-reserves or fails
//! loudly). These tests use local ledgers, so they need no serialization.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{BlobStore, CacheStore, MediaStore};
use birchtree::config::RemoteCacheConfig;
use birchtree::domain::{Error, Id, Result};
use birchtree::federation::delivery::DeliveryError;
use birchtree::federation::media;
use birchtree::media::remote_cache::{CacheLedger, MediaClass, Reservation};
use birchtree::persistence::redb::RedbStore;

use common::temp_store;

const T0: u64 = 1_700_000_000_000;

fn settle_caps() -> RemoteCacheConfig {
    RemoteCacheConfig {
        global_cap_bytes: 1024 * 1024,
        avatar_emoji_cap_bytes: 256 * 1024,
        preview_cap_bytes: 512 * 1024,
        full_cap_bytes: 512 * 1024,
        ..RemoteCacheConfig::default()
    }
}

/// The settle seam: the four durable writes, faultable one by one.
struct FaultSettle {
    inner: Arc<RedbStore>,
    fail_mark_stored: AtomicBool,
    fail_stage: AtomicBool,
    stage_missing: AtomicBool,
}

impl FaultSettle {
    fn new(inner: &Arc<RedbStore>) -> Self {
        Self {
            inner: inner.clone(),
            fail_mark_stored: AtomicBool::new(false),
            fail_stage: AtomicBool::new(false),
            stage_missing: AtomicBool::new(false),
        }
    }
}

impl media::SettleStore for FaultSettle {
    fn cache_mark_stored(
        &self,
        source_url: &str,
        variant: &str,
        blob_key: &str,
        size: u64,
        etag: Option<String>,
        now_ms: u64,
        cfg: &RemoteCacheConfig,
    ) -> Result<()> {
        if self.fail_mark_stored.load(Ordering::SeqCst) {
            return Err(Error::Storage("injected cache_mark_stored failure".into()));
        }
        CacheStore::cache_mark_stored(
            &*self.inner,
            source_url,
            variant,
            blob_key,
            size,
            etag,
            now_ms,
            cfg,
        )
    }

    fn cache_mark_evicted(&self, source_url: &str, variant: &str) -> Result<Option<String>> {
        CacheStore::cache_mark_evicted(&*self.inner, source_url, variant)
    }

    fn stage_remote_media(
        &self,
        media_id: Id,
        file_key: &str,
        mime_type: &str,
        file_size: u64,
        now_ms: u64,
    ) -> Result<Option<birchtree::domain::MediaAttachment>> {
        if self.fail_stage.load(Ordering::SeqCst) {
            return Err(Error::Storage("injected stage_remote_media failure".into()));
        }
        if self.stage_missing.load(Ordering::SeqCst) {
            return Ok(None);
        }
        FederationContentStore::stage_remote_media(
            &*self.inner,
            media_id,
            file_key,
            mime_type,
            file_size,
            now_ms,
        )
    }

    fn mark_remote_media_failed(&self, media_id: Id, reason: &str, now_ms: u64) -> Result<()> {
        FederationContentStore::mark_remote_media_failed(&*self.inner, media_id, reason, now_ms)
    }
}

/// In-memory blobs with a faultable delete.
#[derive(Default)]
struct FaultBlobs {
    map: Mutex<HashMap<String, Vec<u8>>>,
    fail_delete: AtomicBool,
    delete_attempts: AtomicUsize,
}

impl BlobStore for FaultBlobs {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        self.map
            .lock()
            .unwrap()
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.map.lock().unwrap().get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.delete_attempts.fetch_add(1, Ordering::SeqCst);
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(Error::Storage("injected blob deletion failure".into()));
        }
        self.map.lock().unwrap().remove(key);
        Ok(())
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.map.lock().unwrap().contains_key(key))
    }
}

fn test_media(store: &RedbStore, source: &str) -> Id {
    use birchtree::application::ports::NewMedia;

    let account = common::mk_account(store, &format!("settle-{T0}"));
    let attachment = store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "unknown".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media row");
    store
        .set_remote_media_source(attachment.id, source)
        .expect("source");
    attachment.id
}

/// F6: `cache_mark_stored` fails → blob deleted, no row, reservation freed.
#[test]
fn mark_stored_failure_rolls_back() {
    let (_tmp, store) = temp_store();
    let cfg = settle_caps();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let settle = FaultSettle::new(&store);
    settle.fail_mark_stored.store(true, Ordering::SeqCst);
    let blobs = FaultBlobs::default();

    let source = "https://cdn.example/s1.png";
    let key = "remote/s1";
    let media_id = test_media(&store, source);
    blobs.put(key, b"bytes").expect("publish first");
    let mut reservation = Reservation::reserve(&ledger, MediaClass::Full, 1000).expect("reserve");

    let outcome = media::settle_staged(
        &settle,
        &blobs,
        &mut reservation,
        source,
        "original",
        media_id,
        key,
        5,
        "image/png",
        T0,
        &cfg,
    );
    assert!(
        matches!(outcome, Err(DeliveryError::Retry(_))),
        "transient, got {outcome:?}"
    );
    drop(reservation);

    assert_eq!(blobs.get(key).unwrap(), None, "no orphan blob");
    assert_eq!(
        store
            .cache_get(source, "original")
            .unwrap()
            .map(|row| row.has_bytes),
        None,
        "no row claims bytes"
    );
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_total(), 0);
}

/// F6: ledger commit fails → blob *and* durable row go, reservation freed.
#[test]
fn commit_failure_evicts_the_durable_row() {
    let (_tmp, store) = temp_store();
    let cfg = settle_caps();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let settle = FaultSettle::new(&store);
    let blobs = FaultBlobs::default();

    let source = "https://cdn.example/s2.png";
    let key = "remote/s2";
    let media_id = test_media(&store, source);
    blobs.put(key, b"bytes").expect("publish first");
    let mut reservation = Reservation::reserve(&ledger, MediaClass::Full, 1000).expect("reserve");
    // Flip the ledger to disabled between reserve and settle: the commit
    // refuses loudly.
    ledger.lock().unwrap().set_caps(RemoteCacheConfig {
        mode: birchtree::config::RemoteCacheMode::Disabled,
        ..cfg.clone()
    });

    let outcome = media::settle_staged(
        &settle,
        &blobs,
        &mut reservation,
        source,
        "original",
        media_id,
        key,
        5,
        "image/png",
        T0,
        &cfg,
    );
    assert!(
        matches!(outcome, Err(DeliveryError::Quarantine(_))),
        "refused, got {outcome:?}"
    );
    drop(reservation);

    assert_eq!(blobs.get(key).unwrap(), None, "no orphan blob");
    assert_eq!(
        store
            .cache_get(source, "original")
            .unwrap()
            .map(|row| row.has_bytes),
        Some(false),
        "durable row evicted back to a shell"
    );
    assert_eq!(store.cache_accounting().unwrap().stored_total, 0);
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_total(), 0);
}

/// F6: `stage_remote_media` fails → blob, row and mapping all unwind.
#[test]
fn stage_failure_rolls_back() {
    let (_tmp, store) = temp_store();
    let cfg = settle_caps();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let settle = FaultSettle::new(&store);
    settle.fail_stage.store(true, Ordering::SeqCst);
    let blobs = FaultBlobs::default();

    let source = "https://cdn.example/s3.png";
    let key = "remote/s3";
    let media_id = test_media(&store, source);
    blobs.put(key, b"bytes").expect("publish first");
    let mut reservation = Reservation::reserve(&ledger, MediaClass::Full, 1000).expect("reserve");

    let outcome = media::settle_staged(
        &settle,
        &blobs,
        &mut reservation,
        source,
        "original",
        media_id,
        key,
        5,
        "image/png",
        T0,
        &cfg,
    );
    assert!(
        matches!(outcome, Err(DeliveryError::Retry(_))),
        "transient, got {outcome:?}"
    );
    drop(reservation);

    assert_eq!(blobs.get(key).unwrap(), None);
    assert_eq!(
        store
            .cache_get(source, "original")
            .unwrap()
            .map(|row| row.has_bytes),
        Some(false),
        "durable row evicted back to a shell"
    );
    assert_eq!(store.cache_accounting().unwrap().stored_total, 0);
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_total(), 0);
}

/// The row-gone path (stage reports the row missing) unwinds the same way
/// and fails the row loudly — terminal, never a retry loop.
#[test]
fn missing_row_unwinds_and_quarantines() {
    let (_tmp, store) = temp_store();
    let cfg = settle_caps();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let settle = FaultSettle::new(&store);
    settle.stage_missing.store(true, Ordering::SeqCst);
    let blobs = FaultBlobs::default();

    let source = "https://cdn.example/s4.png";
    let key = "remote/s4";
    let media_id = test_media(&store, source);
    blobs.put(key, b"bytes").expect("publish first");
    let mut reservation = Reservation::reserve(&ledger, MediaClass::Full, 1000).expect("reserve");

    let outcome = media::settle_staged(
        &settle,
        &blobs,
        &mut reservation,
        source,
        "original",
        media_id,
        key,
        5,
        "image/png",
        T0,
        &cfg,
    );
    assert!(
        matches!(outcome, Err(DeliveryError::Quarantine(ref msg)) if msg.contains("missing")),
        "terminal, got {outcome:?}"
    );
    drop(reservation);

    assert_eq!(blobs.get(key).unwrap(), None);
    assert_eq!(
        store
            .cache_get(source, "original")
            .unwrap()
            .map(|row| row.has_bytes),
        Some(false)
    );
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_total(), 0);
    let row = store.media_by_id(media_id).expect("row").expect("media");
    assert!(
        row.error.as_deref().is_some_and(|e| e.contains("missing")),
        "loud failure recorded: {:?}",
        row.error
    );
}

/// Blob deletion itself fails: the reservation is still freed and no row
/// claims bytes. The surviving blob is reconciler residue.
#[test]
fn rollback_survives_blob_deletion_failure() {
    let (_tmp, store) = temp_store();
    let cfg = settle_caps();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let settle = FaultSettle::new(&store);
    settle.fail_mark_stored.store(true, Ordering::SeqCst);
    let blobs = FaultBlobs::default();
    blobs.fail_delete.store(true, Ordering::SeqCst);

    let source = "https://cdn.example/s5.png";
    let key = "remote/s5";
    let media_id = test_media(&store, source);
    blobs.put(key, b"bytes").expect("publish first");
    let mut reservation = Reservation::reserve(&ledger, MediaClass::Full, 1000).expect("reserve");

    let outcome = media::settle_staged(
        &settle,
        &blobs,
        &mut reservation,
        source,
        "original",
        media_id,
        key,
        5,
        "image/png",
        T0,
        &cfg,
    );
    assert!(outcome.is_err(), "still fails, got {outcome:?}");
    drop(reservation);

    assert!(
        blobs.delete_attempts.load(Ordering::SeqCst) > 0,
        "rollback attempted the delete"
    );
    assert_eq!(
        store
            .cache_get(source, "original")
            .unwrap()
            .map(|row| row.has_bytes),
        None,
        "no row claims bytes"
    );
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_total(), 0);
}

/// N2: `staged > reserved` re-reserves the difference and succeeds.
#[test]
fn top_up_grows_the_reservation_and_settles() {
    let (_tmp, store) = temp_store();
    let cfg = settle_caps();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let settle = FaultSettle::new(&store);
    let blobs = FaultBlobs::default();

    let source = "https://cdn.example/s6.png";
    let key = "remote/s6";
    let media_id = test_media(&store, source);
    blobs.put(key, &vec![9u8; 200]).expect("publish first");
    let mut reservation = Reservation::reserve(&ledger, MediaClass::Full, 100).expect("reserve");

    media::settle_staged(
        &settle,
        &blobs,
        &mut reservation,
        source,
        "original",
        media_id,
        key,
        200,
        "image/png",
        T0,
        &cfg,
    )
    .expect("top-up settles");
    drop(reservation);

    assert_eq!(blobs.get(key).unwrap().expect("blob").len(), 200);
    assert_eq!(store.cache_accounting().unwrap().stored_total, 200);
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_for(MediaClass::Full), 200);
}

/// N2: when the top-up itself is refused, the fetch fails and the blob goes.
#[test]
fn refused_top_up_fails_and_deletes_the_blob() {
    let (_tmp, store) = temp_store();
    let cfg = settle_caps();
    let ledger = Mutex::new(CacheLedger::new(cfg.clone()));
    let settle = FaultSettle::new(&store);
    let blobs = FaultBlobs::default();

    let source = "https://cdn.example/s7.png";
    let key = "remote/s7";
    let media_id = test_media(&store, source);
    blobs.put(key, &vec![9u8; 200]).expect("publish first");
    let mut reservation = Reservation::reserve(&ledger, MediaClass::Full, 100).expect("reserve");
    // Shrink the class cap below the top-up between reserve and settle.
    ledger.lock().unwrap().set_caps(RemoteCacheConfig {
        full_cap_bytes: 150,
        ..cfg.clone()
    });

    let outcome = media::settle_staged(
        &settle,
        &blobs,
        &mut reservation,
        source,
        "original",
        media_id,
        key,
        200,
        "image/png",
        T0,
        &cfg,
    );
    assert!(outcome.is_err(), "refused, got {outcome:?}");
    drop(reservation);

    assert_eq!(blobs.get(key).unwrap(), None, "blob deleted");
    assert_eq!(
        store
            .cache_get(source, "original")
            .unwrap()
            .map(|row| row.has_bytes),
        None,
        "no row claims bytes"
    );
    let held = ledger.lock().unwrap();
    assert_eq!(held.inflight_total(), 0);
    assert_eq!(held.stored_total(), 0);
}

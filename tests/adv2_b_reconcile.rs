//! Wave 2 B4: reconcile only rebuilds when the page changed something.
//!
//! A no-op page does no ledger rebuild, so a concurrent reservation made
//! during reconcile is never lost.

mod common;

use std::sync::{Arc, Mutex};

use birchtree::config::{RemoteCacheConfig, RemoteCacheMode};
use birchtree::media::cache_maintenance::CacheMaintenance;
use birchtree::media::reconciler::ReconCursor;
use birchtree::media::remote_cache::{CacheLedger, MediaClass};

use common::temp_store;

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

#[test]
fn noop_reconcile_keeps_concurrent_reservation() {
    let (_dir, store) = temp_store();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache =
        birchtree::media::blob_store::FsBlobStore::open(cache_dir.path().join("c")).expect("pool");
    let cfg = test_cfg();
    // Leak a ledger so the maintenance service can hold it statically.
    let ledger: &'static Mutex<CacheLedger> =
        Box::leak(Box::new(Mutex::new(CacheLedger::new(cfg.clone()))));
    ledger
        .lock()
        .unwrap()
        .reserve(MediaClass::Full, 64 * 1024)
        .expect("reserve holds budget");
    let maint = CacheMaintenance::new(store, cache, cfg).with_ledger(ledger);
    let outcome = maint
        .run_reconcile(1_700_000_000_000, &ReconCursor::default())
        .expect("reconcile");
    // Empty tree: nothing adopted, nothing repaired, ledger untouched.
    assert_eq!(outcome.adopted, 0);
    assert_eq!(outcome.accounting.before_total, 0);
    assert_eq!(outcome.accounting.after_total, 0);
    let guard = ledger.lock().unwrap();
    assert_eq!(
        guard.inflight_for(MediaClass::Full),
        64 * 1024,
        "concurrent reservation survives a no-op page"
    );
}

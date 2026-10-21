//! Idempotent storage reconciler sweep (Agent C, storage overhaul
//! `docs/database.md`).
//!
//! The filesystem and `redb` cannot commit atomically: bytes are staged
//! first, metadata commits afterwards, so a crash can leave orphans
//! (bytes without metadata), missing blobs (metadata without bytes), stale
//! metadata (expired TTL rows whose bytes should be gone), and unfinished
//! deletions (a crash between the metadata commit and the byte delete).
//! This module is the bounded, idempotent sweep that converges those states
//! back to zero.
//!
//! The sweep is storage-agnostic: it runs against the [`ReconStore`] trait
//! (one paged listing + idempotent point operations per category), so no
//! blobs move inside a `redb` transaction — the parent's `redb`
//! implementation performs each point op in its own short transaction after
//! the listing commits, and the sweep itself never touches the network
//! (a missing blob is *reported* for re-fetch; the fetch happens outside the
//! sweep). No shell is used anywhere.
//!
//! Bounds: every listing page is capped at [`RECONCILE_BATCH_LIMIT`] (500)
//! keys; per-sweep work is therefore bounded and a large backlog converges
//! over repeated sweeps. Report payloads stay far under the 64 KiB job
//! budget (at most 500 short keys per category per sweep).

use std::collections::HashSet;

use crate::domain::Result;

/// Maximum keys processed per category per sweep (matches the
/// `ReconcileStorage` job contract in `src/domain/job.rs`).
pub const RECONCILE_BATCH_LIMIT: usize = 500;

/// Storage view a sweep runs against.
///
/// Implementations (the parent's `redb` adapter) must keep each method to a
/// short, bounded transaction and make every mutating method idempotent, so
/// a crashed sweep can simply run again.
pub trait ReconStore: Send + Sync {
    /// One page of stored byte keys (`None` starts from the beginning).
    fn list_blob_keys(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>>;
    /// One page of metadata keys (all rows, including stale/missing ones).
    fn list_meta_keys(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>>;
    /// Whether a metadata row exists for `key`.
    fn meta_has(&self, key: &str) -> Result<bool>;
    /// Whether bytes exist for `key`.
    fn blob_exists(&self, key: &str) -> Result<bool>;
    /// Whether the metadata row is past its TTL (`None` when the row is gone).
    fn meta_is_stale(&self, key: &str, now_ms: u64) -> Result<Option<bool>>;
    /// Whether the row was already flagged missing by an earlier sweep.
    fn meta_is_flagged_missing(&self, key: &str) -> Result<Option<bool>>;
    /// Idempotent byte delete.
    fn remove_blob(&self, key: &str) -> Result<()>;
    /// Idempotent metadata-row delete.
    fn remove_meta(&self, key: &str) -> Result<()>;
    /// Flag a metadata row as missing-bytes (reported once; the re-fetch or
    /// explicit missing-mark happens outside the sweep).
    fn flag_missing(&self, key: &str, now_ms: u64) -> Result<()>;
    /// One page of unfinished-deletion markers.
    fn list_pending_deletes(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>>;
    /// Clear an unfinished-deletion marker (idempotent).
    fn clear_pending_delete(&self, key: &str) -> Result<()>;
}

/// Outcome of one bounded sweep.
#[derive(Debug, Clone, Default)]
pub struct ReconReport {
    /// Blob keys scanned this sweep.
    pub scanned_blobs: usize,
    /// Metadata keys scanned this sweep.
    pub scanned_meta: usize,
    /// Orphan byte keys deleted (bytes without metadata).
    pub orphans_removed: usize,
    /// Metadata rows whose TTL expired (row + bytes deleted).
    pub stale_removed: usize,
    /// Metadata rows newly observed without bytes (flagged for re-fetch).
    pub missing_keys: Vec<String>,
    /// Unfinished deletions completed (bytes deleted + marker cleared).
    pub deletes_completed: usize,
    /// True when a listing page hit the batch cap (more work remains).
    pub truncated: bool,
}

impl ReconReport {
    /// Whether another sweep is expected to find nothing: no orphan, stale,
    /// or unfinished-deletion work was done and no page was truncated.
    /// Newly-missing rows do not block convergence — they are flagged once
    /// and skipped by later sweeps until an outside re-fetch heals them.
    pub fn converged(&self) -> bool {
        self.orphans_removed == 0
            && self.stale_removed == 0
            && self.deletes_completed == 0
            && !self.truncated
    }
}

/// Resume point of a multi-job sweep: the last key seen in each listing.
///
/// A sweep that stops at the batch cap hands this back so the next job
/// continues where it ended instead of re-reading the same first page
/// forever (healthy keys stay in the first page, so a sweep without a cursor
/// would never reach the rest of a large tree). Serialized into the job
/// payload; a handful of short keys, far under the payload budget.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReconCursor {
    /// Last blob key scanned (`None` starts from the beginning).
    pub blob_after: Option<String>,
    /// Last metadata key scanned.
    pub meta_after: Option<String>,
    /// The blob listing reached its end in an earlier page of this pass.
    pub blobs_done: bool,
    /// The metadata listing reached its end in an earlier page of this pass.
    pub metas_done: bool,
}

/// One page of a resumable sweep.
#[derive(Debug, Clone)]
pub struct ReconPage {
    /// What this page found and fixed.
    pub report: ReconReport,
    /// Where the next page continues; `None` when this page reached the end
    /// of both listings (a full pass is complete).
    pub next: Option<ReconCursor>,
}

/// Run one bounded, idempotent sweep from the beginning of the listings.
///
/// Every category is page-capped at `batch` (clamped to
/// `1..=RECONCILE_BATCH_LIMIT`), so a single sweep never scans without
/// bound; repeat sweeps converge the backlog to zero. Use
/// [`reconcile_page`] to walk the whole keyspace across jobs.
pub fn reconcile(store: &dyn ReconStore, now_ms: u64, batch: usize) -> Result<ReconReport> {
    Ok(reconcile_page(store, now_ms, batch, &ReconCursor::default())?.report)
}

/// Run one bounded sweep page starting at `cursor`.
pub fn reconcile_page(
    store: &dyn ReconStore,
    now_ms: u64,
    batch: usize,
    cursor: &ReconCursor,
) -> Result<ReconPage> {
    let cap = batch.clamp(1, RECONCILE_BATCH_LIMIT);
    let mut report = ReconReport::default();

    let blobs = if cursor.blobs_done {
        Vec::new()
    } else {
        store.list_blob_keys(cursor.blob_after.as_deref(), cap)?
    };
    let metas = if cursor.metas_done {
        Vec::new()
    } else {
        store.list_meta_keys(cursor.meta_after.as_deref(), cap)?
    };
    let pending = store.list_pending_deletes(None, cap)?;
    report.scanned_blobs = blobs.len();
    report.scanned_meta = metas.len();
    report.truncated = blobs.len() >= cap || metas.len() >= cap || pending.len() >= cap;

    let meta_set: HashSet<&str> = metas.iter().map(String::as_str).collect();
    for key in &blobs {
        if !meta_set.contains(key.as_str()) && !store.meta_has(key)? {
            // Double-checked against the store (the page snapshot may lag a
            // concurrent commit); the delete itself is idempotent.
            store.remove_blob(key)?;
            report.orphans_removed += 1;
        }
    }

    for key in &metas {
        match store.meta_is_stale(key, now_ms)? {
            // Stale metadata: evict bytes (idempotent) then drop the row.
            Some(true) => {
                store.remove_blob(key)?;
                store.remove_meta(key)?;
                report.stale_removed += 1;
            }
            _ => {
                if store.meta_is_flagged_missing(key)?.unwrap_or(false) {
                    continue;
                }
                if !store.blob_exists(key)? {
                    store.flag_missing(key, now_ms)?;
                    report.missing_keys.push(key.clone());
                }
            }
        }
    }

    for key in &pending {
        store.remove_blob(key)?;
        store.clear_pending_delete(key)?;
        report.deletes_completed += 1;
    }

    let blobs_done: bool = cursor.blobs_done || blobs.len() < cap;
    let metas_done: bool = cursor.metas_done || metas.len() < cap;
    let next = if blobs_done && metas_done {
        None
    } else {
        Some(ReconCursor {
            blob_after: blobs.last().cloned().or_else(|| cursor.blob_after.clone()),
            meta_after: metas.last().cloned().or_else(|| cursor.meta_after.clone()),
            blobs_done,
            metas_done,
        })
    };
    Ok(ReconPage { report, next })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    /// In-memory [`ReconStore`] double (the parent's `redb` adapter honors
    /// the same point-op contract in short transactions).
    pub struct MemoryStore {
        pub blobs: Mutex<BTreeSet<String>>,
        pub metas: Mutex<BTreeMap<String, MetaRow>>,
        pub pending: Mutex<BTreeSet<String>>,
    }

    #[derive(Clone)]
    pub struct MetaRow {
        pub stale: bool,
        pub missing: bool,
    }

    impl MemoryStore {
        pub fn new() -> Self {
            Self {
                blobs: Mutex::new(BTreeSet::new()),
                metas: Mutex::new(BTreeMap::new()),
                pending: Mutex::new(BTreeSet::new()),
            }
        }

        fn page(set: &BTreeSet<String>, after: Option<&str>, limit: usize) -> Vec<String> {
            set.iter()
                .filter(|k| after.is_none_or(|a| k.as_str() > a))
                .take(limit)
                .cloned()
                .collect()
        }
    }

    impl ReconStore for MemoryStore {
        fn list_blob_keys(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>> {
            Ok(Self::page(&self.blobs.lock().unwrap(), after, limit))
        }
        fn list_meta_keys(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>> {
            let metas = self.metas.lock().unwrap();
            let keys: BTreeSet<String> = metas.keys().cloned().collect();
            Ok(Self::page(&keys, after, limit))
        }
        fn meta_has(&self, key: &str) -> Result<bool> {
            Ok(self.metas.lock().unwrap().contains_key(key))
        }
        fn blob_exists(&self, key: &str) -> Result<bool> {
            Ok(self.blobs.lock().unwrap().contains(key))
        }
        fn meta_is_stale(&self, key: &str, _now_ms: u64) -> Result<Option<bool>> {
            Ok(self.metas.lock().unwrap().get(key).map(|row| row.stale))
        }
        fn meta_is_flagged_missing(&self, key: &str) -> Result<Option<bool>> {
            Ok(self
                .metas
                .lock()
                .unwrap()
                .get(key)
                .map(|row| row.missing))
        }
        fn remove_blob(&self, key: &str) -> Result<()> {
            self.blobs.lock().unwrap().remove(key);
            Ok(())
        }
        fn remove_meta(&self, key: &str) -> Result<()> {
            self.metas.lock().unwrap().remove(key);
            Ok(())
        }
        fn flag_missing(&self, key: &str, _now_ms: u64) -> Result<()> {
            if let Some(row) = self.metas.lock().unwrap().get_mut(key) {
                row.missing = true;
            }
            Ok(())
        }
        fn list_pending_deletes(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>> {
            Ok(Self::page(&self.pending.lock().unwrap(), after, limit))
        }
        fn clear_pending_delete(&self, key: &str) -> Result<()> {
            self.pending.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn row(stale: bool) -> MetaRow {
        MetaRow { stale, missing: false }
    }

    #[test]
    fn sweep_fixes_every_category_and_second_sweep_is_quiet() {
        let store = MemoryStore::new();
        // Orphans: bytes without metadata.
        store.blobs.lock().unwrap().insert("orphan/1".into());
        store.blobs.lock().unwrap().insert("orphan/2".into());
        // Missing: metadata without bytes.
        store.metas.lock().unwrap().insert("missing/1".into(), row(false));
        // Stale: expired metadata + leftover bytes.
        store.blobs.lock().unwrap().insert("stale/1".into());
        store.metas.lock().unwrap().insert("stale/1".into(), row(true));
        // Unfinished deletion.
        store.blobs.lock().unwrap().insert("gone/1".into());
        store.pending.lock().unwrap().insert("gone/1".into());
        // Healthy pair.
        store.blobs.lock().unwrap().insert("ok/1".into());
        store.metas.lock().unwrap().insert("ok/1".into(), row(false));

        let first = reconcile(&store, 1_700_000_000_000, 500).unwrap();
        // Three orphans: the two unmapped bytes plus the unfinished
        // deletion's bytes (no metadata row covers them, so the orphan scan
        // claims them before the pending-delete arm completes the marker).
        assert_eq!(first.orphans_removed, 3);
        assert_eq!(first.stale_removed, 1);
        assert_eq!(first.missing_keys, vec!["missing/1".to_string()]);
        assert_eq!(first.deletes_completed, 1);
        assert!(!first.truncated);

        // Zero orphans: every remaining byte key has metadata.
        for key in store.list_blob_keys(None, 500).unwrap() {
            assert!(store.meta_has(&key).unwrap(), "orphan left: {key}");
        }

        let second = reconcile(&store, 1_700_000_000_000, 500).unwrap();
        assert!(second.converged());
        assert!(second.missing_keys.is_empty());
    }

    #[test]
    fn batches_are_bounded_and_truncated() {
        let store = MemoryStore::new();
        for i in 0..10 {
            store.blobs.lock().unwrap().insert(format!("orphan/{i:02}"));
        }
        let report = reconcile(&store, 0, 4).unwrap();
        assert!(report.truncated);
        assert!(report.orphans_removed <= 4);
    }
}

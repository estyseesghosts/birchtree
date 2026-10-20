//! Bounded, restartable backfill for the embedded search index.
//!
//! A backfill run pages the public status corpus newest-first in bounded
//! batches and indexes every indexable row it meets. Each batch is
//! idempotent (re-indexing overwrites), so a retried batch is harmless, and
//! the run chains batches through a resume cursor: the next batch starts
//! below the oldest id the previous batch covered.
//!
//! Scope note: the run pages `recent_statuses` with no author, which covers
//! the public timeline corpus. Unlisted statuses are indexable but live
//! outside that corpus, so historical unlisted rows need the follow-up
//! `indexable_statuses_page` port (a direct bounded scan over the status
//! table); until it lands, unlisted rows join the index through the
//! incremental post-commit job only. The limitation is explicit in the final
//! report rather than silently scanned around.

use crate::application::ports::Store;
use crate::domain::{Id, Result};

use super::embedded::EmbeddedIndex;
use super::{indexable_text, is_indexable};

/// Default backfill batch size (statuses read per batch).
pub const BACKFILL_DEFAULT_BATCH: usize = 500;
/// Hard cap on a backfill batch (one bounded read transaction).
pub const BACKFILL_MAX_BATCH: usize = 2_000;

/// Outcome of one backfill batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillProgress {
    /// Indexable rows (re-)indexed by this batch.
    pub indexed: usize,
    /// Resume cursor for the next batch: the oldest id covered, exclusive.
    /// `None` when the corpus is covered (`done` is true).
    pub next_cursor: Option<Id>,
    /// Whether the corpus is fully covered.
    pub done: bool,
}

/// Index one bounded batch of the corpus into `index`.
///
/// `cursor` starts below the given id (exclusive, newest-first paging);
/// `None` starts at the newest row. Returns the progress, including the
/// cursor the next batch must resume from. Pure derived-state work: the store
/// is only read, and every index write is an idempotent upsert.
pub fn backfill_batch(
    store: &dyn Store,
    index: &EmbeddedIndex,
    cursor: Option<Id>,
    batch: usize,
) -> Result<BackfillProgress> {
    let batch = batch.clamp(1, BACKFILL_MAX_BATCH);
    let page = store.recent_statuses(None, cursor, None, batch)?;
    let mut indexed = 0usize;
    let mut oldest: Option<Id> = None;
    for status in &page {
        oldest = Some(status.id);
        if is_indexable(status) {
            index.index_status(status.id, &indexable_text(status));
            indexed += 1;
        } else {
            // Defensive: a row that stopped being indexable must not linger
            // when a backfill meets it (e.g. a tombstone filed between the
            // range read and this batch on a retried run).
            index.remove_status(status.id);
        }
    }
    let done = page.len() < batch;
    Ok(BackfillProgress {
        indexed,
        next_cursor: if done { None } else { oldest },
        done,
    })
}

#[cfg(test)]
mod tests {
    // Covered by the integration suite (`tests/phase7_2_search.rs`), which
    // exercises batching, resume and crash recovery against a real store.
}

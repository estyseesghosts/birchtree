//! Timeline marker persistence.
//!
//! One row per (account, timeline) with Mastodon's optimistic `lock_version`:
//! a stale update answers 409 through [`MarkerUpdateError::Stale`].

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{MarkerStore, MarkerUpdateError};
use crate::domain::{Id, Marker, Result};

use super::schema::MARKERS;
use super::{decode, encode, RedbStore, StoreResult};

/// Key for [`MARKERS`]: `account_be || 0xFF || timeline` (`0xFF` cannot
/// appear in a timeline name, so the account prefix scan stays exact).
fn marker_key(account_id: u64, timeline: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(9 + timeline.len());
    key.extend_from_slice(&account_id.to_be_bytes());
    key.push(0xFF);
    key.extend_from_slice(timeline.as_bytes());
    key
}

/// Read one marker inside an open write transaction.
pub(super) fn read_marker_txn(
    write: &WriteTransaction,
    account_id: Id,
    timeline: &str,
) -> StoreResult<Option<Marker>> {
    let table = write.open_table(MARKERS)?;
    match table.get(marker_key(account_id.value(), timeline).as_slice())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

impl MarkerStore for RedbStore {
    fn markers(&self, account_id: Id, timelines: &[&str]) -> Result<Vec<Marker>> {
        let read = self.begin_read()?;
        let table = read.open_table(MARKERS)?;
        let mut out = Vec::with_capacity(timelines.len());
        for timeline in timelines {
            if let Some(guard) = table.get(marker_key(account_id.value(), timeline).as_slice())? {
                out.push(decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn set_marker(
        &self,
        account_id: Id,
        timeline: &str,
        last_read_id: Id,
        lock_version: Option<u64>,
        now_ms: u64,
    ) -> std::result::Result<Marker, MarkerUpdateError> {
        if !Marker::timeline_valid(timeline) {
            return Err(MarkerUpdateError::InvalidTimeline);
        }
        let write = self
            .begin_write()
            .map_err(|_| MarkerUpdateError::InvalidTimeline)?;
        let existing = match read_marker_txn(&write, account_id, timeline) {
            Ok(existing) => existing,
            Err(_) => return Err(MarkerUpdateError::InvalidTimeline),
        };
        if let (Some(expected), Some(current)) = (lock_version, existing.as_ref()) {
            if expected != current.lock_version {
                return Err(MarkerUpdateError::Stale);
            }
        }
        let marker = Marker {
            timeline: timeline.to_string(),
            last_read_id,
            // Mastodon's find_or_create + update! leaves a fresh marker at
            // lock_version 1 (create=0, then one update bump).
            lock_version: existing.as_ref().map_or(1, |m| m.lock_version + 1),
            updated_at_ms: now_ms,
        };
        {
            let mut table = match write.open_table(MARKERS) {
                Ok(table) => table,
                Err(_) => return Err(MarkerUpdateError::InvalidTimeline),
            };
            if table
                .insert(
                    marker_key(account_id.value(), timeline).as_slice(),
                    encode(&marker).unwrap_or_default().as_slice(),
                )
                .is_err()
            {
                return Err(MarkerUpdateError::InvalidTimeline);
            }
        }
        if write.commit().is_err() {
            return Err(MarkerUpdateError::InvalidTimeline);
        }
        Ok(marker)
    }
}

//! Dismissed follow suggestions (fix round P1/S1).
//!
//! Upstream `follow_recommendation_mutes`: one row per (viewer, target)
//! dismissal, read back into the suggestion exclusion set. No counters or
//! jobs attach to the write; one `redb` transaction commits the row alone.

use redb::ReadableTable;

use crate::application::ports::SuggestionStore;
use crate::domain::{Id, Result};

use super::schema::{self, SUGGESTION_MUTES};
use super::RedbStore;

impl SuggestionStore for RedbStore {
    fn dismiss_suggestion(&self, viewer_id: Id, target_id: Id, now_ms: u64) -> Result<bool> {
        let write = self.begin_write()?;
        let mut table = write.open_table(SUGGESTION_MUTES)?;
        let key = schema::pair_key(viewer_id.value(), target_id.value());
        let fresh = table.get(key.as_slice())?.is_none();
        table.insert(key.as_slice(), now_ms)?;
        drop(table);
        write.commit()?;
        Ok(fresh)
    }

    fn dismissed_suggestions(&self, viewer_id: Id, cap: usize) -> Result<Vec<Id>> {
        let read = self.begin_read()?;
        let table = read.open_table(SUGGESTION_MUTES)?;
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&viewer_id.value().to_be_bytes());
        let mut out = Vec::new();
        for entry in table.range(prefix.as_slice()..)? {
            let (key, _) = entry?;
            let bytes = key.value();
            if bytes.len() != 16 || &bytes[..8] != prefix.as_slice() {
                break;
            }
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[8..16]);
            out.push(Id::new(u64::from_be_bytes(raw)));
            if out.len() >= cap {
                break;
            }
        }
        Ok(out)
    }
}

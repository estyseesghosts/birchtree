//! Place registry persistence (core extension, Stage 1 CORE-P).
//!
//! NEW FILE ONLY — not wired into `mod.rs` (see the proposed snippet in the
//! subagent report). Once wired, these helpers run *inside* the status
//! create/update transaction owned by `statuses.rs`, following the
//! `media.rs` `read_media_txn` / `write_media_txn` pattern: JSON-encoded
//! values (`encode` / `decode`), native ordered keys (`u64` id, `&str` name).
//!
//! Tables (schema v18, already opened by migrations):
//! - `PLACES` (`u64` place id → JSON `Place`).
//! - `PLACES_BY_NAME` (`&str` name → `u64` place id, unique).
//!
//! Assumes the proposed `crate::domain::place::Place` module wiring
//! (`pub mod place;` in `src/domain/mod.rs`).

use std::collections::HashMap;

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::PlaceStore;
use crate::domain::place::Place;
use crate::domain::{Id, Result};

use super::schema::{PLACES, PLACES_BY_NAME};
use super::{decode, encode, RedbStore, StoreResult};

/// Read a place row by id inside an open write transaction.
pub(super) fn read_place_txn(
    write: &WriteTransaction,
    id: Id,
) -> StoreResult<Option<Place>> {
    let table = write.open_table(PLACES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Read a place row by exact name inside an open write transaction.
pub(super) fn read_place_by_name_txn(
    write: &WriteTransaction,
    name: &str,
) -> StoreResult<Option<Place>> {
    let id = {
        let table = write.open_table(PLACES_BY_NAME)?;
        match table.get(name)? {
            Some(guard) => Id::new(guard.value()),
            None => return Ok(None),
        }
    };
    read_place_txn(write, id)
}

/// Write (upsert) a place row plus its unique name-index entry inside an open
/// write transaction.
///
/// The caller validates the place first (`Place::validate`). When renaming
/// (the same id under a new name), remove the stale name-index entry:
/// `write.open_table(PLACES_BY_NAME)?.remove(old_name)?`.
pub(super) fn insert_place_txn(write: &WriteTransaction, place: &Place) -> StoreResult<()> {
    {
        let mut table = write.open_table(PLACES)?;
        table.insert(place.id.value(), encode(place)?.as_slice())?;
    }
    {
        let mut by_name = write.open_table(PLACES_BY_NAME)?;
        by_name.insert(place.name.as_str(), place.id.value())?;
    }
    Ok(())
}

impl PlaceStore for RedbStore {
    fn upsert_place(&self, place: Place) -> Result<Place> {
        place.validate()?;
        let write = self.begin_write()?;
        // Renames must not orphan the old name-index entry: drop any other
        // name currently pointing at this id.
        let stale: Vec<String> = {
            let by_name = write.open_table(PLACES_BY_NAME)?;
            let mut stale = Vec::new();
            for entry in by_name.iter()? {
                let (name, id) = entry?;
                if id.value() == place.id.value() && name.value() != place.name {
                    stale.push(name.value().to_string());
                }
            }
            stale
        };
        insert_place_txn(&write, &place)?;
        if !stale.is_empty() {
            let mut by_name = write.open_table(PLACES_BY_NAME)?;
            for name in &stale {
                by_name.remove(name.as_str())?;
            }
        }
        write.commit()?;
        Ok(place)
    }

    fn place_by_id(&self, id: Id) -> Result<Option<Place>> {
        let write = self.begin_write()?;
        Ok(read_place_txn(&write, id)?)
    }

    fn places_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Place>> {
        let write = self.begin_write()?;
        let table = write.open_table(PLACES)?;
        let mut out = HashMap::with_capacity(ids.len());
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            if seen.insert(id.value()) {
                if let Some(guard) = table.get(id.value())? {
                    out.insert(*id, decode(guard.value())?);
                }
            }
        }
        Ok(out)
    }

    fn link_remote_place(&self, place: &Place) -> Result<Place> {
        place.validate()?;
        let write = self.begin_write()?;
        if let Some(existing) = read_place_by_name_txn(&write, &place.name)? {
            return Ok(existing);
        }
        let minted = Place { id: self.next_id(), ..place.clone() };
        insert_place_txn(&write, &minted)?;
        write.commit()?;
        Ok(minted)
    }
}


use std::collections::HashMap;

use crate::domain::{Id, Result};

/// Core extension CORE-P: the place registry (`places` / `places_by_name`,
/// schema v22 — renumbered from v18). Places are seeded by tests/operators
/// and remote ingest; there is no public proximity/discover/geo-search
/// surface (stub).
pub trait PlaceStore: Send + Sync {
    /// Validate and upsert a place row + its name-index entry in ONE
    /// transaction, returning the stored row.
    fn upsert_place(&self, place: crate::domain::Place) -> Result<crate::domain::Place>;
    /// Read a place row by id.
    fn place_by_id(&self, id: Id) -> Result<Option<crate::domain::Place>>;
    /// Places by id in ONE transaction (page loader; missing ids omitted).
    fn places_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, crate::domain::Place>>;
    /// Link a validated remote place: a same-name local row wins (returned
    /// as-is, so remote input can neither hijack nor duplicate local
    /// names); otherwise mint a local row in ONE transaction.
    fn link_remote_place(&self, place: &crate::domain::Place) -> Result<crate::domain::Place>;
}

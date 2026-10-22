use crate::domain::{Id, Marker, Result};

/// Why a marker update was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerUpdateError {
    /// Unknown or invalid timeline name.
    InvalidTimeline,
    /// `lock_version` did not match (Mastodon answers 409).
    Stale,
}

pub trait MarkerStore: Send + Sync {
    /// The account's markers for `timelines` (missing ones are omitted).
    fn markers(&self, account_id: Id, timelines: &[&str]) -> Result<Vec<Marker>>;
    /// Upsert a marker with optimistic locking: when `lock_version` is given
    /// it must match the stored version, else `Stale`.
    fn set_marker(
        &self,
        account_id: Id,
        timeline: &str,
        last_read_id: Id,
        lock_version: Option<u64>,
        now_ms: u64,
    ) -> std::result::Result<Marker, MarkerUpdateError>;
}

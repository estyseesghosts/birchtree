pub mod schema;
pub mod accounts;
pub mod statuses;
pub mod relationships;
pub mod quotes;
pub mod polls;
pub mod media;
pub mod notifications;
pub mod oauth;
pub mod jobs;
pub mod migrations;

/// Stub store (Phase 1 replaces with a real `RedbStore`).
pub struct RedbStore;

impl RedbStore {
    pub fn open(
        _path: std::path::PathBuf,
        _clock: std::sync::Arc<dyn crate::domain::Clock>,
        _ids: std::sync::Arc<dyn crate::domain::IdGenerator>,
    ) -> Result<Self, String> {
        Ok(Self)
    }
}

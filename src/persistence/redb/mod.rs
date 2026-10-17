//! `redb` storage adapter.
//!
//! One process-wide `redb::Database` behind the narrow ports in
//! `application::ports`. Values are JSON-encoded; keys are ordered primitives
//! so index scans stay bounded. No redb types escape this module.

pub mod schema;
pub mod migrations;
pub mod accounts;
pub mod oauth;
pub mod jobs;

// Planned storage modules (Phase 3+); still empty stubs.
pub mod statuses;
pub mod relationships;
pub mod quotes;
pub mod polls;
pub mod media;
pub mod notifications;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use redb::Database;
use redb::ReadableDatabase;

use crate::domain::{Clock, Id, IdGenerator};

/// Errors raised by the storage adapter.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("open database at {path}: {source}")]
    Open {
        path: PathBuf,
        source: redb::DatabaseError,
    },
    #[error("redb transaction: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("redb table: {0}")]
    Table(#[from] redb::TableError),
    #[error("redb storage: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("redb commit: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("encode/decode: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("migration: {0}")]
    Migration(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<StoreError> for crate::domain::Error {
    fn from(err: StoreError) -> Self {
        crate::domain::Error::Storage(err.to_string())
    }
}

// Allow the `?` operator to lift individual redb errors straight into the
// domain error type inside the port implementations.
macro_rules! domain_from_redb {
    ($($ty:ty),* $(,)?) => {
        $(
            impl From<$ty> for crate::domain::Error {
                fn from(err: $ty) -> Self {
                    crate::domain::Error::Storage(err.to_string())
                }
            }
        )*
    };
}

domain_from_redb!(
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
    redb::DatabaseError,
);

/// Convenience alias for storage results.
pub type StoreResult<T> = Result<T, StoreError>;

/// JSON-encode a record for storage.
pub(super) fn encode<T: serde::Serialize>(value: &T) -> StoreResult<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

/// JSON-decode a stored record.
pub(super) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> StoreResult<T> {
    Ok(serde_json::from_slice(bytes)?)
}

/// The process-wide storage handle.
pub struct RedbStore {
    db: Arc<Database>,
    path: PathBuf,
    /// Kept for record writes that stamp their own times in later phases.
    #[allow(dead_code)]
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl RedbStore {
    /// Open (creating if necessary) the database at `path` and run migrations.
    pub fn open(
        path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> StoreResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            prepare_data_dir(parent)?;
        }
        let db = Database::create(&path).map_err(|source| StoreError::Open {
            path: path.clone(),
            source,
        })?;
        migrations::apply(&db)?;
        Ok(Self {
            db: Arc::new(db),
            path,
            clock,
            ids,
        })
    }

    /// Filesystem path of the underlying database, for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn db(&self) -> &Database {
        &self.db
    }

    #[allow(dead_code)]
    pub(super) fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    pub(super) fn next_id(&self) -> Id {
        self.ids.generate()
    }
}

impl crate::application::ports::ReadyCheck for RedbStore {
    fn is_ready(&self) -> bool {
        self.db.begin_read().is_ok()
    }
}

/// Create the data directory if needed and check that it is not world-writable.
///
/// A world-writable directory lets another local user replace the database
/// file. We create new directories as `0700` on Unix and reject pre-existing
/// world-writable ones.
pub fn prepare_data_dir(dir: &Path) -> StoreResult<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(dir, perms)?;
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o002 != 0 {
            return Err(StoreError::Migration(format!(
                "data directory {} is world-writable (mode {:o}); refusing to open storage",
                dir.display(),
                mode & 0o777
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SystemClock, TimestampIdGenerator};

    #[test]
    fn opens_and_migrates_fresh_database() {
        let dir = tempfile::tempdir().unwrap();
        let store = RedbStore::open(
            dir.path().join("birchtree.redb"),
            Arc::new(SystemClock),
            Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
        )
        .unwrap();
        assert!(store.path().exists());
    }

    #[test]
    fn rejects_world_writable_data_dir() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let nested = dir.path().join("data");
            std::fs::create_dir(&nested).unwrap();
            std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o707)).unwrap();
            let err = prepare_data_dir(&nested).unwrap_err();
            assert!(matches!(err, StoreError::Migration(_)));
        }
    }
}

//! Versioned schema migration runner.
//!
//! Migrations are applied once, in order, recorded in [`schema::META`], and are
//! idempotent/restartable: opening every table in a write transaction creates
//! any missing table, so re-running a step is harmless. The runner refuses to
//! open a database written by a newer build.

use redb::{Database, ReadableDatabase};

use super::schema::{
    self, ACCESS_TOKENS, ACCOUNTS, AUTHORIZATION_CODES, HANDLE_INDEX, JOBS, JOBS_INDEX, LOCAL_USERS,
    META, OAUTH_APPS, SESSIONS,
};
use super::StoreError;

/// Desired schema version for this build.
pub fn target_version() -> u64 {
    schema::SCHEMA_VERSION
}

/// Read the applied schema version, or `0` for a fresh database.
pub fn applied_version(db: &Database) -> Result<u64, StoreError> {
    let read = db.begin_read()?;
    let table = match read.open_table(META) {
        Ok(table) => table,
        // A brand-new database has no `meta` table yet.
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(err) => return Err(err.into()),
    };
    match table.get(schema::SCHEMA_VERSION_KEY)? {
        Some(value) => {
            let bytes = value.value();
            let arr: [u8; 8] = bytes
                .try_into()
                .map_err(|_| StoreError::Migration("corrupt schema_version record".into()))?;
            Ok(u64::from_be_bytes(arr))
        }
        None => Ok(0),
    }
}

/// Apply pending migrations. Idempotent and safe to call on every open.
pub fn apply(db: &Database) -> Result<u64, StoreError> {
    let current = applied_version(db)?;
    let target = target_version();

    if current > target {
        return Err(StoreError::Migration(format!(
            "database schema v{current} is newer than this build's v{target}; upgrade the server"
        )));
    }
    if current == target {
        return Ok(current);
    }

    let write = db.begin_write()?;
    {
        // Step v1: create every table this build knows about. Opening a table
        // in a write transaction creates it; re-running is idempotent.
        if current < 1 {
            let _ = write.open_table(META)?;
            let _ = write.open_table(ACCOUNTS)?;
            let _ = write.open_table(HANDLE_INDEX)?;
            let _ = write.open_table(LOCAL_USERS)?;
            let _ = write.open_table(OAUTH_APPS)?;
            let _ = write.open_table(ACCESS_TOKENS)?;
            let _ = write.open_table(SESSIONS)?;
            let _ = write.open_table(JOBS)?;
            let _ = write.open_table(JOBS_INDEX)?;
        }

        // Step v2: authorization codes for the authorization-code + PKCE grant.
        if current < 2 {
            let _ = write.open_table(AUTHORIZATION_CODES)?;
        }

        {
            let mut meta = write.open_table(META)?;
            meta.insert(
                schema::SCHEMA_VERSION_KEY,
                target.to_be_bytes().as_slice(),
            )?;
        }
    }
    write.commit()?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::create(dir.path().join("test.redb")).unwrap();
        (dir, db)
    }

    #[test]
    fn fresh_database_migrates_to_target() {
        let (_dir, db) = temp_db();
        assert_eq!(applied_version(&db).unwrap(), 0);
        assert_eq!(apply(&db).unwrap(), target_version());
        assert_eq!(applied_version(&db).unwrap(), target_version());
    }

    #[test]
    fn migration_is_idempotent_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        {
            let db = Database::create(&path).unwrap();
            apply(&db).unwrap();
        }
        {
            let db = Database::create(&path).unwrap();
            // Re-applying is a no-op returning the current version.
            assert_eq!(apply(&db).unwrap(), target_version());
        }
    }

    #[test]
    fn refuses_newer_schema() {
        let (_dir, db) = temp_db();
        apply(&db).unwrap();
        // Stamp a future version and ensure the runner refuses it.
        let write = db.begin_write().unwrap();
        {
            let mut meta = write.open_table(META).unwrap();
            meta.insert(
                schema::SCHEMA_VERSION_KEY,
                (target_version() + 1).to_be_bytes().as_slice(),
            )
            .unwrap();
        }
        write.commit().unwrap();
        assert!(matches!(apply(&db), Err(StoreError::Migration(_))));
    }
}

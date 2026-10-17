//! `redb` table definitions.
//!
//! Tables are addressed by these `const` definitions; no application code opens
//! them directly. Values are JSON-encoded bytes so a record can evolve without
//! a key-layout migration; keys are the native ordered primitive so range
//! scans stay bounded and index-ordered.

use redb::TableDefinition;

/// Schema/migration metadata (key -> value bytes).
pub const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// Accounts by numeric ID.
pub const ACCOUNTS: TableDefinition<u64, &[u8]> = TableDefinition::new("accounts");

/// Unique handle index: normalized `user@domain` -> account ID.
pub const HANDLE_INDEX: TableDefinition<&str, u64> = TableDefinition::new("handle_index");

/// Local credentials by account ID.
pub const LOCAL_USERS: TableDefinition<u64, &[u8]> = TableDefinition::new("local_users");

/// OAuth applications by client ID.
pub const OAUTH_APPS: TableDefinition<&str, &[u8]> = TableDefinition::new("oauth_apps");

/// Access tokens by hex-encoded SHA-256 token hash.
pub const ACCESS_TOKENS: TableDefinition<&str, &[u8]> = TableDefinition::new("access_tokens");

/// OAuth authorization codes by hex-encoded SHA-256 code hash (schema v2).
pub const AUTHORIZATION_CODES: TableDefinition<&str, &[u8]> =
    TableDefinition::new("authorization_codes");

/// Login sessions by hex-encoded SHA-256 session-token hash.
pub const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");

/// Durable jobs by numeric job ID.
pub const JOBS: TableDefinition<u64, &[u8]> = TableDefinition::new("jobs");

/// Job scheduling index: fixed 16-byte `(due_ms_be || job_id_be)` -> job ID.
///
/// For a pending job the leading timestamp is its due time; for a leased job it
/// is the lease expiry, so an abandoned lease becomes claimable again without a
/// recovery scan.
pub const JOBS_INDEX: TableDefinition<&[u8], u64> = TableDefinition::new("jobs_index");

/// Key under `META` holding the applied schema version (big-endian `u64`).
pub const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Current schema version. Bump with a new migration step.
pub const SCHEMA_VERSION: u64 = 2;

/// All table names, for diagnostics and migration verification.
pub const TABLE_NAMES: &[&str] = &[
    "meta",
    "accounts",
    "handle_index",
    "local_users",
    "oauth_apps",
    "access_tokens",
    "authorization_codes",
    "sessions",
    "jobs",
    "jobs_index",
];

/// Build the fixed 16-byte scheduling key used by [`JOBS_INDEX`].
pub fn job_index_key(timestamp_ms: u64, job_id: u64) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&timestamp_ms.to_be_bytes());
    key[8..].copy_from_slice(&job_id.to_be_bytes());
    key
}

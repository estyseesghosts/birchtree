//! Shared helpers for integration tests.
#![allow(dead_code)]

use std::sync::Arc;

use birchtree::application::accounts::{create_account, CreateAccountCommand};
use birchtree::domain::{Account, SystemClock, TimestampIdGenerator};
use birchtree::persistence::redb::RedbStore;

/// Open a fresh store in a temporary directory. The `TempDir` must be kept
/// alive for the duration of the test.
pub fn temp_store() -> (tempfile::TempDir, Arc<RedbStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    (dir, Arc::new(store))
}

/// Open a store at an arbitrary directory (used to reopen after a restart).
pub fn open_store(dir: &std::path::Path) -> RedbStore {
    RedbStore::open(
        dir.join("birchtree.redb"),
        Arc::new(SystemClock),
        Arc::new(TimestampIdGenerator::new(Box::new(SystemClock))),
    )
    .expect("open store")
}

/// A fixed timestamp used across tests for determinism.
pub const T0: u64 = 1_700_000_000_000;

/// Create a local account through the application use case.
pub fn mk_account(store: &RedbStore, username: &str) -> Account {
    create_account(
        store,
        "https://example.social",
        T0,
        CreateAccountCommand {
            username: username.to_string(),
            email: None,
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create account")
}

//! Schema v24 probe: per-destination delivery sequence counters plus the
//! transactional delivery index.
//!
//! `SCHEMA_VERSION == 24` with `delivery_seq` and `delivery_active`
//! registered in `TABLE_NAMES`; fresh databases migrate to the build target.

use birchtree::persistence::redb::{migrations, schema};

#[test]
fn fresh_database_migrates_to_target() {
    let dir = tempfile::tempdir().expect("tempdir for probe db");
    let db = redb::Database::create(dir.path().join("probe.redb")).expect("create probe db");
    let applied = migrations::apply(&db).expect("apply migrations");
    assert_eq!(
        applied,
        migrations::target_version(),
        "apply returns the build target"
    );
    assert_eq!(
        applied,
        schema::SCHEMA_VERSION,
        "target tracks SCHEMA_VERSION"
    );
}

#[test]
fn delivery_seq_table_registered_at_v23() {
    assert_eq!(schema::SCHEMA_VERSION, 24, "delivery index lands in v24");
    assert!(
        schema::TABLE_NAMES.contains(&"delivery_seq"),
        "TABLE_NAMES registers delivery_seq"
    );
    assert!(
        schema::TABLE_NAMES.contains(&"delivery_active"),
        "TABLE_NAMES registers delivery_active"
    );
}

//! Schema v22 probe (subagent slice, read-only against shared files).
//!
//! Passes on master (v21) and validates the Stage 1 CORE-P rebase once the
//! parent merges the snippets: `SCHEMA_VERSION == 22` with `places` and
//! `places_by_name` registered in `TABLE_NAMES`.

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
fn places_tables_registered_when_v22() {
    if schema::SCHEMA_VERSION < 22 {
        eprintln!(
            "skip: SCHEMA_VERSION is {} (pre-merge master); parent merge raises it to 22",
            schema::SCHEMA_VERSION
        );
        return;
    }
    assert_eq!(schema::SCHEMA_VERSION, 22, "rebase pins schema to v22");
    assert!(
        schema::TABLE_NAMES.contains(&"places"),
        "TABLE_NAMES registers places"
    );
    assert!(
        schema::TABLE_NAMES.contains(&"places_by_name"),
        "TABLE_NAMES registers places_by_name"
    );
}

//! Aperture mode config plumbing (Stage 2 AP-A1): default, file overlay,
//! schema parity, and `ServerInfo` carry-through. Uses the already-wired
//! `birchtree::config::Config` directly; asserts no interplay with the three
//! Stage 1 core-extension policies.

use std::io::Write;

use birchtree::api::mastodon::ServerInfo;
use birchtree::config::Config;

fn write_file(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

#[test]
fn default_is_off_mastodon_parity() {
    assert!(!Config::default().aperture_mode);
}

#[test]
fn file_overlay_enables_aperture() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "aperture.json", r#"{"aperture_mode": true}"#);
    let cfg = Config::load_from_file(&path).expect("loads");
    assert!(cfg.aperture_mode);
    assert!(cfg.validate().is_ok());
}

#[test]
fn no_policy_interplay() {
    // Enabling aperture mode touches nothing about the three core-extension
    // policies: they stay at their compiled defaults and the merged config
    // still validates.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "aperture.json", r#"{"aperture_mode": true}"#);
    let cfg = Config::load_from_file(&path).expect("loads");
    assert_eq!(cfg.license_policy, "off");
    assert_eq!(cfg.place_policy, "off");
    assert_eq!(cfg.alt_text_policy, "off");
    assert!(cfg.validate().is_ok());
    // And the reverse: core policies can move while aperture stays default.
    let path = write_file(
        &dir,
        "core.json",
        r#"{"license_policy": "enforced", "place_policy": "unenforced", "alt_text_policy": "block"}"#,
    );
    let cfg = Config::load_from_file(&path).expect("loads");
    assert!(!cfg.aperture_mode);
    assert!(cfg.validate().is_ok());
}

#[test]
fn schema_properties_include_aperture_mode() {
    // Mirror of `config_file.rs::schema_properties_match_serialized_default_keys`,
    // scoped to the new key plus the no-drift pin in both directions.
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../docs/config-schema.json")).expect("schema parses");
    let props = schema["properties"]
        .as_object()
        .expect("schema has properties");
    assert!(
        props.contains_key("aperture_mode"),
        "schema missing aperture_mode"
    );
    let default_json = serde_json::to_value(Config::default()).expect("default serializes");
    let keys = default_json.as_object().expect("default is an object");
    assert!(
        keys.contains_key("aperture_mode"),
        "Config missing aperture_mode"
    );
    assert_eq!(default_json["aperture_mode"], false);
    for key in props.keys() {
        assert!(keys.contains_key(key), "schema key {key} missing from Config");
    }
    for key in keys.keys() {
        assert!(props.contains_key(key), "Config key {key} missing from schema");
    }
}

#[test]
fn server_info_carries_aperture_mode() {
    let off = Config::default();
    assert!(!ServerInfo::from_config(&off).aperture_mode);
    let on = Config {
        aperture_mode: true,
        ..Config::default()
    };
    assert!(ServerInfo::from_config(&on).aperture_mode);
}

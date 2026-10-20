//! JSON config file: overlay, rejection, env indirection, schema parity.

use std::io::Write;

use birchtree::config::Config;

fn write_file(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

#[test]
fn partial_file_overlays_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(
        &dir,
        "base.json",
        r#"{"public_url": "https://example.social", "max_status_chars": 1000,
            "federation_allowlist": ["10.0.0.0/8", "fd00::1"],
            "search_mode": "database", "worker_poll_interval_ms": 500}"#,
    );
    let cfg = Config::load_from_file(&path).expect("loads");
    assert_eq!(cfg.public_url, "https://example.social");
    assert_eq!(cfg.max_status_chars, 1000);
    assert_eq!(cfg.search_mode, birchtree::search::SearchMode::Database);
    assert_eq!(cfg.worker_poll_interval, std::time::Duration::from_millis(500));
    assert_eq!(cfg.federation_allowlist.len(), 2);
    // Untouched keys keep compiled defaults.
    assert_eq!(cfg.listen.to_string(), "0.0.0.0:3000");
    assert!(cfg.validate().is_ok());
}

#[test]
fn unknown_keys_are_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "typo.json", r#"{"pubic_url": "https://example.social"}"#);
    let err = Config::load_from_file(&path).expect_err("typo must fail");
    assert!(err.contains("pubic_url"), "{err}");
}

#[test]
fn malformed_values_fail() {
    let dir = tempfile::tempdir().expect("tempdir");
    // search_mode "solr" and a malformed CIDR fail at parse; webp_quality
    // 101 parses but fails validate.
    let path = write_file(&dir, "mode.json", r#"{"search_mode": "solr"}"#);
    assert!(Config::load_from_file(&path).is_err());
    let path = write_file(&dir, "cidr.json", r#"{"federation_allowlist": ["999.1.1.1/8"]}"#);
    assert!(Config::load_from_file(&path).is_err());
    let path = write_file(&dir, "quality.json", r#"{"webp_quality": 101}"#);
    let cfg = Config::load_from_file(&path).expect("parses");
    assert!(cfg.validate().is_err(), "webp_quality 101 must not validate");
}

#[test]
fn missing_file_is_a_named_error() {
    let err = Config::load_from_file(std::path::Path::new("/nonexistent/birchtree-x.json"))
        .expect_err("must fail");
    assert!(err.contains("birchtree-x.json"), "{err}");
}

#[test]
fn env_indirection_resolves_and_missing_fails() {
    let mut cfg = Config {
        smtp_url: Some("env:BIRCHTREE_TEST_SMTP_URL".into()),
        mail_from: Some("noreply@example.social".into()),
        ..Config::default()
    };
    assert!(cfg.resolve_env_indirect().is_err());
    // SAFETY: single-threaded test, unique var name.
    unsafe {
        std::env::set_var("BIRCHTREE_TEST_SMTP_URL", "smtp://mail.example.com:587");
    }
    assert!(cfg.resolve_env_indirect().is_ok());
    assert_eq!(cfg.smtp_url.as_deref(), Some("smtp://mail.example.com:587"));
    assert!(cfg.validate().is_ok());
    unsafe {
        std::env::remove_var("BIRCHTREE_TEST_SMTP_URL");
    }
}

#[test]
fn schema_properties_match_serialized_default_keys() {
    // docs/config-schema.json is hand-maintained; this test pins it to the
    // struct so the two cannot drift: every schema property must exist in a
    // serialized default Config and vice versa.
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../docs/config-schema.json")).expect("schema parses");
    let props = schema["properties"]
        .as_object()
        .expect("schema has properties");
    let default_json =
        serde_json::to_value(Config::default()).expect("default serializes");
    let keys = default_json.as_object().expect("default is an object");
    for key in props.keys() {
        assert!(keys.contains_key(key), "schema key {key} missing from Config");
    }
    for key in keys.keys() {
        assert!(props.contains_key(key), "Config key {key} missing from schema");
    }
    // Spot-check units: durations serialize as millis numbers.
    assert_eq!(default_json["worker_poll_interval_ms"], 250);
}

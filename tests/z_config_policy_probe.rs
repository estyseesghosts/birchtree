//! Config probe: core-extension policy keys pre-merge.
//!
//! Pre-merge the three Stage 1 policy keys (`license_policy`,
//! `place_policy`, `alt_text_policy`) do not exist on `Config`, so a file
//! carrying them is rejected by `deny_unknown_fields`. After the parent
//! applies snippet (a) this file's second test must be flipped to expect
//! `Ok` plus field defaults of `"off"` and fail-closed validation
//! (`off`/`unenforced`/`enforced`, `off`/`nag`/`block`).

use std::io::Write;

use birchtree::config::Config;

fn write_file(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

#[test]
fn probe_hermetic_default_validates() {
    let cfg = Config {
        transcoding_enabled: false,
        ..Config::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn probe_policy_keys_present_post_merge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(
        &dir,
        "policies.json",
        r#"{"license_policy": "off", "place_policy": "enforced", "alt_text_policy": "nag"}"#,
    );
    let cfg = Config::load_from_file(&path).expect("policy keys merge post-rebase");
    assert_eq!(cfg.license_policy, "off");
    assert_eq!(cfg.place_policy, "enforced");
    assert_eq!(cfg.alt_text_policy, "nag");
    assert!(cfg.validate().is_ok());
    // Fail-closed validation: out-of-enum values are rejected by validate().
    let bad = write_file(
        &dir,
        "bad.json",
        r#"{"license_policy": "sometimes", "place_policy": "off", "alt_text_policy": "off"}"#,
    );
    let bad_cfg = Config::load_from_file(&bad).expect("parses as string");
    assert_eq!(bad_cfg.license_policy, "sometimes");
    assert!(bad_cfg.validate().is_err());
}

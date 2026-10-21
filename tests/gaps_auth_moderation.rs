//! G-E: auth + moderation hardening.
//!
//! Covers: TOTP enroll/confirm/verify + backup-code single-use (with the RFC
//! 6238 vector pinning the HMAC-SHA1 core), WebAuthn ES256
//! challenge/response round-trips (registration `none` + `packed`
//! self-attestation, authentication, sign-count clone detection, and every
//! refusal shape), min-age + time-zone gates, the IP-block true-client
//! matrix across proxy shapes, the email-domain/canonical-email block matrix
//! reconciled with confirmation redemption, the D35 per-action flag matrix,
//! and numeric role ids with the real upstream bitmask.
//!
//! Secrecy: no test logs a TOTP seed, backup-code plaintext (beyond holding
//! it in a local to redeem it), credential id, or password hash. Debug
//! redaction is asserted, not assumed.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;

use birchtree::api::mastodon::auth::{AdminArea, Authenticated};
use birchtree::application::accounts::{
    MemorySecondFactorStore, SecondFactorStore,
};
use birchtree::application::accounts::{
    EmailPolicyOutcome, LoginOutcome, RegistrationProfile, SecondFactorKind,
    begin_webauthn_authentication, begin_webauthn_registration, check_registration_email,
    confirm_email, confirm_totp, create_account, enroll_totp, finish_webauthn_authentication,
    finish_webauthn_registration, login_with_mfa, register_account_with_profile,
    verify_second_factor, CreateAccountCommand, RegisterAccountCommand, INVALID_2FA_MESSAGE,
};
use birchtree::application::admin_infra as infra_uc;
use birchtree::application::auth as auth_uc;
use birchtree::application::ports::{AccountStore, AdminInfraStore};
use birchtree::domain::user::{
    AdminAction, TotpRecord, WebauthnChallenge, WebauthnPurpose, base32_decode, base32_encode,
    check_date_of_birth, totp_code_at, totp_counter_at, validate_time_zone, verify_totp_at,
    ADMIN_PERMISSIONS, MODERATOR_PERMISSIONS, PERM_MANAGE_BLOCKS, PERM_MANAGE_FEDERATION,
    PERM_MANAGE_REPORTS, PERM_MANAGE_TAXONOMIES, PERM_MANAGE_USERS, PERM_VIEW_AUDIT_LOG,
    PERM_VIEW_DASHBOARD, PERM_VIEW_FEEDS,
};
use birchtree::domain::{EmailToken, EmailTokenKind, Error, Id, OAuthApplication, UserRole};

use common::{T0, TestServer};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn open_store() -> (tempfile::TempDir, birchtree::persistence::redb::RedbStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = birchtree::persistence::redb::RedbStore::open(
        dir.path().join("birchtree.redb"),
        Arc::new(birchtree::domain::SystemClock),
        Arc::new(birchtree::domain::TimestampIdGenerator::new(Box::new(
            birchtree::domain::SystemClock,
        ))),
    )
    .expect("open store");
    (dir, store)
}

fn test_app(id: u64) -> OAuthApplication {
    OAuthApplication {
        id: Id::new(id),
        name: "g-e test app".to_string(),
        client_id: format!("ge-client-{id}"),
        client_secret_hash: None,
        redirect_uris: vec!["urn:ietf:wg:oauth:2.0:oob".to_string()],
        scopes: vec!["read".to_string(), "write".to_string()],
        website: None,
        created_at_ms: T0,
    }
}

fn reg_cmd(username: &str, email: &str) -> RegisterAccountCommand {
    RegisterAccountCommand {
        username: username.to_string(),
        email: email.to_string(),
        password: "password123".to_string(),
        agreement: true,
        locale: None,
        reason: None,
        invite_code: None,
        display_name: None,
        time_zone: None,
        date_of_birth: None,
    }
}

fn cli_account(
    store: &birchtree::persistence::redb::RedbStore,
    username: &str,
    email: Option<&str>,
) -> birchtree::domain::Account {
    create_account(
        store,
        "https://example.social",
        T0,
        CreateAccountCommand {
            username: username.to_string(),
            email: email.map(str::to_string),
            password: "password123".to_string(),
            display_name: None,
        },
    )
    .expect("create account")
}

// ---------------------------------------------------------------------------
// TOTP
// ---------------------------------------------------------------------------

#[test]
fn totp_rfc6238_sha1_vectors() {
    // RFC 6238 Appendix B, SHA1 family: the secret is the ASCII string
    // "12345678901234567890".
    let secret = b"12345678901234567890";
    assert_eq!(totp_code_at(secret, 1), "287082"); // T = 59
    assert_eq!(totp_code_at(secret, 37037036), "081804"); // T = 1111111109
    assert_eq!(totp_code_at(secret, 66666666), "279037"); // T = 2000000000
    assert_eq!(totp_code_at(secret, 66666666).len(), 6);
}

#[test]
fn totp_base32_round_trip_and_known_vector() {
    assert_eq!(base32_decode("MY======").expect("decode"), b"f");
    assert_eq!(base32_decode("my").expect("lowercase"), b"f");
    let raw: Vec<u8> = (0u8..=255).collect();
    assert_eq!(base32_decode(&base32_encode(&raw)).expect("round-trip"), raw);
    assert!(base32_decode("!!!!").is_err());
    assert!(base32_decode("A").is_err());
}

#[test]
fn totp_enroll_confirm_verify_backup_single_use() {
    let (_dir, store) = open_store();
    let mfa = MemorySecondFactorStore::new();
    let account = cli_account(&store, "totp-alice", Some("totp-alice@example.com"));
    let now_ms = 1_700_000_001_000u64;

    // Enroll: URI shape + 32-char secret (20 bytes, no padding).
    let enrollment = enroll_totp(&mfa, "totp-alice", "example.social", account.id, now_ms)
        .expect("enroll");
    assert!(enrollment.otpauth_uri.starts_with("otpauth://totp/example.social:totp-alice?"));
    assert!(enrollment.otpauth_uri.contains("algorithm=SHA1&digits=6&period=30"));
    assert_eq!(enrollment.secret_base32.len(), 32);
    // Second enrollment without revoke is refused (never two live secrets).
    assert!(matches!(
        enroll_totp(&mfa, "totp-alice", "example.social", account.id, now_ms),
        Err(Error::Conflict(_))
    ));
    // Unconfirmed enrollments never authenticate.
    assert!(matches!(
        verify_second_factor(&mfa, account.id, "000000", now_ms),
        Err(Error::Unauthorized(_))
    ));

    // Wrong code cannot confirm.
    assert!(confirm_totp(&mfa, account.id, "000000", now_ms).is_err());
    // Live code confirms and mints 10 backup codes (shown once).
    let secret = base32_decode(&enrollment.secret_base32).expect("secret decodes");
    let code = totp_code_at(&secret, totp_counter_at(now_ms));
    let backup_codes = confirm_totp(&mfa, account.id, &code, now_ms).expect("confirm");
    assert_eq!(backup_codes.len(), 10);
    for backup in &backup_codes {
        assert_eq!(backup.len(), 16);
        assert!(backup.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    // Re-confirm is refused.
    assert!(confirm_totp(&mfa, account.id, &code, now_ms).is_err());

    // Replay of the confirmation code is refused (counter persisted).
    assert!(matches!(
        verify_second_factor(&mfa, account.id, &code, now_ms),
        Err(Error::Unauthorized(message)) if message == INVALID_2FA_MESSAGE
    ));
    // A code from outside the ±1 window is refused.
    let far_future = totp_code_at(&secret, totp_counter_at(now_ms) + 5);
    assert!(verify_second_factor(&mfa, account.id, &far_future, now_ms).is_err());
    // A fresh code verifies.
    let later_ms = (totp_counter_at(now_ms) + 2) * 30 * 1000;
    let fresh = totp_code_at(&secret, totp_counter_at(later_ms));
    assert_eq!(
        verify_second_factor(&mfa, account.id, &fresh, later_ms).expect("fresh totp"),
        SecondFactorKind::Totp
    );

    // Backup codes: first use ok, second use refused (single-use).
    assert_eq!(
        verify_second_factor(&mfa, account.id, &backup_codes[0], later_ms).expect("backup"),
        SecondFactorKind::BackupCode
    );
    assert!(matches!(
        verify_second_factor(&mfa, account.id, &backup_codes[0], later_ms),
        Err(Error::Unauthorized(message)) if message == INVALID_2FA_MESSAGE
    ));
    // Unknown codes and unknown enrollments share the one refusal shape.
    assert!(matches!(
        verify_second_factor(&mfa, account.id, "deadbeefdeadbeef", later_ms),
        Err(Error::Unauthorized(message)) if message == INVALID_2FA_MESSAGE
    ));
    assert!(matches!(
        verify_second_factor(&mfa, Id::new(424242), "123456", later_ms),
        Err(Error::Unauthorized(message)) if message == INVALID_2FA_MESSAGE
    ));

    // At rest: hashes only. The spent code is gone; nine digests remain; no
    // plaintext backup code appears in the stored JSON.
    let record = mfa.totp_for(account.id).expect("read").expect("row");
    assert_eq!(record.backup_hashes.len(), 9);
    let stored_json = serde_json::to_string(&record).expect("json");
    for spent in [&backup_codes[0]] {
        assert!(!stored_json.contains(spent), "spent plaintext must not persist");
    }
    // The live secret must persist (verification needs it) inside the 0700
    // store file — but it never appears in Debug output.
    assert!(stored_json.contains(&enrollment.secret_base32));
    let debug = format!("{record:?}");
    assert!(!debug.contains(&enrollment.secret_base32), "secret leaked to Debug");
    assert!(!debug.contains(&backup_codes[1]), "backup plaintext leaked to Debug");
    assert!(debug.contains("[redacted]"));
}

#[test]
fn totp_window_accepts_adjacent_counters() {
    // ±1 step skew: the previous and next codes verify at the same instant.
    // (Counter 0 can never verify past a `last_counter` of 0 — replay
    // protection — so the window is probed mid-stream.)
    let secret = b"12345678901234567890";
    let now_ms = 150_000; // counter 5
    assert_eq!(totp_counter_at(now_ms), 5);
    assert_eq!(verify_totp_at(secret, &totp_code_at(secret, 5), now_ms, 3), Some(5));
    let prev = totp_code_at(secret, 4);
    let next = totp_code_at(secret, 6);
    assert_eq!(verify_totp_at(secret, &prev, now_ms, 3), Some(4));
    assert_eq!(verify_totp_at(secret, &next, now_ms, 3), Some(6));
    // A counter at or below the replay floor stays refused, window or not.
    assert_eq!(verify_totp_at(secret, &prev, now_ms, 4), None);
    // Malformed codes never verify (and never panic).
    assert_eq!(verify_totp_at(secret, "12345", now_ms, 0), None);
    assert_eq!(verify_totp_at(secret, "1234567", now_ms, 0), None);
    assert_eq!(verify_totp_at(secret, "abcdef", now_ms, 0), None);
    assert_eq!(verify_totp_at(secret, "", now_ms, 0), None);
}

// ---------------------------------------------------------------------------
// WebAuthn (ES256, real signatures via ring)
// ---------------------------------------------------------------------------

/// Minimal definite-length CBOR encoder for building ceremony fixtures.
mod cbor_enc {
    pub fn head(major: u8, n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        if n < 24 {
            out.push(major << 5 | n as u8);
        } else if n <= u8::MAX as u64 {
            out.push(major << 5 | 24);
            out.push(n as u8);
        } else if n <= u16::MAX as u64 {
            out.push(major << 5 | 25);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            out.push(major << 5 | 26);
            out.extend_from_slice(&(n as u32).to_be_bytes());
        }
        out
    }

    pub fn uint(n: u64) -> Vec<u8> {
        head(0, n)
    }

    pub fn nint(n: i64) -> Vec<u8> {
        assert!(n < 0);
        head(1, (-1 - n) as u64)
    }

    pub fn bstr(bytes: &[u8]) -> Vec<u8> {
        let mut out = head(2, bytes.len() as u64);
        out.extend_from_slice(bytes);
        out
    }

    pub fn tstr(text: &str) -> Vec<u8> {
        let mut out = head(3, text.len() as u64);
        out.extend_from_slice(text.as_bytes());
        out
    }

    pub fn array(items: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = head(4, items.len() as u64);
        for item in items {
            out.extend_from_slice(&item);
        }
        out
    }

    pub fn map(pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
        let mut out = head(5, pairs.len() as u64);
        for (key, value) in pairs {
            out.extend_from_slice(&key);
            out.extend_from_slice(&value);
        }
        out
    }
}

use cbor_enc as enc;

const RP_ID: &str = "example.social";
const ORIGIN: &str = "https://example.social";

/// Generate a P-256 pair; returns the uncompressed public point + PKCS#8.
fn p256_pair() -> (Vec<u8>, Vec<u8>) {
    let rng = ring::rand::SystemRandom::new();
    let doc = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .expect("generate");
    let der = doc.as_ref();
    let pub_off = der
        .windows(4)
        .position(|w| *w == [0x03, 0x42, 0x00, 0x04])
        .expect("pub marker");
    let pub_raw = der[pub_off + 3..pub_off + 3 + 65].to_vec();
    assert_eq!(pub_raw[0], 0x04);
    (pub_raw, der.to_vec())
}

fn p256_sign(pkcs8: &[u8], message: &[u8]) -> Vec<u8> {
    let rng = ring::rand::SystemRandom::new();
    let pair = ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        pkcs8,
        &rng,
    )
    .expect("parse");
    pair.sign(&rng, message).expect("sign").as_ref().to_vec()
}

fn sha256_bytes(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, data).as_ref().to_vec()
}

/// COSE_Key for ES256: `{1: 2, 3: -7, -1: 1, -2: x, -3: y}`.
fn cose_es256(pub_raw: &[u8]) -> Vec<u8> {
    enc::map(vec![
        (enc::uint(1), enc::uint(2)),
        (enc::uint(3), enc::nint(-7)),
        (enc::nint(-1), enc::uint(1)),
        (enc::nint(-2), enc::bstr(&pub_raw[1..33])),
        (enc::nint(-3), enc::bstr(&pub_raw[33..65])),
    ])
}

fn registration_auth_data(
    rp_id: &str,
    cred_id: &[u8],
    cose: &[u8],
    sign_count: u32,
    flags: u8,
) -> Vec<u8> {
    let mut out = sha256_bytes(rp_id.as_bytes());
    out.push(flags);
    out.extend_from_slice(&sign_count.to_be_bytes());
    out.extend_from_slice(&[0u8; 16]); // AAGUID
    out.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
    out.extend_from_slice(cred_id);
    out.extend_from_slice(cose);
    out
}

fn client_data_json(kind: &str, challenge_b64: &str, origin: &str) -> Vec<u8> {
    serde_json::json!({
        "type": kind,
        "challenge": challenge_b64,
        "origin": origin,
    })
    .to_string()
    .into_bytes()
}

fn attestation_object(fmt: &str, auth_data: &[u8], att_stmt: Vec<u8>) -> Vec<u8> {
    enc::map(vec![
        (enc::tstr("fmt"), enc::tstr(fmt)),
        (enc::tstr("authData"), enc::bstr(auth_data)),
        (enc::tstr("attStmt"), att_stmt),
    ])
}

#[test]
fn webauthn_none_attestation_round_trip() {
    let mfa = MemorySecondFactorStore::new();
    let account_id = Id::new(7001);
    let (pub_raw, _priv) = p256_pair();
    let cred_id = b"test-credential-id-001".to_vec();

    let challenge =
        begin_webauthn_registration(&mfa, account_id, T0).expect("begin registration");
    assert_eq!(challenge.challenge_b64.len(), 43); // 32 bytes, no pad
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let auth_data = registration_auth_data(RP_ID, &cred_id, &cose_es256(&pub_raw), 0, 0x41);
    let att_obj = attestation_object("none", &auth_data, enc::map(vec![]));

    let credential = finish_webauthn_registration(
        &mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_obj,
    )
    .expect("finish registration");
    assert_eq!(credential.account_id, account_id);
    assert_eq!(credential.credential_id, cred_id);
    assert_eq!(credential.public_key_uncompressed, pub_raw);
    assert_eq!(mfa.webauthn_for_account(account_id).expect("list").len(), 1);

    // The challenge is single-use: finishing twice is refused.
    let again = finish_webauthn_registration(
        &mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_obj,
    );
    assert!(
        matches!(again, Err(Error::InvalidInput(ref message)) if message.contains("already used")),
        "got {again:?}"
    );

    // The same credential id cannot register twice (other account or same).
    let challenge2 =
        begin_webauthn_registration(&mfa, Id::new(7002), T0).expect("second challenge");
    let client_data2 = client_data_json("webauthn.create", &challenge2.challenge_b64, ORIGIN);
    let auth_data2 = registration_auth_data(RP_ID, &cred_id, &cose_es256(&pub_raw), 0, 0x41);
    let att_obj2 = attestation_object("none", &auth_data2, enc::map(vec![]));
    assert!(matches!(
        finish_webauthn_registration(
            &mfa, RP_ID, ORIGIN, &challenge2.challenge_b64, T0, &client_data2, &att_obj2,
        ),
        Err(Error::Conflict(_))
    ));

    // Debug redacts the credential id and key.
    let debug = format!("{credential:?}");
    assert!(!debug.contains("test-credential-id-001"));
    assert!(debug.contains("[redacted]"));
}

#[test]
fn webauthn_registration_refusals() {
    let mfa = MemorySecondFactorStore::new();
    let account_id = Id::new(7010);
    let (pub_raw, _priv) = p256_pair();
    let cred_id = b"refusal-cred".to_vec();
    let cose = cose_es256(&pub_raw);

    // Unknown challenge.
    let client_data = client_data_json("webauthn.create", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", ORIGIN);
    let auth_data = registration_auth_data(RP_ID, &cred_id, &cose, 0, 0x41);
    let att_obj = attestation_object("none", &auth_data, enc::map(vec![]));
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", T0, &client_data, &att_obj),
        Err(Error::InvalidInput(message)) if message.contains("unknown")
    ));

    // Wrong origin.
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let bad_origin = client_data_json("webauthn.create", &challenge.challenge_b64, "https://evil.example");
    let att_bad = attestation_object("none", &auth_data, enc::map(vec![]));
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &bad_origin, &att_bad),
        Err(Error::InvalidInput(message)) if message.contains("origin")
    ));

    // Wrong RP hash.
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let wrong_rp = registration_auth_data("other.example", &cred_id, &cose, 0, 0x41);
    let att_wrong = attestation_object("none", &wrong_rp, enc::map(vec![]));
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_wrong),
        Err(Error::InvalidInput(message)) if message.contains("different party")
    ));

    // Missing user-presence flag.
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let no_up = registration_auth_data(RP_ID, &cred_id, &cose, 0, 0x40);
    let att_no_up = attestation_object("none", &no_up, enc::map(vec![]));
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_no_up),
        Err(Error::InvalidInput(message)) if message.contains("user-presence")
    ));

    // Unsupported algorithm (RS256 COSE id) is refused with reason.
    let cose_rs256 = enc::map(vec![
        (enc::uint(1), enc::uint(3)),
        (enc::uint(3), enc::nint(-257)),
        (enc::nint(-1), enc::uint(1)),
        (enc::nint(-2), enc::bstr(&pub_raw[1..33])),
        (enc::nint(-3), enc::bstr(&pub_raw[33..65])),
    ]);
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let rs_auth = registration_auth_data(RP_ID, &cred_id, &cose_rs256, 0, 0x41);
    let att_rs = attestation_object("none", &rs_auth, enc::map(vec![]));
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_rs),
        Err(Error::InvalidInput(message)) if message.contains("not supported")
    ));

    // Unsupported attestation format is refused, never silently accepted.
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let att_fido = attestation_object("fido-u2f", &auth_data, enc::map(vec![]));
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_fido),
        Err(Error::InvalidInput(message)) if message.contains("not supported")
    ));

    // Expired challenge: refused, and consumed (no second probe).
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let att_obj = attestation_object("none", &auth_data, enc::map(vec![]));
    let late = T0 + birchtree::domain::user::WEBAUTHN_CHALLENGE_TTL_MS + 1;
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, late, &client_data, &att_obj),
        Err(Error::InvalidInput(message)) if message.contains("expired")
    ));
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, late, &client_data, &att_obj),
        Err(Error::InvalidInput(message)) if message.contains("already used")
    ));
}

#[test]
fn webauthn_packed_self_attestation_verifies_signature() {
    let mfa = MemorySecondFactorStore::new();
    let account_id = Id::new(7020);
    let (pub_raw, priv_pkcs8) = p256_pair();
    let cred_id = b"packed-cred-01".to_vec();

    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let auth_data = registration_auth_data(RP_ID, &cred_id, &cose_es256(&pub_raw), 0, 0x41);
    let mut message = auth_data.clone();
    message.extend_from_slice(&sha256_bytes(&client_data));
    let sig = p256_sign(&priv_pkcs8, &message);
    let stmt = enc::map(vec![
        (enc::tstr("alg"), enc::nint(-7)),
        (enc::tstr("sig"), enc::bstr(&sig)),
    ]);
    let att_obj = attestation_object("packed", &auth_data, stmt);
    finish_webauthn_registration(
        &mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_obj,
    )
    .expect("packed self-attestation verifies");

    // Tampered signature is refused (real verification, not shape-checked).
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let mut bad_sig = sig.clone();
    let last = bad_sig.len() - 1;
    bad_sig[last] ^= 0x01;
    let stmt = enc::map(vec![
        (enc::tstr("alg"), enc::nint(-7)),
        (enc::tstr("sig"), enc::bstr(&bad_sig)),
    ]);
    let att_obj = attestation_object("packed", &auth_data, stmt);
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_obj),
        Err(Error::InvalidInput(message)) if message.contains("signature is invalid")
    ));

    // Attestation chains are refused with reason (no CA verification here).
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    let client_data = client_data_json("webauthn.create", &challenge.challenge_b64, ORIGIN);
    let stmt = enc::map(vec![
        (enc::tstr("alg"), enc::nint(-7)),
        (enc::tstr("sig"), enc::bstr(&sig)),
        (enc::tstr("x5c"), enc::array(vec![enc::bstr(b"fake-cert")])),
    ]);
    let att_obj = attestation_object("packed", &auth_data, stmt);
    assert!(matches!(
        finish_webauthn_registration(&mfa, RP_ID, ORIGIN, &challenge.challenge_b64, T0, &client_data, &att_obj),
        Err(Error::InvalidInput(message)) if message.contains("not supported")
    ));
}

#[test]
fn webauthn_authentication_and_sign_count() {
    let mfa = MemorySecondFactorStore::new();
    let account_id = Id::new(7030);
    let (pub_raw, priv_pkcs8) = p256_pair();
    let cred_id = b"auth-cred-01".to_vec();

    // Register first (none attestation).
    let reg = begin_webauthn_registration(&mfa, account_id, T0).expect("reg begin");
    let reg_data = client_data_json("webauthn.create", &reg.challenge_b64, ORIGIN);
    let reg_auth = registration_auth_data(RP_ID, &cred_id, &cose_es256(&pub_raw), 0, 0x41);
    finish_webauthn_registration(
        &mfa, RP_ID, ORIGIN, &reg.challenge_b64, T0,
        &reg_data, &attestation_object("none", &reg_auth, enc::map(vec![])),
    )
    .expect("registered");

    // Authenticate with sign_count 1.
    let login = begin_webauthn_authentication(&mfa, account_id, T0).expect("auth begin");
    assert_eq!(login.challenge_b64.len(), 43);
    let client_data = client_data_json("webauthn.get", &login.challenge_b64, ORIGIN);
    let mut auth_data = sha256_bytes(RP_ID.as_bytes());
    auth_data.push(0x01);
    auth_data.extend_from_slice(&1u32.to_be_bytes());
    let mut message = auth_data.clone();
    message.extend_from_slice(&sha256_bytes(&client_data));
    let sig = p256_sign(&priv_pkcs8, &message);
    let (stored, kind) = finish_webauthn_authentication(
        &mfa, RP_ID, ORIGIN, &login.challenge_b64, T0,
        &cred_id, &client_data, &auth_data, &sig,
    )
    .expect("authentication verifies");
    assert_eq!(kind, SecondFactorKind::WebAuthn);
    assert_eq!(stored.sign_count, 1);

    // Replaying the same (now stale) count with a fresh challenge is refused.
    let login2 = begin_webauthn_authentication(&mfa, account_id, T0).expect("auth begin 2");
    let client_data2 = client_data_json("webauthn.get", &login2.challenge_b64, ORIGIN);
    let mut auth_data2 = sha256_bytes(RP_ID.as_bytes());
    auth_data2.push(0x01);
    auth_data2.extend_from_slice(&1u32.to_be_bytes());
    let mut message2 = auth_data2.clone();
    message2.extend_from_slice(&sha256_bytes(&client_data2));
    let sig2 = p256_sign(&priv_pkcs8, &message2);
    assert!(matches!(
        finish_webauthn_authentication(&mfa, RP_ID, ORIGIN, &login2.challenge_b64, T0, &cred_id, &client_data2, &auth_data2, &sig2),
        Err(Error::InvalidInput(message)) if message.contains("cloned")
    ));

    // A signature from another key is refused with the generic shape.
    let (_other_pub, other_priv) = p256_pair();
    let login3 = begin_webauthn_authentication(&mfa, account_id, T0).expect("auth begin 3");
    let client_data3 = client_data_json("webauthn.get", &login3.challenge_b64, ORIGIN);
    let mut auth_data3 = sha256_bytes(RP_ID.as_bytes());
    auth_data3.push(0x01);
    auth_data3.extend_from_slice(&2u32.to_be_bytes());
    let mut message3 = auth_data3.clone();
    message3.extend_from_slice(&sha256_bytes(&client_data3));
    let wrong_sig = p256_sign(&other_priv, &message3);
    assert!(matches!(
        finish_webauthn_authentication(&mfa, RP_ID, ORIGIN, &login3.challenge_b64, T0, &cred_id, &client_data3, &auth_data3, &wrong_sig),
        Err(Error::InvalidInput(message)) if message.contains("authentication failed")
    ));

    // Unknown credential ids are refused.
    let login4 = begin_webauthn_authentication(&mfa, account_id, T0).expect("auth begin 4");
    assert!(matches!(
        finish_webauthn_authentication(&mfa, RP_ID, ORIGIN, &login4.challenge_b64, T0, b"no-such-cred", &client_data3, &auth_data3, &wrong_sig),
        Err(Error::InvalidInput(message)) if message.contains("unknown")
    ));

    // Revocation removes the credential (and refuses strangers' deletes).
    assert!(mfa.delete_webauthn(account_id, &cred_id).expect("delete"));
    assert!(mfa.webauthn_for_account(account_id).expect("list").is_empty());
    assert!(!mfa.delete_webauthn(account_id, &cred_id).expect("second delete"));
}

// ---------------------------------------------------------------------------
// Min-age + time zone
// ---------------------------------------------------------------------------

#[test]
fn min_age_gate() {
    // T0 = 2023-11-14T22:13:20Z.
    assert!(check_date_of_birth("2007-11-14", T0).is_ok(), "exactly 16");
    let err = check_date_of_birth("2007-11-15", T0).expect_err("one day short");
    assert!(matches!(err, Error::InvalidInput(message) if message.contains("at least 16")));
    assert!(check_date_of_birth("1990-05-30", T0).is_ok());
    assert!(check_date_of_birth("2000-02-29", T0).is_ok(), "leap day parses");
    // Impossible, malformed, and future dates share the invalid shape.
    for bad in ["2021-02-29", "2007-13-01", "2007-00-10", "2007-11-31", "not-a-date", "2007/11/14", "", "2030-01-01"] {
        let err = check_date_of_birth(bad, T0).expect_err(&format!("{bad} refused"));
        assert!(
            matches!(err, Error::InvalidInput(ref message) if message.contains("invalid")),
            "got {err:?} for {bad}"
        );
    }
}

#[test]
fn time_zone_gate() {
    for good in ["America/New_York", "Europe/Paris", "UTC", "utc", "America/Argentina/Buenos_Aires", "Pacific/Auckland"] {
        assert!(validate_time_zone(good).is_ok(), "{good} accepted");
    }
    for bad in ["", "Foo", "America New York", "America//New_York", "/Paris", "America/", &"A".repeat(65)] {
        assert!(validate_time_zone(bad).is_err(), "{bad} refused");
    }
}

#[test]
fn registration_profile_gates_before_account_exists() {
    let (_dir, store) = open_store();
    let app = test_app(8101);
    let base = RegistrationProfile::default();

    // Under-age: refused, and nothing was created (no orphan row/token).
    let under_age = RegistrationProfile {
        date_of_birth: Some("2015-06-01".to_string()),
        ..base.clone()
    };
    let before = store.account_count().expect("count");
    let err = register_account_with_profile(
        &store, &store, &store, &store,
        "https://example.social", T0, true, false, true, 16, &app, &under_age,
        reg_cmd("kiddo", "kiddo@example.com"),
    )
    .expect_err("under-age refused");
    assert!(matches!(err, Error::InvalidInput(message) if message.contains("at least 16")));
    assert_eq!(store.account_count().expect("count"), before);

    // Bad time zone: same refusal shape family, same atomicity.
    let bad_tz = RegistrationProfile {
        time_zone: Some("Not A Zone".to_string()),
        ..base.clone()
    };
    assert!(matches!(
        register_account_with_profile(
            &store, &store, &store, &store,
            "https://example.social", T0, true, false, true, 16, &app, &bad_tz,
            reg_cmd("tzfail", "tzfail@example.com"),
        ),
        Err(Error::InvalidInput(message)) if message.contains("Time zone")
    ));

    // Valid profile: registration proceeds.
    let good = RegistrationProfile {
        time_zone: Some("America/New_York".to_string()),
        date_of_birth: Some("1990-05-30".to_string()),
    };
    let registered = register_account_with_profile(
        &store, &store, &store, &store,
        "https://example.social", T0, true, false, true, 16, &app, &good,
        reg_cmd("adult", "adult@example.com"),
    )
    .expect("adult registers");
    assert_eq!(registered.account.username, "adult");
}

// ---------------------------------------------------------------------------
// Email-domain / canonical-email blocks vs confirmation
// ---------------------------------------------------------------------------

#[test]
fn email_block_policy_matrix() {
    let (_dir, store) = open_store();
    let app = test_app(8201);
    let profile = RegistrationProfile::default();

    store
        .admin_create_email_domain_block("blocked.example", false, T0)
        .expect("domain block");
    store
        .admin_create_email_domain_block("approval.example", true, T0)
        .expect("approval block");
    let victim_hash = infra_uc::canonical_email_hash("victim@example.com");
    store
        .admin_create_canonical_email_block(&victim_hash, T0)
        .expect("canonical block");

    let attempt = |username: &str, email: &str| {
        register_account_with_profile(
            &store, &store, &store, &store,
            "https://example.social", T0, true, false, true, 16, &app, &profile,
            reg_cmd(username, email),
        )
    };

    // Hard-blocked domain: refused before anything commits.
    let err = attempt("spammer", "spammer@blocked.example").expect_err("blocked domain");
    assert!(
        matches!(err, Error::InvalidInput(ref message) if message.contains("domain is blocked")),
        "got {err:?}"
    );
    // Case-insensitive domain match.
    assert!(attempt("spammer2", "x@BLOCKED.EXAMPLE").is_err());

    // allow_with_approval domain: lands in the admin pending queue.
    let pending = attempt("newcomer", "newcomer@approval.example").expect("queued");
    let user = store
        .user_by_account_id(pending.account.id)
        .expect("user")
        .expect("row");
    assert!(!user.approved, "approval-gated address queues");

    // Canonical block: the exact address and its canonical equivalents refuse.
    for email in ["victim@example.com", "Victim@Example.COM", "v.i.c.t.i.m+tag@example.com"] {
        let err = attempt("v", email).expect_err(&format!("canonical {email}"));
        assert!(
            matches!(err, Error::InvalidInput(ref message) if message.contains("Email is blocked")),
            "got {err:?} for {email}"
        );
    }

    // A merely similar address is unaffected.
    let clean = attempt("victim2", "victim2@example.com").expect("similar ok");
    assert!(store.user_by_account_id(clean.account.id).expect("user").expect("row").approved);

    // Pure-policy unit matrix (no store): domain allow / approval / refuse.
    let blocks = store
        .admin_email_domain_blocks_page(None, None, None, 100)
        .expect("page");
    assert_eq!(
        check_registration_email(&blocks, false, "a@clean.example").expect("allow"),
        EmailPolicyOutcome::Allow
    );
    assert_eq!(
        check_registration_email(&blocks, false, "a@approval.example").expect("approval"),
        EmailPolicyOutcome::RequireApproval
    );
}

#[test]
fn email_blocks_reconciled_with_confirmation() {
    let (_dir, store) = open_store();

    // A legacy account created before the block existed: confirmed, logging in.
    let legacy = cli_account(&store, "legacy", Some("legacy@soon-blocked.example"));
    store
        .store_email_token(
            "ge-legacy-token",
            EmailToken { account_id: legacy.id, kind: EmailTokenKind::Confirmation, created_at_ms: T0 },
        )
        .expect("token");
    store
        .admin_create_email_domain_block("soon-blocked.example", false, T0)
        .expect("block after signup");

    // Confirmation redemption stays live for blocked addresses (blocks gate
    // signup only — a later block never strands confirmation).
    let user = confirm_email(&store, "ge-legacy-token", T0).expect("still confirms");
    assert!(user.email_verified);
    // And the confirmed legacy account keeps logging in.
    assert!(
        auth_uc::authenticate_password(&store, "legacy", "example.social", "password123")
            .expect("auth")
            .is_some()
    );

    // New signups on the now-blocked domain are refused (policy matrix).
    let app = test_app(8202);
    assert!(matches!(
        register_account_with_profile(
            &store, &store, &store, &store,
            "https://example.social", T0, true, false, true, 16, &app,
            &RegistrationProfile::default(),
            reg_cmd("late", "late@soon-blocked.example"),
        ),
        Err(Error::InvalidInput(_))
    ));
}

// ---------------------------------------------------------------------------
// IP blocks: true-client-IP matrix across proxy shapes
// ---------------------------------------------------------------------------

fn ip_blocks_fixture() -> Vec<birchtree::domain::AdminIpBlock> {
    use birchtree::domain::{AdminIpBlock, AdminIpSeverity};
    vec![
        AdminIpBlock {
            id: Id::new(1),
            ip: "203.0.113.0/24".to_string(),
            severity: AdminIpSeverity::NoAccess,
            comment: String::new(),
            expires_at_ms: None,
            created_at_ms: T0,
        },
        AdminIpBlock {
            id: Id::new(2),
            ip: "198.51.100.7".to_string(),
            severity: AdminIpSeverity::SignUpBlock,
            comment: String::new(),
            expires_at_ms: None,
            created_at_ms: T0,
        },
        AdminIpBlock {
            id: Id::new(3),
            ip: "192.0.2.0/24".to_string(),
            severity: AdminIpSeverity::NoAccess,
            comment: String::new(),
            expires_at_ms: Some(T0 - 1), // expired: must not match
            created_at_ms: T0 - 10_000,
        },
    ]
}

fn header_map(xff: Option<&str>) -> axum::http::HeaderMap {
    let mut headers = axum::http::HeaderMap::new();
    if let Some(value) = xff {
        headers.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_str(value).expect("header"),
        );
    }
    headers
}

#[test]
fn ip_block_true_client_matrix() {
    use std::net::IpAddr;
    let blocks = ip_blocks_fixture();
    let client: IpAddr = "203.0.113.7".parse().unwrap();
    let signup_blocked: IpAddr = "198.51.100.7".parse().unwrap();
    let proxy1: IpAddr = "10.0.0.1".parse().unwrap();
    let proxy2: IpAddr = "10.0.0.2".parse().unwrap();
    let attacker: IpAddr = "198.51.100.99".parse().unwrap();
    let expired_net: IpAddr = "192.0.2.5".parse().unwrap();

    // (peer, X-Forwarded-For, trusted proxies, expected resolved client)
    let shapes: Vec<(IpAddr, Option<&str>, Vec<IpAddr>, IpAddr)> = vec![
        (client, None, vec![], client), // direct
        (proxy1, Some("203.0.113.7"), vec![proxy1], client), // single proxy
        (
            proxy2,
            Some("203.0.113.7, 10.0.0.1, 10.0.0.2"),
            vec![proxy1, proxy2],
            client, // chained: leftmost wins
        ),
        // Untrusted peer spoofing a victim header: the header is ignored.
        (attacker, Some("203.0.113.7"), vec![proxy1], attacker),
        // Trusted peer with garbage header: falls back to the peer.
        (proxy1, Some("not-an-ip"), vec![proxy1], proxy1),
    ];
    for (peer, xff, trusted, expected) in &shapes {
        // Both client-IP helpers agree on every shape.
        let via_api = birchtree::api::mastodon::request::client_ip(
            &header_map(*xff),
            *peer,
            trusted,
        );
        assert_eq!(via_api, *expected, "api helper for peer {peer} xff {xff:?}");
        let forwarded = xff.as_deref().map(str::to_string);
        let via_uc = infra_uc::client_ip(forwarded.as_deref(), Some(*peer), trusted)
            .expect("peer always resolves");
        assert_eq!(via_uc, *expected, "use-case helper for peer {peer} xff {xff:?}");
    }

    // Enforcement on the resolved address: the /24 no_access block fires for
    // the true client in every trusted shape, never for the spoof.
    for (peer, xff, trusted, expected) in &shapes {
        let resolved = infra_uc::client_ip(xff.as_deref().map(str::to_string).as_deref(), Some(*peer), trusted)
            .expect("resolve");
        assert_eq!(resolved, *expected);
        let blocked = infra_uc::is_no_access_blocked(&blocks, resolved, T0);
        assert_eq!(
            blocked,
            *expected == client,
            "no_access for resolved {resolved} (peer {peer}, xff {xff:?})"
        );
    }

    // Severity isolation: sign_up_block never triggers no_access and vice versa.
    assert!(!infra_uc::is_no_access_blocked(&blocks, signup_blocked, T0));
    assert!(infra_uc::is_sign_up_blocked(&blocks, signup_blocked, T0));
    assert!(!infra_uc::is_sign_up_blocked(&blocks, client, T0));
    // Expired blocks never match.
    assert!(!infra_uc::is_no_access_blocked(&blocks, expired_net, T0));
}

// ---------------------------------------------------------------------------
// D35: per-action flags, numeric role ids, real bitmask
// ---------------------------------------------------------------------------

struct RoleFixture {
    _dir: tempfile::TempDir,
    store: Arc<birchtree::persistence::redb::RedbStore>,
    app: OAuthApplication,
}

impl RoleFixture {
    fn new() -> Self {
        let (dir, store) = common::temp_store();
        let registered = auth_uc::register_app(
            &*store,
            "g-e roles".to_string(),
            vec!["https://client.example/cb".to_string()],
            vec![
                "admin:read".to_string(),
                "admin:write".to_string(),
                "admin:read:accounts".to_string(),
                "admin:write:accounts".to_string(),
                "admin:read:reports".to_string(),
                "admin:write:reports".to_string(),
                "read".to_string(),
            ],
            None,
            false,
            T0,
        )
        .expect("register app");
        Self { _dir: dir, store, app: registered.app }
    }

    fn auth(&self, username: &str, role: UserRole, scopes: &[&str]) -> Authenticated {
        let account = match self.store.account_by_handle(username, "example.social").expect("store") {
            Some(account) => account,
            None => common::mk_account(&self.store, username),
        };
        if role.is_staff() {
            self.store.set_user_role(account.id, role).expect("grant");
        }
        let (_, token) = auth_uc::issue_user_token(
            &*self.store,
            self.app.id,
            account.id,
            scopes.iter().map(|s| s.to_string()).collect(),
            T0,
        )
        .expect("token");
        Authenticated { token, app: self.app.clone(), approved: true }
    }
}

const ALL_ACTIONS: [AdminAction; 9] = [
    AdminAction::ViewAccounts,
    AdminAction::ModerateAccount,
    AdminAction::ReviewReports,
    AdminAction::ManageDomainBlocks,
    AdminAction::ManageIpBlocks,
    AdminAction::ManageEmailBlocks,
    AdminAction::ManageTags,
    AdminAction::ViewMetrics,
    AdminAction::ManageAnnouncements,
];

#[test]
fn per_action_flag_matrix() {
    let fx = RoleFixture::new();
    let full_scopes = &["admin:read", "admin:write"];
    let moderator = fx.auth("mod", UserRole::Moderator, full_scopes);
    let admin = fx.auth("root", UserRole::Admin, full_scopes);
    let plain = fx.auth("plain", UserRole::None, full_scopes);

    // Moderator passes exactly its upstream flag set: accounts, reports,
    // taxonomies (tags), dashboard (metrics) — and nothing else.
    for action in ALL_ACTIONS {
        let result = moderator.require_admin_action(&*fx.store, action, full_scopes);
        let pass = result.is_ok();
        let expected = matches!(
            action,
            AdminAction::ViewAccounts
                | AdminAction::ModerateAccount
                | AdminAction::ReviewReports
                | AdminAction::ManageTags
                | AdminAction::ViewMetrics
        );
        assert_eq!(pass, expected, "moderator {:?}", action);
    }
    // Admin passes every action; role-less passes none (role error, not scope).
    for action in ALL_ACTIONS {
        assert!(
            admin.require_admin_action(&*fx.store, action, full_scopes).is_ok(),
            "admin {:?}",
            action
        );
        let err = plain
            .require_admin_action(&*fx.store, action, full_scopes)
            .expect_err("role-less refused");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(err.message, "Forbidden");
    }

    // Ordering: role check runs before the scope check (upstream
    // `PermissionsConcern` before the controller scope filter).
    let scoped_out_admin = fx.auth("root2", UserRole::Admin, &["read"]);
    let err = scoped_out_admin
        .require_admin_action(&*fx.store, AdminAction::ViewAccounts, &["admin:write"])
        .expect_err("scoped-out admin");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert_eq!(err.message, "This action is outside the authorized scopes");
    let role_less_scoped_out = fx.auth("plain2", UserRole::None, &["read"]);
    let err = role_less_scoped_out
        .require_admin_action(&*fx.store, AdminAction::ViewAccounts, &["admin:write"])
        .expect_err("role-less and scoped-out");
    assert_eq!(err.message, "Forbidden", "role refusal wins");

    // App-only tokens 422 before any flag check.
    let (_, app_token) = auth_uc::issue_client_credentials_token(
        &*fx.store,
        fx.app.id,
        vec!["admin:read".to_string()],
        T0,
    )
    .expect("app token");
    let app_auth = Authenticated { token: app_token, app: fx.app.clone(), approved: true };
    let err = app_auth
        .require_admin_action(&*fx.store, AdminAction::ViewAccounts, &["admin:read"])
        .expect_err("app-only refused");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Action names are stable and unique (audit rows key on them).
    let mut names: Vec<&str> = ALL_ACTIONS.iter().map(|a| a.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), ALL_ACTIONS.len());
}

#[test]
fn role_flag_bits_match_upstream() {
    // Moderator = exactly the upstream set (`user_role.rb:20-44`): dashboard,
    // audit log, feeds, users, reports, taxonomies. (Note: the dto.rs comment
    // claims this sums to `1049888`, but the listed positions sum to
    // `1049884` — arithmetic slip in that comment, flagged for parent
    // re-verification against the pin; the positions themselves are the
    // verified claim and are shared here, not forked.)
    assert_eq!(
        UserRole::Moderator.permissions(),
        PERM_VIEW_DASHBOARD
            | PERM_VIEW_AUDIT_LOG
            | PERM_VIEW_FEEDS
            | PERM_MANAGE_USERS
            | PERM_MANAGE_REPORTS
            | PERM_MANAGE_TAXONOMIES
    );
    assert_eq!(UserRole::Moderator.permissions(), 1049884);
    assert_eq!(UserRole::None.permissions(), 0);
    assert!(UserRole::Admin.permissions() > MODERATOR_PERMISSIONS);
    assert_eq!(UserRole::Admin.permissions(), ADMIN_PERMISSIONS);
    assert_eq!(MODERATOR_PERMISSIONS, 1049884);
    // Spot-check individual bits.
    assert!(UserRole::Moderator.has_permission(PERM_MANAGE_USERS));
    assert!(UserRole::Moderator.has_permission(PERM_MANAGE_REPORTS));
    assert!(UserRole::Moderator.has_permission(PERM_MANAGE_TAXONOMIES));
    assert!(UserRole::Moderator.has_permission(PERM_VIEW_DASHBOARD));
    assert!(!UserRole::Moderator.has_permission(PERM_MANAGE_BLOCKS));
    assert!(!UserRole::Moderator.has_permission(PERM_MANAGE_FEDERATION));
    assert!(!UserRole::None.has_permission(PERM_MANAGE_USERS));
    assert!(!UserRole::None.has_permission(0), "zero flag never grants");
    assert!(UserRole::Admin.has_permission(PERM_MANAGE_BLOCKS));
    // `allows` agrees with the raw flag check on every action.
    for action in ALL_ACTIONS {
        assert_eq!(
            UserRole::Moderator.allows(action),
            UserRole::Moderator.has_permission(action.required_permission())
        );
    }
}

#[test]
fn role_ids_are_numeric_and_round_trip() {
    assert_eq!(UserRole::None.numeric_id(), 0);
    assert_eq!(UserRole::Moderator.numeric_id(), 1);
    assert_eq!(UserRole::Admin.numeric_id(), 2);
    for role in [UserRole::None, UserRole::Moderator, UserRole::Admin] {
        assert_eq!(UserRole::from_numeric_id(role.numeric_id()), Some(role));
    }
    assert_eq!(UserRole::from_numeric_id(3), None);
    assert_eq!(UserRole::Moderator.role_name(), "Moderator");
    assert_eq!(UserRole::None.role_name(), "User");
}

#[test]
fn admin_account_serializer_is_numeric_with_real_bitmask() {
    let (_dir, store) = open_store();
    let server = common::server_info();
    let admin = cli_account(&store, "ser-admin", Some("ser-admin@example.com"));
    let moderator = cli_account(&store, "ser-mod", Some("ser-mod@example.com"));
    let plain = cli_account(&store, "ser-plain", None);
    store.set_user_role(admin.id, UserRole::Admin).expect("grant");
    store.set_user_role(moderator.id, UserRole::Moderator).expect("grant");

    let render = |id: Id| {
        let account = store.account_by_id(id).expect("account").expect("row");
        let user = store.user_by_account_id(id).expect("user");
        birchtree::api::mastodon::endpoints::admin_accounts::admin_account_json(
            &server, &store, &store, &account, user.as_ref(),
        )
    };
    let admin_role = &render(admin.id)["role"];
    assert_eq!(admin_role["id"], serde_json::json!(2));
    assert_eq!(admin_role["name"], serde_json::json!("Admin"));
    assert_eq!(
        admin_role["permissions"],
        serde_json::json!(ADMIN_PERMISSIONS.to_string())
    );
    let mod_role = &render(moderator.id)["role"];
    assert_eq!(mod_role["id"], serde_json::json!(1));
    assert_eq!(mod_role["permissions"], serde_json::json!("1049884"));
    let user_role = &render(plain.id)["role"];
    assert_eq!(user_role["id"], serde_json::json!(0));
    assert_eq!(user_role["permissions"], serde_json::json!("0"));
    // No `-1` id and no empty-permissions shape anywhere in the output.
    for account_id in [admin.id, moderator.id, plain.id] {
        let body = serde_json::to_string(&render(account_id)).expect("json");
        assert!(!body.contains("\"-1\""), "no -1 role id: {body}");
        assert!(!body.contains("\"permissions\":\"\""), "no empty bitmask: {body}");
    }
}

#[tokio::test]
async fn admin_accounts_http_numeric_roles_and_role_ids_filter() {
    use common::{get_request, json, send};
    let srv = TestServer::new(false);
    let admin = common::mk_account(&srv.store, "http-admin");
    let moderator = common::mk_account(&srv.store, "http-mod");
    srv.store.set_user_role(admin.id, UserRole::Admin).expect("grant");
    srv.store.set_user_role(moderator.id, UserRole::Moderator).expect("grant");
    let app = auth_uc::register_app(
        &*srv.store,
        "http roles".to_string(),
        vec!["urn:ietf:wg:oauth:2.0:oob".to_string()],
        vec!["admin:read".to_string(), "admin:write".to_string()],
        None,
        false,
        T0,
    )
    .expect("app");
    let token = |account: Id| {
        auth_uc::issue_user_token(
            &*srv.store,
            app.app.id,
            account,
            vec!["admin:read".to_string(), "admin:write".to_string()],
            T0,
        )
        .expect("token")
        .0
    };
    let admin_token = token(admin.id);

    // Show renders the numeric role with the real bitmask.
    let (status, _, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/admin/accounts/{}", moderator.id),
            Some(&admin_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let body = json(&bytes);
    assert_eq!(body["role"]["id"], serde_json::json!(1));
    assert_eq!(body["role"]["permissions"], serde_json::json!("1049884"));

    // `role_ids=1` (numeric) selects the moderator; `role_ids=2` the admin.
    let (status, _, bytes) = send(
        &srv.router,
        get_request("/api/v2/admin/accounts?role_ids=1", Some(&admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let page = json(&bytes);
    let ids: Vec<String> = page
        .as_array()
        .expect("array")
        .iter()
        .map(|row| row["id"].as_str().expect("id").to_string())
        .collect();
    assert!(ids.contains(&moderator.id.to_string()), "moderator listed: {ids:?}");
    assert!(!ids.contains(&admin.id.to_string()), "admin excluded: {ids:?}");

    // A moderator still passes the accounts reads (per-action flag gate).
    let mod_token = token(moderator.id);
    let (status, _, _) = send(
        &srv.router,
        get_request("/api/v1/admin/accounts", Some(&mod_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // ... but is refused outside its flags (domain blocks need federation).
    let (status, _, _) = send(
        &srv.router,
        get_request("/api/v1/admin/domain_blocks", Some(&mod_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Login flow: password + second factor staging, auth.rs gates
// ---------------------------------------------------------------------------

#[test]
fn login_with_mfa_stages_and_verifies() {
    let (_dir, store) = open_store();
    let mfa = MemorySecondFactorStore::new();
    let account = cli_account(&store, "login-alice", Some("login-alice@example.com"));

    // No factor: straight through.
    assert!(matches!(
        login_with_mfa(&store, &mfa, "login-alice", "example.social", "password123", None, T0),
        Ok(LoginOutcome::Authenticated(_))
    ));
    // Wrong password: uniform refusal (no MFA enrolled yet).
    let no_mfa_err = match login_with_mfa(&store, &mfa, "login-alice", "example.social", "wrongpass1", None, T0) {
        Err(err) => err,
        Ok(_) => panic!("wrong password must fail"),
    };
    assert!(matches!(no_mfa_err, Error::Unauthorized(message) if message == "Invalid username or password"));

    // Enroll + confirm.
    let enrollment = enroll_totp(&mfa, "login-alice", "example.social", account.id, T0).expect("enroll");
    let secret = base32_decode(&enrollment.secret_base32).expect("secret");
    let code = totp_code_at(&secret, totp_counter_at(T0));
    confirm_totp(&mfa, account.id, &code, T0).expect("confirm");

    // Wrong password now gives the SAME refusal (no enrollment oracle).
    let mfa_err = match login_with_mfa(&store, &mfa, "login-alice", "example.social", "wrongpass1", None, T0) {
        Err(err) => err,
        Ok(_) => panic!("wrong password must fail"),
    };
    assert!(matches!(mfa_err, Error::Unauthorized(message) if message == "Invalid username or password"));

    // Right password without a code stages the second step (no secrets in hand).
    assert!(matches!(
        login_with_mfa(&store, &mfa, "login-alice", "example.social", "password123", None, T0),
        Ok(LoginOutcome::MfaRequired(_))
    ));
    // Wrong second factor: the uniform second-factor refusal.
    let later_ms = (totp_counter_at(T0) + 2) * 30 * 1000;
    let fresh = totp_code_at(&secret, totp_counter_at(later_ms));
    assert!(matches!(
        login_with_mfa(&store, &mfa, "login-alice", "example.social", "password123", Some("000000"), later_ms),
        Err(Error::Unauthorized(message)) if message == INVALID_2FA_MESSAGE
    ));
    // Right second factor: authenticated. Unknown users share the password shape.
    assert!(matches!(
        login_with_mfa(&store, &mfa, "login-alice", "example.social", "password123", Some(&fresh), later_ms),
        Ok(LoginOutcome::Authenticated(_))
    ));
    assert!(matches!(
        login_with_mfa(&store, &mfa, "nobody", "example.social", "password123", None, T0),
        Err(Error::Unauthorized(message)) if message == "Invalid username or password"
    ));
}

#[test]
fn auth_gate_mfa_required_and_verify_shapes() {
    let fx = RoleFixture::new();
    let mfa = MemorySecondFactorStore::new();
    let authed = fx.auth("mfa-user", UserRole::None, &["read"]);
    let user_id = authed.token.user_id.expect("user token");

    assert!(!authed.mfa_required(&mfa, user_id).expect("none enrolled"));
    let enrollment = enroll_totp(&mfa, "mfa-user", "example.social", user_id, T0).expect("enroll");
    // Unconfirmed enrollments do not gate (cannot lock the owner out mid-setup).
    assert!(!authed.mfa_required(&mfa, user_id).expect("unconfirmed"));
    let secret = base32_decode(&enrollment.secret_base32).expect("secret");
    confirm_totp(&mfa, user_id, &totp_code_at(&secret, totp_counter_at(T0)), T0).expect("confirm");
    assert!(authed.mfa_required(&mfa, user_id).expect("confirmed gates"));

    // Verify gate: ok on the live code, uniform 401 otherwise.
    let later_ms = (totp_counter_at(T0) + 2) * 30 * 1000;
    let fresh = totp_code_at(&secret, totp_counter_at(later_ms));
    assert!(authed.verify_second_factor(&mfa, user_id, &fresh, later_ms).is_ok());
    let err = authed.verify_second_factor(&mfa, user_id, "000000", later_ms).expect_err("wrong");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    assert_eq!(err.message, INVALID_2FA_MESSAGE);
    let err = authed
        .verify_second_factor(&mfa, Id::new(999999), "123456", later_ms)
        .expect_err("unknown enrollment");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    assert_eq!(err.message, INVALID_2FA_MESSAGE);
}

// ---------------------------------------------------------------------------
// Store plumbing: single-use challenges, revocation, secret hygiene
// ---------------------------------------------------------------------------

#[test]
fn second_factor_store_plumbing() {
    let mfa = MemorySecondFactorStore::new();
    let account_id = Id::new(9001);

    // TOTP put/get/delete.
    assert!(mfa.totp_for(account_id).expect("read").is_none());
    mfa.put_totp(TotpRecord {
        account_id,
        secret_base32: base32_encode(b"12345678901234567890"),
        created_at_ms: T0,
        confirmed: true,
        backup_hashes: vec![],
        last_counter: 0,
    })
    .expect("put");
    assert!(mfa.totp_for(account_id).expect("read").is_some());
    assert!(mfa.delete_totp(account_id).expect("delete"));
    assert!(!mfa.delete_totp(account_id).expect("second delete"));

    // Challenges are single-use.
    let challenge = begin_webauthn_registration(&mfa, account_id, T0).expect("begin");
    assert!(
        mfa.take_webauthn_challenge(&challenge.challenge_b64)
            .expect("take")
            .is_some()
    );
    assert!(
        mfa.take_webauthn_challenge(&challenge.challenge_b64)
            .expect("take again")
            .is_none()
    );

    // Challenge mint refuses short randomness (caller bug, never silent).
    assert!(matches!(
        WebauthnChallenge::mint(account_id, WebauthnPurpose::Registration, &[1u8, 2, 3], T0),
        Err(Error::Internal(_))
    ));
}

#[test]
fn admin_area_gate_still_holds_for_compat_callers() {
    // The area-level gate keeps its exact shipped behavior for the call
    // sites that still use it (moderator = accounts+reports only).
    let fx = RoleFixture::new();
    let moderator = fx.auth("compat-mod", UserRole::Moderator, &["admin:read"]);
    assert!(
        moderator
            .require_admin(&*fx.store, AdminArea::Accounts, &["admin:read"])
            .is_ok()
    );
    assert!(
        moderator
            .require_admin(&*fx.store, AdminArea::Reports, &["admin:read"])
            .is_ok()
    );
    assert!(
        moderator
            .require_admin(&*fx.store, AdminArea::AdminOnly, &["admin:read"])
            .is_err()
    );
}

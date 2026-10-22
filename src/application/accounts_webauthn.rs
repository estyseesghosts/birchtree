//! WebAuthn registration/authentication ceremonies (S11 slice group A).
//!
//! Verbatim extraction proposal from [`crate::application::accounts`]: the
//! registration and authentication ceremony plumbing
//! ([`begin_webauthn_registration`], [`finish_webauthn_registration`],
//! [`begin_webauthn_authentication`], [`finish_webauthn_authentication`]).
//! Scalar parameters are unchanged.
//!
//! [`SecondFactorStore`] and [`SecondFactorKind`] stay in
//! [`crate::application::accounts`] (shared with the TOTP paths in
//! [`crate::application::accounts_mfa`]); this module imports them. The
//! parent owns the cutover: declare `mod accounts_webauthn;` in
//! `src/application/mod.rs` and delete the moved items from `accounts.rs`.

use crate::application::accounts::{SecondFactorKind, SecondFactorStore};
use crate::domain::user::{
    WebauthnChallenge, WebauthnCredential, WebauthnPurpose, verify_webauthn_assertion,
    verify_webauthn_registration,
};
use crate::domain::{Error, Id, Result};

use super::accounts_mfa::random_bytes;

/// Mint a registration ceremony challenge (returned to the adapter, which
/// hands it to the authenticator; the row is consumed by
/// [`finish_webauthn_registration`]).
pub fn begin_webauthn_registration(
    store: &dyn SecondFactorStore,
    account_id: Id,
    now_ms: u64,
) -> Result<WebauthnChallenge> {
    let challenge = WebauthnChallenge::mint(
        account_id,
        WebauthnPurpose::Registration,
        &random_bytes::<32>(),
        now_ms,
    )?;
    store.store_webauthn_challenge(challenge.clone())?;
    Ok(challenge)
}

/// Finish a registration ceremony: consume the challenge (single-use —
/// unknown, expired, or already-used challenges are indistinguishable) and
/// persist the verified credential. `Conflict` when the credential id is
/// already registered.
pub fn finish_webauthn_registration(
    store: &dyn SecondFactorStore,
    rp_id: &str,
    origin: &str,
    challenge_b64: &str,
    now_ms: u64,
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> Result<WebauthnCredential> {
    let challenge = store
        .take_webauthn_challenge(challenge_b64)?
        .ok_or_else(|| {
            Error::InvalidInput("WebAuthn challenge is unknown, expired, or already used".into())
        })?;
    let parsed = verify_webauthn_registration(
        rp_id,
        origin,
        &challenge,
        now_ms,
        client_data_json,
        attestation_object,
    )?;
    if store
        .webauthn_by_credential_id(&parsed.credential_id)?
        .is_some()
    {
        return Err(Error::Conflict(
            "this security key is already registered".into(),
        ));
    }
    let mut point = vec![0x04u8];
    point.extend_from_slice(&parsed.x);
    point.extend_from_slice(&parsed.y);
    store.put_webauthn(WebauthnCredential {
        account_id: challenge.account_id,
        credential_id: parsed.credential_id,
        public_key_uncompressed: point,
        sign_count: parsed.sign_count,
        created_at_ms: now_ms,
    })
}

/// Mint an authentication ceremony challenge.
pub fn begin_webauthn_authentication(
    store: &dyn SecondFactorStore,
    account_id: Id,
    now_ms: u64,
) -> Result<WebauthnChallenge> {
    let challenge = WebauthnChallenge::mint(
        account_id,
        WebauthnPurpose::Authentication,
        &random_bytes::<32>(),
        now_ms,
    )?;
    store.store_webauthn_challenge(challenge.clone())?;
    Ok(challenge)
}

/// Finish an authentication ceremony: consume the challenge and verify the
/// assertion against the stored credential, persisting the new sign count.
/// Any failure is a client error with no oracle beyond the staged flow
/// (the challenge row is gone either way — no second probe).
pub fn finish_webauthn_authentication(
    store: &dyn SecondFactorStore,
    rp_id: &str,
    origin: &str,
    challenge_b64: &str,
    now_ms: u64,
    credential_id: &[u8],
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature_der: &[u8],
) -> Result<(WebauthnCredential, SecondFactorKind)> {
    let challenge = store
        .take_webauthn_challenge(challenge_b64)?
        .ok_or_else(|| {
            Error::InvalidInput("WebAuthn challenge is unknown, expired, or already used".into())
        })?;
    let mut credential = store
        .webauthn_by_credential_id(credential_id)?
        .ok_or_else(|| Error::InvalidInput("WebAuthn credential is unknown".into()))?;
    let sign_count = verify_webauthn_assertion(
        &credential,
        rp_id,
        origin,
        &challenge,
        now_ms,
        client_data_json,
        authenticator_data,
        signature_der,
    )?;
    credential.sign_count = sign_count;
    let stored = store.put_webauthn(credential)?;
    Ok((stored, SecondFactorKind::WebAuthn))
}

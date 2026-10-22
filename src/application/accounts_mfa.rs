//! TOTP/backup-code second factors and mail send paths (S11 slice group A).
//!
//! Verbatim extraction proposal from [`crate::application::accounts`]:
//! password login staging ([`login_with_mfa`]), the TOTP
//! enrollment/verification and backup-code paths ([`enroll_totp`],
//! [`confirm_totp`], [`revoke_totp`], [`verify_second_factor`]), and the mail
//! send paths ([`request_confirmation_email`], [`request_password_reset`]).
//! Scalar parameters are unchanged; the `MailTokenInput` bundling and the
//! `RegisterInput` collapse are parent-owned follow-ups.
//!
//! [`SecondFactorStore`] and [`SecondFactorKind`] stay in
//! [`crate::application::accounts`] (shared with the WebAuthn ceremonies in
//! [`crate::application::accounts_webauthn`] and the email-token redemption
//! paths); this module imports them. [`InviteStore`] also stays in
//! [`crate::application::accounts`] by proposal instruction. The parent owns
//! the cutover: declare `mod accounts_mfa;` in `src/application/mod.rs` and
//! delete the moved items from `accounts.rs`.

use crate::application::accounts::{SecondFactorKind, SecondFactorStore};
use crate::application::auth;
use crate::application::ports::AccountStore;
use crate::domain::user::{
    BACKUP_CODE_BYTES, BACKUP_CODE_COUNT, TOTP_SECRET_LEN, TotpRecord, base32_decode,
    base32_encode, consume_backup_code_hash, verify_totp_at,
};
use crate::domain::{Error, Id, LocalUser, Result};
use crate::mailer::{MailError, Mailer};

use rand::Rng as _;

/// Queue a confirmation mail through the mailer port (called after commit,
/// never inside the request transaction).
pub fn request_confirmation_email(
    mailer: &dyn Mailer,
    from: &str,
    to: &str,
    account_id: Id,
    token: &str,
    base_url: &str,
) -> std::result::Result<(), MailError> {
    if token.is_empty() {
        return Err(MailError::Transport(
            "confirmation token is missing; message not sent".to_string(),
        ));
    }
    mailer.send_confirmation(from, to, account_id, token, base_url)
}

/// Queue a password-reset mail. Unknown addresses are the caller's business
/// (the HTTP layer answers 200 either way to avoid account enumeration, as
/// upstream's Devise flow does); an invalid address shape is refused here.
pub fn request_password_reset(
    mailer: &dyn Mailer,
    from: &str,
    to: &str,
    account_id: Id,
    token: &str,
    base_url: &str,
) -> std::result::Result<(), MailError> {
    if token.is_empty() {
        return Err(MailError::Transport(
            "reset token is missing; message not sent".to_string(),
        ));
    }
    mailer.send_password_reset(from, to, account_id, token, base_url)
}

/// Uniform login-gate refusal for any second-factor failure (wrong TOTP,
/// spent or unknown backup code, unconfirmed enrollment): one message, so
/// the gate is never an oracle for which credential failed.
pub const INVALID_2FA_MESSAGE: &str = "Invalid two-factor authentication code";

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::rng().fill_bytes(&mut buf);
    buf
}

/// A fresh TOTP enrollment: the `otpauth://` URI and base32 secret, each
/// shown to the owner exactly once (the secret is never returned again).
#[derive(Debug, Clone)]
pub struct TotpEnrollment {
    pub otpauth_uri: String,
    pub secret_base32: String,
}

/// Begin a TOTP enrollment: mint a secret and store it unconfirmed.
/// `Conflict` when an enrollment already exists (revoke first).
pub fn enroll_totp(
    store: &dyn SecondFactorStore,
    username: &str,
    domain: &str,
    account_id: Id,
    now_ms: u64,
) -> Result<TotpEnrollment> {
    if store.totp_for(account_id)?.is_some() {
        return Err(Error::Conflict(
            "a second factor is already enrolled for this account".into(),
        ));
    }
    let secret = random_bytes::<TOTP_SECRET_LEN>();
    let secret_base32 = base32_encode(&secret);
    store.put_totp(TotpRecord {
        account_id,
        secret_base32: secret_base32.clone(),
        created_at_ms: now_ms,
        confirmed: false,
        backup_hashes: Vec::new(),
        last_counter: 0,
    })?;
    Ok(TotpEnrollment {
        otpauth_uri: format!(
            "otpauth://totp/{domain}:{username}?secret={secret_base32}&issuer={domain}&algorithm=SHA1&digits=6&period=30"
        ),
        secret_base32,
    })
}

/// Mint fresh backup-code plaintexts (shown once; only hashes are stored).
fn mint_backup_codes() -> Vec<String> {
    (0..BACKUP_CODE_COUNT)
        .map(|_| crate::application::crypto::random_token_hex(BACKUP_CODE_BYTES))
        .collect()
}

/// Confirm an enrollment with a live TOTP code: flips `confirmed`, seeds the
/// replay counter, mints backup codes, and returns the plaintext codes
/// (shown once — the caller displays them, then drops them).
pub fn confirm_totp(
    store: &dyn SecondFactorStore,
    account_id: Id,
    code: &str,
    now_ms: u64,
) -> Result<Vec<String>> {
    let mut record = store
        .totp_for(account_id)?
        .ok_or_else(|| Error::NotFound("no second-factor enrollment exists".into()))?;
    if record.confirmed {
        return Err(Error::InvalidInput(
            "second factor is already confirmed".into(),
        ));
    }
    let secret = base32_decode(&record.secret_base32)
        .map_err(|_| Error::InvalidInput("second-factor enrollment is corrupt".into()))?;
    let counter = verify_totp_at(&secret, code, now_ms, record.last_counter)
        .ok_or_else(|| Error::InvalidInput(INVALID_2FA_MESSAGE.into()))?;
    record.confirmed = true;
    record.last_counter = counter;
    let codes = mint_backup_codes();
    record.backup_hashes = codes
        .iter()
        .map(|code| crate::domain::user::hash_backup_code(code))
        .collect();
    store.put_totp(record)?;
    Ok(codes)
}

/// Revoke a TOTP enrollment (and its backup codes + WebAuthn credentials
/// stay untouched — they are revoked separately).
pub fn revoke_totp(store: &dyn SecondFactorStore, account_id: Id) -> Result<bool> {
    store.delete_totp(account_id)
}

/// Verify a login second factor: a live TOTP code first, then a backup code.
/// Any failure is the uniform [`INVALID_2FA_MESSAGE`] refusal — the gate
/// never reveals whether TOTP or backup codes were tried, or whether the
/// account enrolls a factor at all. Backup codes are single-use: a consumed
/// code is removed before returning.
pub fn verify_second_factor(
    store: &dyn SecondFactorStore,
    account_id: Id,
    code: &str,
    now_ms: u64,
) -> Result<SecondFactorKind> {
    let unauthorized = || Error::Unauthorized(INVALID_2FA_MESSAGE.into());
    let mut record = store.totp_for(account_id)?.ok_or_else(unauthorized)?;
    if !record.confirmed {
        return Err(unauthorized());
    }
    let secret = base32_decode(&record.secret_base32).map_err(|_| unauthorized())?;
    if let Some(counter) = verify_totp_at(&secret, code, now_ms, record.last_counter) {
        record.last_counter = counter;
        store.put_totp(record)?;
        return Ok(SecondFactorKind::Totp);
    }
    if consume_backup_code_hash(&mut record.backup_hashes, code) {
        store.put_totp(record)?;
        return Ok(SecondFactorKind::BackupCode);
    }
    Err(unauthorized())
}

/// Burn the same CPU as a real TOTP check without a record, so a login for
/// an account without MFA does not return measurably faster.
fn dummy_mfa_verify(now_ms: u64) {
    let _ = verify_totp_at(&[0u8; TOTP_SECRET_LEN], "000000", now_ms, 0);
}

/// Password login with second-factor staging.
///
/// - Wrong password (or unknown/disabled/unapproved account) → the uniform
///   `Unauthorized("Invalid username or password")`, with a dummy MFA check
///   so timing reveals nothing about MFA enrollment.
/// - Right password, no confirmed factor → `Authenticated`.
/// - Right password, factor enrolled, no `mfa_code` → `MfaRequired` (the
///   adapter renders the second-factor step; the returned [`LocalUser`]
///   carries no secret material beyond the pre-existing password hash).
/// - Right password + `mfa_code` → `Authenticated`, or the uniform
///   [`INVALID_2FA_MESSAGE`] refusal.
///
/// The staged shape mirrors upstream's two-step web login: the OTP error is
/// only reachable after the password passed, so the distinct messages are
/// not an enumeration oracle.
pub enum LoginOutcome {
    Authenticated(LocalUser),
    MfaRequired(LocalUser),
}

pub fn login_with_mfa(
    accounts: &dyn AccountStore,
    mfa: &dyn SecondFactorStore,
    username: &str,
    domain: &str,
    password: &str,
    mfa_code: Option<&str>,
    now_ms: u64,
) -> Result<LoginOutcome> {
    let invalid_password = || Error::Unauthorized("Invalid username or password".into());
    let Some(user) = auth::authenticate_password(accounts, username, domain, password)? else {
        dummy_mfa_verify(now_ms);
        return Err(invalid_password());
    };
    let totp_enrolled = matches!(mfa.totp_for(user.account_id)?, Some(record) if record.confirmed);
    let webauthn_enrolled = !mfa.webauthn_for_account(user.account_id)?.is_empty();
    if !totp_enrolled && !webauthn_enrolled {
        return Ok(LoginOutcome::Authenticated(user));
    }
    match mfa_code {
        None => Ok(LoginOutcome::MfaRequired(user)),
        Some(code) => match verify_second_factor(mfa, user.account_id, code, now_ms) {
            Ok(_) => Ok(LoginOutcome::Authenticated(user)),
            Err(_) => Err(Error::Unauthorized(INVALID_2FA_MESSAGE.into())),
        },
    }
}

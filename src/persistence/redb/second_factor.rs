//! `redb` second-factor rows (gaps batch G-E, parent merge of S4).
//!
//! Implements [`crate::application::accounts::SecondFactorStore`] over the
//! `TOTP_SECRETS` (account id -> sealed or legacy-JSON `TotpRecord`) and
//! `WEBAUTHN_CREDS` (raw credential id bytes -> JSON `WebauthnCredential`)
//! tables (schema v18).
//!
//! Work package 1/11 additions:
//!
//! - TOTP rows are sealed with AES-256-GCM under the installed
//!   [`SecretBox`] (context `totp:<account id>`); a sealed row with a
//!   missing or wrong key is an error, never "no enrollment".
//! - `WEBAUTHN_BY_ACCOUNT` indexes credentials by owner and is maintained in
//!   the same transaction as every credential write; a per-account limit
//!   ([`MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT`]) is enforced there. The index
//!   is created and backfilled lazily (once, marker in `META`), so databases
//!   written before it existed need no schema-version bump.
//! - The email-token and mail-outbox operations that must be atomic with
//!   credential updates.

use crate::application::accounts::SecondFactorStore;
use crate::domain::user::{
    EmailToken, EmailTokenKind, TotpRecord, WebauthnChallenge, WebauthnCredential,
    MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT,
};
use crate::domain::{Error, Id, Job, LocalUser, Result};

use redb::{ReadableTable, WriteTransaction};

use super::accounts::{
    ensure_email_tokens_keyed, insert_email_token, mail_job_kind, take_email_token,
};
use super::secrets::{SecretBox, SecretError};
use super::schema::{
    self, EMAIL_TOKENS, LOCAL_USERS, META, TOTP_SECRETS, WEBAUTHN_BY_ACCOUNT,
    WEBAUTHN_CHALLENGES, WEBAUTHN_CREDS,
};
use super::{decode, encode, RedbStore, StoreResult};

/// Outcome of [`RedbStore::seal_legacy_secrets`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SealReport {
    /// TOTP rows rewritten (legacy plaintext or retired key).
    pub totp_rows: usize,
}

fn totp_context(account_id: Id) -> Vec<u8> {
    format!("totp:{}", account_id.value()).into_bytes()
}

/// Decode a stored TOTP row: sealed envelope (fails closed) or legacy JSON.
fn open_totp(secrets: &SecretBox, account_id: Id, stored: &[u8]) -> Result<TotpRecord> {
    if SecretBox::is_sealed(stored) {
        let plain = secrets.open(&totp_context(account_id), stored)?;
        return Ok(decode(plain.as_slice())?);
    }
    Ok(decode(stored)?)
}

/// Encode a TOTP row for storage, sealed whenever a key is configured.
fn seal_totp(secrets: &SecretBox, record: &TotpRecord) -> Result<Vec<u8>> {
    let json: zeroize::Zeroizing<Vec<u8>> = zeroize::Zeroizing::new(encode(record)?);
    if secrets.is_enabled() {
        return Ok(secrets.seal(&totp_context(record.account_id), json.as_slice())?);
    }
    Ok(json.to_vec())
}

/// Build the `WEBAUTHN_BY_ACCOUNT` index from `WEBAUTHN_CREDS`, once.
fn ensure_webauthn_index(write: &WriteTransaction) -> StoreResult<()> {
    {
        let meta = write.open_table(META)?;
        if meta.get(schema::WEBAUTHN_INDEX_MARKER_KEY)?.is_some() {
            return Ok(());
        }
    }
    let mut owned: Vec<(u64, Vec<u8>)> = Vec::new();
    {
        let creds = write.open_table(WEBAUTHN_CREDS)?;
        for row in creds.iter()? {
            let (key, value) = row?;
            let credential: WebauthnCredential = decode(value.value())?;
            owned.push((credential.account_id.value(), key.value().to_vec()));
        }
    }
    {
        let mut index = write.open_table(WEBAUTHN_BY_ACCOUNT)?;
        for (account, credential_id) in &owned {
            index.insert(
                schema::webauthn_account_key(*account, credential_id).as_slice(),
                (),
            )?;
        }
    }
    let mut meta = write.open_table(META)?;
    meta.insert(schema::WEBAUTHN_INDEX_MARKER_KEY, [1u8].as_slice())?;
    Ok(())
}

/// How many credential ids `account_id` owns, counting at most one past the
/// limit (enough to decide whether another registration fits).
fn owned_credential_count(write: &WriteTransaction, account_id: Id) -> StoreResult<usize> {
    let index = write.open_table(WEBAUTHN_BY_ACCOUNT)?;
    let mut count: usize = 0;
    scan_account(
        &index,
        account_id,
        MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT + 1,
        &mut |_: &[u8]| count += 1,
    )?;
    Ok(count)
}

/// Visit up to `cap` index keys of one account (prefix range scan).
fn scan_account(
    index: &impl ReadableTable<&'static [u8], ()>,
    account_id: Id,
    cap: usize,
    visit: &mut dyn FnMut(&[u8]),
) -> StoreResult<()> {
    let lower: Vec<u8> = schema::webauthn_account_key(account_id.value(), &[]);
    let mut seen: usize = 0;
    if account_id.value() == u64::MAX {
        for row in index.range(lower.as_slice()..)? {
            let (key, _) = row?;
            visit(key.value());
            seen += 1;
            if seen >= cap {
                break;
            }
        }
        return Ok(());
    }
    let upper: Vec<u8> = schema::webauthn_account_key(account_id.value() + 1, &[]);
    for row in index.range(lower.as_slice()..upper.as_slice())? {
        let (key, _) = row?;
        visit(key.value());
        seen += 1;
        if seen >= cap {
            break;
        }
    }
    Ok(())
}

impl RedbStore {
    /// One-time migration after a keyring is installed: rewrite every TOTP
    /// row that is legacy plaintext or sealed under a retired key so it is
    /// sealed under the active key. Safe to repeat. Fails closed: a row that
    /// cannot be opened aborts the whole pass (nothing is rewritten) rather
    /// than being skipped or reset.
    pub fn seal_legacy_secrets(&self) -> Result<SealReport> {
        let secrets: SecretBox = self.secret_box();
        if !secrets.is_enabled() {
            return Err(SecretError::KeyMissing.into());
        }
        let write = self.begin_write()?;
        let mut rewrites: Vec<(u64, Vec<u8>)> = Vec::new();
        {
            let table = write.open_table(TOTP_SECRETS)?;
            for row in table.iter()? {
                let (key, value) = row?;
                if !secrets.needs_reseal(value.value()) {
                    continue;
                }
                let record: TotpRecord =
                    open_totp(&secrets, Id::new(key.value()), value.value())?;
                rewrites.push((key.value(), seal_totp(&secrets, &record)?));
            }
        }
        {
            let mut table = write.open_table(TOTP_SECRETS)?;
            for (account, sealed) in &rewrites {
                table.insert(*account, sealed.as_slice())?;
            }
        }
        write.commit()?;
        Ok(SealReport {
            totp_rows: rewrites.len(),
        })
    }
}

impl SecondFactorStore for RedbStore {
    fn totp_for(&self, account_id: Id) -> Result<Option<TotpRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(TOTP_SECRETS)?;
        match table.get(account_id.value())? {
            Some(row) => Ok(Some(open_totp(&self.secret_box(), account_id, row.value())?)),
            None => Ok(None),
        }
    }

    fn put_totp(&self, record: TotpRecord) -> Result<TotpRecord> {
        let sealed: Vec<u8> = seal_totp(&self.secret_box(), &record)?;
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(TOTP_SECRETS)?;
            table.insert(record.account_id.value(), sealed.as_slice())?;
        }
        write.commit()?;
        Ok(record)
    }

    fn delete_totp(&self, account_id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(TOTP_SECRETS)?;
            table.remove(account_id.value())?.is_some()
        };
        write.commit()?;
        Ok(removed)
    }

    fn webauthn_for_account(&self, account_id: Id) -> Result<Vec<WebauthnCredential>> {
        // First call on a database predating the index: build it once.
        let built: bool = {
            let read = self.begin_read()?;
            let meta = read.open_table(META)?;
            meta.get(schema::WEBAUTHN_INDEX_MARKER_KEY)?.is_some()
        };
        if !built {
            let write = self.begin_write()?;
            ensure_webauthn_index(&write)?;
            write.commit()?;
        }
        let read = self.begin_read()?;
        let index = read.open_table(WEBAUTHN_BY_ACCOUNT)?;
        let creds = read.open_table(WEBAUTHN_CREDS)?;
        let mut credential_ids: Vec<Vec<u8>> = Vec::new();
        scan_account(
            &index,
            account_id,
            MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT + 1,
            &mut |key: &[u8]| credential_ids.push(key[8..].to_vec()),
        )?;
        let mut out: Vec<WebauthnCredential> = Vec::with_capacity(credential_ids.len());
        for credential_id in &credential_ids {
            if let Some(row) = creds.get(credential_id.as_slice())? {
                out.push(decode(row.value())?);
            }
        }
        Ok(out)
    }

    fn webauthn_by_credential_id(
        &self,
        credential_id: &[u8],
    ) -> Result<Option<WebauthnCredential>> {
        let read = self.begin_read()?;
        let table = read.open_table(WEBAUTHN_CREDS)?;
        match table.get(credential_id)? {
            Some(row) => Ok(Some(decode(row.value())?)),
            None => Ok(None),
        }
    }

    fn put_webauthn(&self, credential: WebauthnCredential) -> Result<WebauthnCredential> {
        let write = self.begin_write()?;
        ensure_webauthn_index(&write)?;
        let existing: Option<WebauthnCredential> = {
            let table = write.open_table(WEBAUTHN_CREDS)?;
            match table.get(credential.credential_id.as_slice())? {
                Some(row) => Some(decode(row.value())?),
                None => None,
            }
        };
        match &existing {
            Some(previous) if previous.account_id != credential.account_id => {
                return Err(Error::Conflict(
                    "credential id is already registered to another account".into(),
                ));
            }
            // Same owner: a sign-count update, not a new registration.
            Some(_) => {}
            None => {
                if owned_credential_count(&write, credential.account_id)?
                    >= MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT
                {
                    return Err(Error::Conflict(format!(
                        "an account may register at most \
                         {MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT} security keys"
                    )));
                }
            }
        }
        {
            let mut table = write.open_table(WEBAUTHN_CREDS)?;
            table.insert(
                credential.credential_id.as_slice(),
                encode(&credential)?.as_slice(),
            )?;
        }
        {
            let mut index = write.open_table(WEBAUTHN_BY_ACCOUNT)?;
            index.insert(
                schema::webauthn_account_key(
                    credential.account_id.value(),
                    &credential.credential_id,
                )
                .as_slice(),
                (),
            )?;
        }
        write.commit()?;
        Ok(credential)
    }

    fn delete_webauthn(&self, account_id: Id, credential_id: &[u8]) -> Result<bool> {
        let write = self.begin_write()?;
        ensure_webauthn_index(&write)?;
        let removed = {
            let mut table = write.open_table(WEBAUTHN_CREDS)?;
            // Decode under the read guard, then drop it before the
            // mutable remove (the guard borrows the table).
            let owned: Option<WebauthnCredential> = match table.get(credential_id)? {
                Some(row) => Some(decode(row.value())?),
                None => None,
            };
            match owned {
                Some(cred) if cred.account_id == account_id => {
                    table.remove(credential_id)?;
                    true
                }
                _ => false,
            }
        };
        if removed {
            let mut index = write.open_table(WEBAUTHN_BY_ACCOUNT)?;
            index.remove(
                schema::webauthn_account_key(account_id.value(), credential_id).as_slice(),
            )?;
        }
        write.commit()?;
        Ok(removed)
    }

    // Challenges: single-use rows (removed on take either way); TTL is
    // enforced by callers via `WebauthnChallenge::live_at` (same shape as
    // the memory store).
    fn store_webauthn_challenge(&self, challenge: WebauthnChallenge) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(WEBAUTHN_CHALLENGES)?;
            table.insert(
                challenge.challenge_b64.as_str(),
                encode(&challenge)?.as_slice(),
            )?;
        }
        write.commit()?;
        Ok(())
    }

    fn take_webauthn_challenge(
        &self,
        challenge_b64: &str,
    ) -> Result<Option<WebauthnChallenge>> {
        let write = self.begin_write()?;
        let row: Option<WebauthnChallenge> = {
            let mut table = write.open_table(WEBAUTHN_CHALLENGES)?;
            match table.remove(challenge_b64)? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        write.commit()?;
        Ok(row)
    }

    fn issue_email_token(
        &self,
        account_id: Id,
        email: &str,
        kind: EmailTokenKind,
        now_ms: u64,
    ) -> Result<()> {
        let token: String = crate::application::crypto::random_token_hex(32);
        let sealed: String = self.protect_mail_token(account_id, &token)?;
        let write = self.begin_write()?;
        ensure_email_tokens_keyed(&write)?;
        // Re-issue supersedes: older bearers of the same account and kind
        // stop working the moment the replacement commits, so at most one
        // live bearer per (account, kind) exists. A leaked or mailed older
        // link cannot outlive its resend.
        let superseded: Vec<String> = {
            let tokens = write.open_table(EMAIL_TOKENS)?;
            let mut stale: Vec<String> = Vec::new();
            for row in tokens.iter()? {
                let (key, value) = row?;
                let record: EmailToken = decode(value.value())?;
                if record.account_id == account_id && record.kind == kind {
                    stale.push(key.value().to_string());
                }
            }
            stale
        };
        if !superseded.is_empty() {
            let mut tokens = write.open_table(EMAIL_TOKENS)?;
            for key in &superseded {
                tokens.remove(key.as_str())?;
            }
        }
        insert_email_token(
            &write,
            &token,
            &EmailToken {
                account_id,
                kind,
                created_at_ms: now_ms,
            },
        )?;
        let job = Job::new(
            self.next_id(),
            mail_job_kind(kind),
            serde_json::json!({
                "account_id": account_id.value(),
                "email": email,
                "token": sealed,
            }),
            now_ms,
            now_ms,
        );
        super::jobs::insert_job(&write, &job)?;
        write.commit()?;
        Ok(())
    }

    fn reveal_mail_token(&self, account_id: Id, stored: &str) -> Result<String> {
        self.recover_mail_token(account_id, stored)
    }

    fn confirm_email_token(&self, token: &str, _now_ms: u64) -> Result<LocalUser> {
        // Lifetime policy (deliberate, Devise parity): confirmation bearers
        // never expire (`confirm_within` is unset upstream). They stay live
        // until redeemed; `prune_email_secrets` collects them once the owner
        // is verified. Only password-reset bearers carry a TTL.
        let write = self.begin_write()?;
        ensure_email_tokens_keyed(&write)?;
        let record: EmailToken = take_email_token(&write, token)?.ok_or_else(|| {
            Error::NotFound("confirmation token is invalid or has already been used".into())
        })?;
        if record.kind != EmailTokenKind::Confirmation {
            // Single use regardless of endpoint: the wrong-flow bearer is
            // burned, then the call is refused.
            write.commit()?;
            return Err(Error::InvalidInput(
                "this token is for password reset, not email confirmation".into(),
            ));
        }
        // Any failure below drops `write` uncommitted: the bearer survives.
        let mut user: LocalUser = read_user(&write, record.account_id)?;
        user.email_verified = true;
        write_user(&write, record.account_id, &user)?;
        write.commit()?;
        Ok(user)
    }

    fn reset_password_with_token(
        &self,
        token: &str,
        password_hash: String,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<LocalUser> {
        if password_hash.is_empty() {
            return Err(Error::InvalidInput("password hash is empty".into()));
        }
        let write = self.begin_write()?;
        ensure_email_tokens_keyed(&write)?;
        let record: EmailToken = take_email_token(&write, token)?.ok_or_else(|| {
            Error::NotFound("reset token is invalid or has already been used".into())
        })?;
        let live: bool = now_ms.saturating_sub(record.created_at_ms) <= ttl_ms;
        if record.kind != EmailTokenKind::PasswordReset || !live {
            // Wrong-kind and expired bearers are burned; the caller sees one
            // not-found answer for every dead-bearer case.
            write.commit()?;
            return Err(Error::NotFound(
                "reset token is invalid or has already been used".into(),
            ));
        }
        let mut user: LocalUser = read_user(&write, record.account_id)?;
        user.password_hash = password_hash;
        write_user(&write, record.account_id, &user)?;
        write.commit()?;
        Ok(user)
    }

    fn prune_email_secrets(&self, now_ms: u64, reset_ttl_ms: u64, limit: usize) -> Result<usize> {
        let write = self.begin_write()?;
        ensure_email_tokens_keyed(&write)?;
        let mut stale: Vec<String> = Vec::new();
        {
            let tokens = write.open_table(EMAIL_TOKENS)?;
            let users = write.open_table(LOCAL_USERS)?;
            for row in tokens.iter()? {
                let (key, value) = row?;
                let record: EmailToken = decode(value.value())?;
                let owner: Option<LocalUser> = match users.get(record.account_id.value())? {
                    Some(user_row) => Some(decode(user_row.value())?),
                    None => None,
                };
                let dead: bool = match (&owner, record.kind) {
                    (None, _) => true,
                    (Some(_), EmailTokenKind::PasswordReset) => {
                        now_ms.saturating_sub(record.created_at_ms) > reset_ttl_ms
                    }
                    (Some(user), EmailTokenKind::Confirmation) => user.email_verified,
                };
                if dead {
                    stale.push(key.value().to_string());
                    if stale.len() >= limit {
                        break;
                    }
                }
            }
        }
        {
            let mut tokens = write.open_table(EMAIL_TOKENS)?;
            for key in &stale {
                tokens.remove(key.as_str())?;
            }
        }
        write.commit()?;
        let remaining: usize = limit.saturating_sub(stale.len());
        let redacted: usize = if remaining > 0 {
            self.redact_finished_job_payloads(remaining)?
        } else {
            0
        };
        Ok(stale.len() + redacted)
    }
}

fn read_user(write: &WriteTransaction, account_id: Id) -> Result<LocalUser> {
    let users = write.open_table(LOCAL_USERS)?;
    match users.get(account_id.value())? {
        Some(row) => Ok(decode(row.value())?),
        None => Err(Error::NotFound(format!("local user for {account_id}"))),
    }
}

fn write_user(write: &WriteTransaction, account_id: Id, user: &LocalUser) -> Result<()> {
    let mut users = write.open_table(LOCAL_USERS)?;
    users.insert(account_id.value(), encode(user)?.as_slice())?;
    Ok(())
}

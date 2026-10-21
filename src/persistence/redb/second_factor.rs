//! `redb` second-factor rows (gaps batch G-E, parent merge of S4).
//!
//! Implements [`crate::application::accounts::SecondFactorStore`] over the
//! `TOTP_SECRETS` (account id -> JSON `TotpRecord`) and `WEBAUTHN_CREDS`
//! (raw credential id bytes -> JSON `WebauthnCredential`) tables (schema
//! v18). Challenges stay memory-scoped in the login flow until a durable
//! challenges table lands (follow-up).

use crate::application::accounts::SecondFactorStore;
use crate::domain::user::{TotpRecord, WebauthnChallenge, WebauthnCredential};
use crate::domain::{Id, Result};

use redb::ReadableTable;

use super::schema::{TOTP_SECRETS, WEBAUTHN_CHALLENGES, WEBAUTHN_CREDS};
use super::{decode, encode, RedbStore};

/// Bounded scan cap for the per-account credential listing (no
/// `WEBAUTHN_BY_ACCOUNT` index in this batch).
const WEBAUTHN_ACCOUNT_SCAN_CAP: usize = 64;

impl SecondFactorStore for RedbStore {
    fn totp_for(&self, account_id: Id) -> Result<Option<TotpRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(TOTP_SECRETS)?;
        match table.get(account_id.value())? {
            Some(row) => Ok(Some(decode(row.value())?)),
            None => Ok(None),
        }
    }

    fn put_totp(&self, record: TotpRecord) -> Result<TotpRecord> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(TOTP_SECRETS)?;
            table.insert(record.account_id.value(), encode(&record)?.as_slice())?;
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
        let read = self.begin_read()?;
        let table = read.open_table(WEBAUTHN_CREDS)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (_, value) = row?;
            let cred: WebauthnCredential = decode(value.value())?;
            if cred.account_id == account_id {
                out.push(cred);
                if out.len() >= WEBAUTHN_ACCOUNT_SCAN_CAP {
                    break;
                }
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
        {
            let mut table = write.open_table(WEBAUTHN_CREDS)?;
            table.insert(
                credential.credential_id.as_slice(),
                encode(&credential)?.as_slice(),
            )?;
        }
        write.commit()?;
        Ok(credential)
    }

    fn delete_webauthn(&self, account_id: Id, credential_id: &[u8]) -> Result<bool> {
        let write = self.begin_write()?;
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
}

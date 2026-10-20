//! OAuth application and token persistence, and login sessions.

use redb::{ReadableTable};

use crate::application::crypto::sha256_hex;
use crate::application::ports::{
    NewApp, NewAuthCode, NewSession, NewToken, OAuthStore, SessionStore,
};
use crate::domain::{
    AccessToken, AuthorizationCode, Error, Id, LoginSession, OAuthApplication, Result,
};

use super::schema::{ACCESS_TOKENS, AUTHORIZATION_CODES, OAUTH_APPS, SESSIONS};
use super::{decode, encode, RedbStore};

/// D24: entries walked to revoke one account's tokens. Tokens are few per
/// instance; the cap only stops a pathological table from stalling an admin
/// suspension. A hit leaves the remainder live (reported, never silent).
const ACCOUNT_TOKEN_SCAN_CAP: usize = 100_000;

impl OAuthStore for RedbStore {
    fn register_app(&self, input: NewApp) -> Result<OAuthApplication> {
        let write = self.begin_write()?;
        {
            let apps = write.open_table(OAUTH_APPS)?;
            if apps.get(input.client_id.as_str())?.is_some() {
                return Err(Error::AlreadyExists(format!(
                    "client_id already registered: {}",
                    input.client_id
                )));
            }
        }

        let app = OAuthApplication {
            id: self.next_id(),
            name: input.name,
            client_id: input.client_id,
            client_secret_hash: input.client_secret_hash,
            redirect_uris: input.redirect_uris,
            scopes: input.scopes,
            website: input.website,
            created_at_ms: input.now_ms,
        };
        {
            let mut apps = write.open_table(OAUTH_APPS)?;
            apps.insert(app.client_id.as_str(), encode(&app)?.as_slice())?;
        }
        write.commit()?;
        Ok(app)
    }

    fn app_by_client_id(&self, client_id: &str) -> Result<Option<OAuthApplication>> {
        let read = self.begin_read()?;
        let apps = read.open_table(OAUTH_APPS)?;
        match apps.get(client_id)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn app_by_id(&self, id: Id) -> Result<Option<OAuthApplication>> {
        // Applications are few; a bounded scan is acceptable. (An id index can
        // replace this if the app table ever grows large.)
        let read = self.begin_read()?;
        let apps = read.open_table(OAUTH_APPS)?;
        for entry in apps.iter()? {
            let (_key, value) = entry?;
            let app: OAuthApplication = decode(value.value())?;
            if app.id == id {
                return Ok(Some(app));
            }
        }
        Ok(None)
    }

    fn issue_access_token(&self, input: NewToken) -> Result<AccessToken> {
        let token = AccessToken {
            id: self.next_id(),
            app_id: input.app_id,
            user_id: input.user_id,
            token_hash: input.token_hash,
            scopes: input.scopes,
            created_at_ms: input.now_ms,
            expires_at_ms: input.expires_at_ms,
            revoked: false,
        };
        let write = self.begin_write()?;
        {
            let mut tokens = write.open_table(ACCESS_TOKENS)?;
            tokens.insert(token.token_hash.as_str(), encode(&token)?.as_slice())?;
        }
        write.commit()?;
        Ok(token)
    }

    fn token_by_plaintext(&self, plaintext: &str) -> Result<Option<AccessToken>> {
        let hash = sha256_hex(plaintext.as_bytes());
        let read = self.begin_read()?;
        let tokens = read.open_table(ACCESS_TOKENS)?;
        match tokens.get(hash.as_str())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn revoke_token(&self, plaintext: &str) -> Result<bool> {
        let hash = sha256_hex(plaintext.as_bytes());
        let write = self.begin_write()?;
        let existing: Option<AccessToken> = {
            let tokens = write.open_table(ACCESS_TOKENS)?;
            match tokens.get(hash.as_str())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        let Some(mut token) = existing else {
            return Ok(false);
        };
        token.revoked = true;
        {
            let mut tokens = write.open_table(ACCESS_TOKENS)?;
            tokens.insert(hash.as_str(), encode(&token)?.as_slice())?;
        }
        write.commit()?;
        Ok(true)
    }

    fn revoke_account_tokens(&self, account_id: Id) -> Result<Vec<Id>> {
        let write = self.begin_write()?;
        let mut revoked: Vec<(Id, String)> = Vec::new();
        let mut scanned = 0usize;
        {
            let tokens = write.open_table(ACCESS_TOKENS)?;
            for entry in tokens.iter()? {
                if scanned >= ACCOUNT_TOKEN_SCAN_CAP {
                    break;
                }
                scanned += 1;
                let (key, value) = entry?;
                let token: AccessToken = decode(value.value())?;
                if token.user_id == Some(account_id) && !token.revoked {
                    revoked.push((token.id, key.value().to_owned()));
                }
            }
        }
        if scanned >= ACCOUNT_TOKEN_SCAN_CAP && revoked.is_empty() {
            tracing::warn!(
                account = %account_id,
                scanned,
                "token revocation scan hit its cap"
            );
        }
        // Newest-first so the freshest sessions die first.
        revoked.sort_by(|a, b| b.0.cmp(&a.0));
        {
            let mut tokens = write.open_table(ACCESS_TOKENS)?;
            for (id, key) in &revoked {
                let bytes = {
                    let Some(guard) = tokens.get(key.as_str())? else {
                        continue;
                    };
                    guard.value().to_vec()
                };
                let mut token: AccessToken = decode(&bytes)?;
                let _ = id;
                token.revoked = true;
                tokens.insert(key.as_str(), encode(&token)?.as_slice())?;
            }
        }
        write.commit()?;
        Ok(revoked.into_iter().map(|(id, _)| id).collect())
    }

    fn create_authorization_code(&self, input: NewAuthCode) -> Result<AuthorizationCode> {
        let code = AuthorizationCode {
            id: self.next_id(),
            code_hash: input.code_hash,
            app_id: input.app_id,
            user_id: input.user_id,
            redirect_uri: input.redirect_uri,
            scopes: input.scopes,
            code_challenge: input.code_challenge,
            code_challenge_method: input.code_challenge_method,
            created_at_ms: input.now_ms,
            expires_at_ms: input.now_ms + input.ttl_ms,
            used: false,
        };
        let write = self.begin_write()?;
        {
            let mut codes = write.open_table(AUTHORIZATION_CODES)?;
            codes.insert(code.code_hash.as_str(), encode(&code)?.as_slice())?;
        }
        write.commit()?;
        Ok(code)
    }

    fn authorization_code_by_plaintext(&self, plaintext: &str) -> Result<Option<AuthorizationCode>> {
        let hash = sha256_hex(plaintext.as_bytes());
        let read = self.begin_read()?;
        let codes = read.open_table(AUTHORIZATION_CODES)?;
        match codes.get(hash.as_str())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn redeem_authorization_code(
        &self,
        plaintext: &str,
        token: NewToken,
        now_ms: u64,
    ) -> Result<AccessToken> {
        let hash = sha256_hex(plaintext.as_bytes());
        let write = self.begin_write()?;

        // Read, validate and consume the code inside the same transaction that
        // issues the token, so two concurrent exchanges cannot both succeed.
        let mut code: AuthorizationCode = {
            let codes = write.open_table(AUTHORIZATION_CODES)?;
            match codes.get(hash.as_str())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::InvalidInput("invalid_grant".into())),
            }
        };
        if !code.is_redeemable_at(now_ms) {
            return Err(Error::InvalidInput("invalid_grant".into()));
        }
        code.used = true;
        {
            let mut codes = write.open_table(AUTHORIZATION_CODES)?;
            codes.insert(hash.as_str(), encode(&code)?.as_slice())?;
        }

        let record = AccessToken {
            id: self.next_id(),
            app_id: token.app_id,
            user_id: token.user_id,
            token_hash: token.token_hash,
            scopes: token.scopes,
            created_at_ms: token.now_ms,
            expires_at_ms: token.expires_at_ms,
            revoked: false,
        };
        {
            let mut tokens = write.open_table(ACCESS_TOKENS)?;
            tokens.insert(record.token_hash.as_str(), encode(&record)?.as_slice())?;
        }
        write.commit()?;
        Ok(record)
    }
}

impl SessionStore for RedbStore {
    fn create_session(&self, input: NewSession) -> Result<LoginSession> {
        let session = LoginSession {
            id: self.next_id(),
            user_id: input.user_id,
            token_hash: input.token_hash,
            created_at_ms: input.now_ms,
            expires_at_ms: input.now_ms + input.ttl_ms,
        };
        let write = self.begin_write()?;
        {
            let mut sessions = write.open_table(SESSIONS)?;
            sessions.insert(session.token_hash.as_str(), encode(&session)?.as_slice())?;
        }
        write.commit()?;
        Ok(session)
    }

    fn session_by_plaintext(&self, plaintext: &str) -> Result<Option<LoginSession>> {
        let hash = sha256_hex(plaintext.as_bytes());
        let read = self.begin_read()?;
        let sessions = read.open_table(SESSIONS)?;
        match sessions.get(hash.as_str())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn revoke_session(&self, plaintext: &str) -> Result<bool> {
        let hash = sha256_hex(plaintext.as_bytes());
        let write = self.begin_write()?;
        let existed = {
            let mut sessions = write.open_table(SESSIONS)?;
            sessions.remove(hash.as_str())?.is_some()
        };
        write.commit()?;
        Ok(existed)
    }
}

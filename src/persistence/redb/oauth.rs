//! OAuth application and token persistence, and login sessions.

use redb::{ReadableDatabase, ReadableTable};

use crate::application::crypto::sha256_hex;
use crate::application::ports::{NewApp, NewSession, NewToken, OAuthStore, SessionStore};
use crate::domain::{AccessToken, Error, LoginSession, OAuthApplication, Result};

use super::schema::{ACCESS_TOKENS, OAUTH_APPS, SESSIONS};
use super::{decode, encode, RedbStore};

impl OAuthStore for RedbStore {
    fn register_app(&self, input: NewApp) -> Result<OAuthApplication> {
        let write = self.db().begin_write()?;
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
        let read = self.db().begin_read()?;
        let apps = read.open_table(OAUTH_APPS)?;
        match apps.get(client_id)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
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
        let write = self.db().begin_write()?;
        {
            let mut tokens = write.open_table(ACCESS_TOKENS)?;
            tokens.insert(token.token_hash.as_str(), encode(&token)?.as_slice())?;
        }
        write.commit()?;
        Ok(token)
    }

    fn token_by_plaintext(&self, plaintext: &str) -> Result<Option<AccessToken>> {
        let hash = sha256_hex(plaintext.as_bytes());
        let read = self.db().begin_read()?;
        let tokens = read.open_table(ACCESS_TOKENS)?;
        match tokens.get(hash.as_str())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn revoke_token(&self, plaintext: &str) -> Result<bool> {
        let hash = sha256_hex(plaintext.as_bytes());
        let write = self.db().begin_write()?;
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
        let write = self.db().begin_write()?;
        {
            let mut sessions = write.open_table(SESSIONS)?;
            sessions.insert(session.token_hash.as_str(), encode(&session)?.as_slice())?;
        }
        write.commit()?;
        Ok(session)
    }

    fn session_by_plaintext(&self, plaintext: &str) -> Result<Option<LoginSession>> {
        let hash = sha256_hex(plaintext.as_bytes());
        let read = self.db().begin_read()?;
        let sessions = read.open_table(SESSIONS)?;
        match sessions.get(hash.as_str())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn revoke_session(&self, plaintext: &str) -> Result<bool> {
        let hash = sha256_hex(plaintext.as_bytes());
        let write = self.db().begin_write()?;
        let existed = {
            let mut sessions = write.open_table(SESSIONS)?;
            sessions.remove(hash.as_str())?.is_some()
        };
        write.commit()?;
        Ok(existed)
    }
}

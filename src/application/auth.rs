//! Authentication use cases: password login and token issuance.
//!
//! OAuth endpoint handlers (Phase 2) call these; the CLI bootstrap uses the
//! account-creation path directly.

use crate::application::crypto;
use crate::application::ports::{
    AccountStore, NewApp, NewSession, NewToken, OAuthStore, SessionStore,
};
use crate::domain::account::normalize_username;
use crate::domain::{AccessToken, Id, LocalUser, LoginSession, OAuthApplication, Result};

/// Default session lifetime for the OAuth approval flow (24h).
pub const SESSION_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// Verify a username/password pair for a local account.
///
/// Runs a dummy Argon2 verification for unknown accounts so timing does not
/// reveal whether the handle exists.
pub fn authenticate_password(
    store: &dyn AccountStore,
    normalized_username: &str,
    domain: &str,
    password: &str,
) -> Result<Option<LocalUser>> {
    let handle = normalize_username(normalized_username);
    match store.local_user_by_handle(&handle, domain)? {
        Some(user) if !user.disabled && crypto::verify_password(&user.password_hash, password) => {
            Ok(Some(user))
        }
        Some(_) => {
            crypto::dummy_verify(password);
            Ok(None)
        }
        None => {
            crypto::dummy_verify(password);
            Ok(None)
        }
    }
}

/// Result of registering an OAuth application: the record plus the one-time
/// plaintext client secret (if the app is confidential).
pub struct RegisteredApp {
    pub app: OAuthApplication,
    pub client_secret: Option<String>,
}

/// Register an OAuth application (`POST /api/v1/apps`, wired in Phase 2).
pub fn register_app(
    store: &dyn OAuthStore,
    name: String,
    redirect_uris: Vec<String>,
    scopes: Vec<String>,
    website: Option<String>,
    confidential: bool,
    now_ms: u64,
) -> Result<RegisteredApp> {
    let client_id = crypto::random_token_hex(16);
    let (client_secret, client_secret_hash) = if confidential {
        let secret = crypto::random_token_hex(32);
        let hash = crypto::sha256_hex(secret.as_bytes());
        (Some(secret), Some(hash))
    } else {
        (None, None)
    };

    let app = store.register_app(NewApp {
        name,
        client_id,
        client_secret_hash,
        redirect_uris,
        scopes,
        website,
        now_ms,
    })?;
    Ok(RegisteredApp { app, client_secret })
}

/// Issue a user access token. Returns the plaintext token and its record.
pub fn issue_user_token(
    store: &dyn OAuthStore,
    app_id: Id,
    user_id: Id,
    scopes: Vec<String>,
    now_ms: u64,
) -> Result<(String, AccessToken)> {
    let plaintext = crypto::random_token_hex(32);
    let token_hash = crypto::sha256_hex(plaintext.as_bytes());
    let record = store.issue_access_token(NewToken {
        app_id,
        user_id: Some(user_id),
        token_hash,
        scopes,
        now_ms,
        // Mastodon issues non-expiring access tokens by default.
        expires_at_ms: None,
    })?;
    Ok((plaintext, record))
}

/// Create a login session for the browser-based OAuth approval flow.
pub fn create_login_session(
    store: &dyn SessionStore,
    user_id: Id,
    now_ms: u64,
) -> Result<(String, LoginSession)> {
    let plaintext = crypto::random_token_hex(32);
    let token_hash = crypto::sha256_hex(plaintext.as_bytes());
    let record = store.create_session(NewSession {
        user_id,
        token_hash,
        now_ms,
        ttl_ms: SESSION_TTL_MS,
    })?;
    Ok((plaintext, record))
}

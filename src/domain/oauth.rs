//! OAuth client and token records.
//!
//! These are the persisted shapes behind the Mastodon `/api/v1/apps` and token
//! endpoints (implemented in Phase 2). The secret material — client secret and
//! access token — is stored only as a hash; the plaintext is returned to the
//! caller once at issuance.

use crate::domain::Id;

/// A registered OAuth application (`POST /api/v1/apps`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthApplication {
    pub id: Id,
    pub name: String,
    /// Public client identifier.
    pub client_id: String,
    /// Hex-encoded SHA-256 of the client secret, if the app is confidential.
    pub client_secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub scopes: Vec<String>,
    /// Mastodon "website" field, if provided.
    pub website: Option<String>,
    pub created_at_ms: u64,
}

/// An issued access token. Only the hash is stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccessToken {
    pub id: Id,
    pub app_id: Id,
    /// Set for user tokens; `None` for client-credentials (app-only) tokens.
    pub user_id: Option<Id>,
    /// Hex-encoded SHA-256 of the opaque bearer token.
    pub token_hash: String,
    pub scopes: Vec<String>,
    pub created_at_ms: u64,
    /// `None` matches Mastodon's default of non-expiring access tokens.
    pub expires_at_ms: Option<u64>,
    pub revoked: bool,
}

impl AccessToken {
    /// Whether the token is usable at `now_ms`.
    pub fn is_valid_at(&self, now_ms: u64) -> bool {
        !self.revoked
            && match self.expires_at_ms {
                Some(exp) => now_ms < exp,
                None => true,
            }
    }
}

use crate::domain::Id;

/// Local login credentials and preferences, separate from the ActivityPub actor.
///
/// The password is stored only as an Argon2 PHC string; the plaintext never
/// reaches this record. A `LocalUser` is always attached to a local `Account`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LocalUser {
    pub id: Id,
    pub account_id: Id,
    /// Argon2 PHC-format password hash. Never logged or serialized outward.
    pub password_hash: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub created_at_ms: u64,
    pub disabled: bool,
    /// User posting/reading preferences (Phase 2). `None` means "use the
    /// Mastodon default", so records written before these fields existed stay
    /// readable (`#[serde(default)]`).
    #[serde(default)]
    pub default_privacy: Option<String>,
    #[serde(default)]
    pub default_sensitive: Option<bool>,
    #[serde(default)]
    pub default_language: Option<String>,
    #[serde(default)]
    pub default_quote_policy: Option<String>,
}

/// A server-side login session used to authenticate the OAuth approval flow.
///
/// Only the hash of the session token is persisted; the plaintext is handed to
/// the client once and never stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LoginSession {
    pub id: Id,
    pub user_id: Id,
    /// Hex-encoded SHA-256 of the session cookie/token.
    pub token_hash: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

impl LoginSession {
    /// Whether the session is still valid at `now_ms`.
    pub fn is_valid_at(&self, now_ms: u64) -> bool {
        now_ms < self.expires_at_ms
    }
}

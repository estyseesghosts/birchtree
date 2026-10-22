use crate::domain::Id;

/// A server-side login session used to authenticate the OAuth approval flow.
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

/// One recorded password sign-in (Phase 7.3 admin `active_users` telemetry).
///
/// Birchtree records a row each time a local user completes the browser
/// password login (`POST /oauth/authorize`); API calls on an existing
/// non-expiring token are not sign-ins. Upstream's `active_users` also
/// counts throttled per-request touches (`UserTrackingConcern`), so the
/// counts here are genuine logins and run lower than Mastodon's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignInEvent {
    pub id: Id,
    pub account_id: Id,
    pub created_at_ms: u64,
    /// Client IP of the password login, when the adapter supplied one
    /// (upstream `user.ips`; backfills as `None` via `#[serde(default)]`).
    #[serde(default)]
    pub ip: Option<std::net::IpAddr>,
}

/// A pending email token (confirmation or password reset): the bearer the
/// mailed link carries. Stored by token string; redemption consumes the row
/// (single-use). Tokens are random per request (`random_token_hex`); the
/// deterministic job-id fallback only exists for registration jobs that
/// predate an explicit token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmailToken {
    pub account_id: Id,
    pub kind: EmailTokenKind,
    pub created_at_ms: u64,
}

/// Which flow an [`EmailToken`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTokenKind {
    Confirmation,
    PasswordReset,
}

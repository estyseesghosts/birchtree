use crate::domain::Id;

/// Staff role for the admin API (Phase 7.3).
///
/// Upstream models this as `UserRole` rows with a permission bitmask
/// (`app/models/user_role.rb`); birchtree collapses it to two roles per the
/// decided split in `birchtree.md` §7 Phase 7.3: `moderator` serves the
/// accounts and reports admin routes, `admin` serves every admin route.
/// Accounts without a role (`None`, the default for all pre-7.3 records) are
/// refused by the admin gate with `403 {"error":"Forbidden"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// No staff role (default; every record written before roles existed).
    #[default]
    None,
    /// Moderation staff: the accounts and reports admin areas only.
    Moderator,
    /// Full administrator: every admin area.
    Admin,
}

impl UserRole {
    /// True for either staff role.
    pub fn is_staff(&self) -> bool {
        !matches!(self, UserRole::None)
    }

    /// Parse a CLI role name (`admin`, `moderator`, `none`).
    pub fn parse_cli(s: &str) -> Option<UserRole> {
        match s.to_ascii_lowercase().as_str() {
            "admin" => Some(UserRole::Admin),
            "moderator" => Some(UserRole::Moderator),
            "none" => Some(UserRole::None),
            _ => None,
        }
    }
}

/// Rows written before the approval queue existed were usable accounts, so
/// they decode as approved.
fn default_approved() -> bool {
    true
}

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
    /// Whether an admin approved the account (upstream `User#approved`).
    /// `false` = awaiting approval (the pending queue). Pre-approval-mode
    /// rows decode as `true` (`#[serde(default)]`); public registration sets
    /// this `false` only when the operator requires approval.
    #[serde(default = "default_approved")]
    pub approved: bool,
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
    /// D22 home mention policy (`show`/`collapse`/`filter`; `None` = pre-7.3
    /// record, read as the `show` default).
    #[serde(default)]
    pub home_mention_policy: Option<String>,
    /// D21 `chosen_languages`: the viewer's preferred content languages
    /// (upstream `users.chosen_languages`, set through the web settings UI;
    /// here set through `PATCH /api/v1/accounts/update_credentials`
    /// `source[chosen_languages][]`). `None` = unset: no language filtering.
    /// Applied to the REST public timeline and the public + hashtag streams,
    /// like upstream (`PublicFeed#language_scope`, `streaming/index.js`
    /// `chosenLanguages`); the REST tag timeline, home and list feeds keep
    /// no chosen-language filter, as upstream's `TagFeed`/`HomeFeed`/
    /// `ListFeed` have none.
    #[serde(default)]
    pub chosen_languages: Option<Vec<String>>,
    /// Staff role for the admin API (Phase 7.3). `None` (the `#[serde(default)]`
    /// for every pre-7.3 record) means "no staff role".
    #[serde(default)]
    pub role: UserRole,
}

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
}

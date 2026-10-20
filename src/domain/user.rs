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
    /// OAuth application that created this account via signup (`None` for
    /// CLI-created and pre-attribution records). Upstream's
    /// `require_user_owned_by_application!` gates the email-confirmation
    /// resend/check endpoints on this; birchtree enforces it via
    /// [`crate::application::accounts::check_confirmation_ownership`]:
    /// `None` (grandfathered rows) is served to any calling app, a recorded
    /// id must equal the caller's app id, otherwise `403 Forbidden`.
    #[serde(default)]
    pub created_by_application: Option<Id>,
}

/// Invite-token validity and code generation (round 3, area A).
///
/// Upstream: `app/models/invite.rb` at v4.7.3 (`8ff11c2b`).
/// `Invite#valid_for_use?` (`invite.rb:27-33`) is
/// `(max_uses.nil? || uses < max_uses) && !expired? && user&.functional?`.
/// The inviter-functional check (not suspended/disabled) lives at the
/// application layer, which can read the inviter's credential row; the
/// counter/expiry checks live here so both REST and any future ActivityPub
/// caller share them. Domain knows no HTTP or database shapes.

/// Length of an invite code (upstream `set_code`, `invite.rb:44-49`).
pub const INVITE_CODE_LEN: usize = 8;

/// Maximum invite comment length (upstream `COMMENT_SIZE_LIMIT`, `invite.rb:13`).
pub const INVITE_COMMENT_LIMIT: usize = 420;

/// Upstream `VALID_CODE_CHARACTERS` (`invite.rb:13-19`): alphanumerics minus
/// the homoglyphs `0 1 I l O` (56 characters).
pub const INVITE_CODE_ALPHABET: &[u8] =
    b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Derive an 8-character invite code from a 64-bit seed with xorshift64*.
///
/// Pure function (no RNG in the domain layer): the application layer feeds a
/// fresh seed per attempt and retries on code collision, so uniqueness comes
/// from the store, not from this mapping.
pub fn invite_code_from_seed(mut seed: u64) -> String {
    if seed == 0 {
        seed = 0x9E3779B97F4A7C15;
    }
    let mut out = String::with_capacity(INVITE_CODE_LEN);
    for _ in 0..INVITE_CODE_LEN {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        let idx =
            (seed.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as usize % INVITE_CODE_ALPHABET.len();
        out.push(INVITE_CODE_ALPHABET[idx] as char);
    }
    out
}

/// Whether `code` has the upstream invite-code shape (8 chars, alphabet-only).
pub fn is_valid_invite_code_shape(code: &str) -> bool {
    code.len() == INVITE_CODE_LEN
        && code
            .bytes()
            .all(|b| INVITE_CODE_ALPHABET.contains(&b))
}

/// Why an invite token cannot be used (known code, unusable state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteRefusal {
    /// Past `expires_at_ms` (upstream `Expireable#expired?`).
    Expired,
    /// `uses >= max_uses` (upstream `valid_for_use?` counter half).
    Exhausted,
}

/// An invite token (upstream `invites` table, `app/models/invite.rb:3-17`).
///
/// `autofollow` is intentionally absent: upstream's web flow follows the
/// inviter back on signup (`registrations_controller.rb: after_sign_in_path`),
/// which has no API-observable equivalent here; recording it would be a
/// success-shaped stub. `user_id` is stored as the creator's account id plus
/// a staff snapshot (staff-issued invites bypass the approval queue; see the
/// application layer).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Invite {
    pub id: Id,
    pub code: String,
    pub created_by_account_id: Id,
    /// Snapshot of whether the creator held a staff role when issuing.
    /// Staff-issued invites bypass `registration_requires_approval`
    /// (birchtree's analogue of upstream `bypass_approval?`, which keys off
    /// the `invite_bypass_approval` role permission we do not model).
    pub created_by_staff: bool,
    pub comment: Option<String>,
    /// Maximum redemptions (`None` = unlimited, upstream `max_uses` NULL).
    pub max_uses: Option<u32>,
    pub uses: u32,
    /// Expiry timestamp (`None` = never, upstream `expires_at` NULL).
    pub expires_at_ms: Option<u64>,
    pub created_at_ms: u64,
}

impl Invite {
    /// Upstream `Expireable#expired?`: expired only when an expiry is set and
    /// passed (a NULL `expires_at` is always available, `invite.rb:23` scope
    /// `available`).
    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_some_and(|exp| now_ms >= exp)
    }

    /// The counter/expiry half of upstream `valid_for_use?` (`invite.rb:27-33`,
    /// minus the inviter-functional check done by the application layer).
    pub fn check_usable(&self, now_ms: u64) -> Result<(), InviteRefusal> {
        if self.is_expired_at(now_ms) {
            return Err(InviteRefusal::Expired);
        }
        if self.max_uses.is_some_and(|max| self.uses >= max) {
            return Err(InviteRefusal::Exhausted);
        }
        Ok(())
    }
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

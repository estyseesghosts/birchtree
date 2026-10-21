use crate::domain::Id;

/// Staff role for the admin API (Phase 7.3).
///
/// Upstream models this as `UserRole` rows with a permission bitmask
/// (`app/models/user_role.rb`); birchtree collapses the role table to three
/// rows with synthetic numeric ids (`0` = everyone/`User`, `1` =
/// `Moderator`, `2` = `Admin`) carrying the upstream flag bits below. The
/// ids are stable and round-trip through the v2 `role_ids` filter; the
/// bitmask positions match upstream `UserRole::FLAGS` exactly (moderator =
/// `1049884`, verified against `user_role.rb:20-44` at `8ff11c2b`; an older
/// comment's `1049888` was an arithmetic slip).
/// Accounts without a role (`None`, the default for all pre-7.3 records) are
/// refused by the admin gate with `403 {"error":"Forbidden"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// No staff role (default; every record written before roles existed).
    #[default]
    None,
    /// Moderation staff: exactly the upstream moderator flag set.
    Moderator,
    /// Full administrator: every upstream flag.
    Admin,
}

// Upstream `UserRole::FLAGS` bit positions (`app/models/user_role.rb:20-44`
// at `8ff11c2b`). Single canonical definition: the credential-account role
// serializer must import these (snippet in the G-E report) instead of
// keeping its own copy.
pub const PERM_VIEW_DASHBOARD: u64 = 1 << 3;
pub const PERM_VIEW_AUDIT_LOG: u64 = 1 << 2;
pub const PERM_VIEW_FEEDS: u64 = 1 << 20;
pub const PERM_MANAGE_USERS: u64 = 1 << 10;
pub const PERM_MANAGE_REPORTS: u64 = 1 << 4;
pub const PERM_MANAGE_TAXONOMIES: u64 = 1 << 8;
pub const PERM_MANAGE_FEDERATION: u64 = 1 << 5;
pub const PERM_MANAGE_SETTINGS: u64 = 1 << 6;
pub const PERM_MANAGE_BLOCKS: u64 = 1 << 7;
pub const PERM_MANAGE_APPEALS: u64 = 1 << 9;
pub const PERM_MANAGE_INVITES: u64 = 1 << 11;
pub const PERM_MANAGE_RULES: u64 = 1 << 12;
pub const PERM_MANAGE_ANNOUNCEMENTS: u64 = 1 << 13;
pub const PERM_MANAGE_CUSTOM_EMOJIS: u64 = 1 << 14;
pub const PERM_MANAGE_WEBHOOKS: u64 = 1 << 15;
pub const PERM_MANAGE_USER_ACCESS: u64 = 1 << 18;
pub const PERM_DELETE_USER_DATA: u64 = 1 << 19;
pub const PERM_MANAGE_ROLES: u64 = 1 << 17;

/// Upstream moderator flag set (`view_dashboard | view_audit_log |
/// view_feeds | manage_users | manage_reports | manage_taxonomies` =
/// `1049884`).
pub const MODERATOR_PERMISSIONS: u64 = PERM_VIEW_DASHBOARD
    | PERM_VIEW_AUDIT_LOG
    | PERM_VIEW_FEEDS
    | PERM_MANAGE_USERS
    | PERM_MANAGE_REPORTS
    | PERM_MANAGE_TAXONOMIES;

/// Upstream administrator flag set (moderator set plus every other flag).
pub const ADMIN_PERMISSIONS: u64 = MODERATOR_PERMISSIONS
    | PERM_MANAGE_FEDERATION
    | PERM_MANAGE_SETTINGS
    | PERM_MANAGE_BLOCKS
    | PERM_MANAGE_APPEALS
    | PERM_MANAGE_INVITES
    | PERM_MANAGE_RULES
    | PERM_MANAGE_ANNOUNCEMENTS
    | PERM_MANAGE_CUSTOM_EMOJIS
    | PERM_MANAGE_WEBHOOKS
    | PERM_MANAGE_USER_ACCESS
    | PERM_DELETE_USER_DATA
    | PERM_MANAGE_ROLES;

/// One fine-grained admin action, in the shape of upstream's
/// `Admin::BasePolicy` per-action checks: each action names the single
/// upstream permission flag the corresponding policy consults. The endpoint
/// gate (`require_admin_action`) refuses before the scope check, exactly
/// like upstream's `PermissionsConcern` → controller `authorize` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    /// Read the admin account index/show (`manage_users`).
    ViewAccounts,
    /// Any account-moderation write: enable/disable/silence/suspend/
    /// approve/reject/destroy/strike (`manage_users`).
    ModerateAccount,
    /// Read and resolve the moderator report queue (`manage_reports`).
    ReviewReports,
    /// Domain allow/block management (`manage_federation`).
    ManageDomainBlocks,
    /// IP block management (`manage_blocks`).
    ManageIpBlocks,
    /// Email-domain and canonical-email block management (`manage_blocks`).
    ManageEmailBlocks,
    /// Tag review flags (`manage_taxonomies`).
    ManageTags,
    /// Measures/dimensions/retention reads (`view_dashboard`).
    ViewMetrics,
    /// Announcement authoring (`manage_announcements`).
    ManageAnnouncements,
}

impl AdminAction {
    /// The upstream permission flag this action requires.
    pub fn required_permission(self) -> u64 {
        match self {
            AdminAction::ViewAccounts | AdminAction::ModerateAccount => PERM_MANAGE_USERS,
            AdminAction::ReviewReports => PERM_MANAGE_REPORTS,
            AdminAction::ManageDomainBlocks => PERM_MANAGE_FEDERATION,
            AdminAction::ManageIpBlocks | AdminAction::ManageEmailBlocks => PERM_MANAGE_BLOCKS,
            AdminAction::ManageTags => PERM_MANAGE_TAXONOMIES,
            AdminAction::ViewMetrics => PERM_VIEW_DASHBOARD,
            AdminAction::ManageAnnouncements => PERM_MANAGE_ANNOUNCEMENTS,
        }
    }

    /// Stable name for audit rows and tests.
    pub fn name(self) -> &'static str {
        match self {
            AdminAction::ViewAccounts => "view_accounts",
            AdminAction::ModerateAccount => "moderate_account",
            AdminAction::ReviewReports => "review_reports",
            AdminAction::ManageDomainBlocks => "manage_domain_blocks",
            AdminAction::ManageIpBlocks => "manage_ip_blocks",
            AdminAction::ManageEmailBlocks => "manage_email_blocks",
            AdminAction::ManageTags => "manage_tags",
            AdminAction::ViewMetrics => "view_metrics",
            AdminAction::ManageAnnouncements => "manage_announcements",
        }
    }
}

impl UserRole {
    /// True for either staff role.
    pub fn is_staff(&self) -> bool {
        !matches!(self, UserRole::None)
    }

    /// Stable numeric id for the role serializer and the v2 `role_ids`
    /// filter (`0` = everyone/`User`, `1` = `Moderator`, `2` = `Admin`).
    /// Never `-1`: every id round-trips through [`UserRole::from_numeric_id`].
    pub fn numeric_id(self) -> u64 {
        match self {
            UserRole::None => 0,
            UserRole::Moderator => 1,
            UserRole::Admin => 2,
        }
    }

    /// Inverse of [`UserRole::numeric_id`]; `None` for unknown ids.
    pub fn from_numeric_id(id: u64) -> Option<UserRole> {
        match id {
            0 => Some(UserRole::None),
            1 => Some(UserRole::Moderator),
            2 => Some(UserRole::Admin),
            _ => None,
        }
    }

    /// Display name matching the seeded upstream role names.
    pub fn role_name(self) -> &'static str {
        match self {
            UserRole::None => "User",
            UserRole::Moderator => "Moderator",
            UserRole::Admin => "Admin",
        }
    }

    /// The upstream permission bitmask this role carries (`0` for
    /// non-staff, `1049888` for moderators, the full set for admins).
    pub fn permissions(self) -> u64 {
        match self {
            UserRole::None => 0,
            UserRole::Moderator => MODERATOR_PERMISSIONS,
            UserRole::Admin => ADMIN_PERMISSIONS,
        }
    }

    /// True when the role carries every bit of `flag` (`flag == 0` never
    /// grants: the non-staff zero mask must not pass any action).
    pub fn has_permission(self, flag: u64) -> bool {
        flag != 0 && self.permissions() & flag == flag
    }

    /// True when the role may perform the fine-grained `action` (D35).
    pub fn allows(self, action: AdminAction) -> bool {
        self.has_permission(action.required_permission())
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
    /// Reading preferences (upstream web-settings-only; here settable through
    /// `update_credentials[source][display_media|expand_spoilers|
    /// autoplay_gifs]` as a birchtree extension, like `chosen_languages`).
    /// `None` = upstream default (`default`/`false`/`false`).
    #[serde(default)]
    pub web_display_media: Option<String>,
    #[serde(default)]
    pub web_expand_spoilers: Option<bool>,
    #[serde(default)]
    pub web_auto_play_gifs: Option<bool>,
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
    /// Account locale captured at registration (upstream `user.locale`;
    /// web-settings changes are out of profile).
    #[serde(default)]
    pub locale: Option<String>,
    /// Invite-request text captured at registration (`reason` param, upstream
    /// `user.invite_request.text`).
    #[serde(default)]
    pub invite_request: Option<String>,
    /// IANA time-zone name captured at registration (upstream `user.time_zone`).
    #[serde(default)]
    pub time_zone: Option<String>,
    /// Date of birth (`YYYY-MM-DD`, upstream `user.date_of_birth`).
    #[serde(default)]
    pub date_of_birth: Option<String>,
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

// ---------------------------------------------------------------------------
// Registration profile: `time_zone` / `date_of_birth` + min-age policy.
// ---------------------------------------------------------------------------

/// Minimum age at registration, in full years.
///
/// Upstream collects `date_of_birth` for age assurance; the operator-facing
/// override (`minimum_age_years` config key) is proposed in the G-E report —
/// until it lands this constant is the policy. Rationale: 16 is the GDPR
/// default age of digital consent and the strictest EU member-state rule, so
/// one static gate satisfies every member state.
pub const MINIMUM_AGE_YEARS: i64 = 16;

/// Validate a captured time-zone name.
///
/// Upstream validates inclusion in the IANA database
/// (`ActiveSupport::TimeZone`); without a tz database this validates the
/// `Area/Location` shape (plus bare `UTC`, which Rails also accepts):
/// 1–64 chars of letters, digits, `_`, `-`, `+` with at least one `/`
/// separating two non-empty parts. Anything else is a 422
/// (`Error::InvalidInput`, rendered `Validation failed: ...`).
pub fn validate_time_zone(raw: &str) -> crate::domain::Result<()> {
    use crate::domain::Error;
    let tz = raw.trim();
    if tz.eq_ignore_ascii_case("utc") {
        return Ok(());
    }
    let mut parts = tz.split('/');
    let area_ok = matches!(parts.next(), Some(area) if !area.is_empty());
    let rest: Vec<&str> = parts.collect();
    let shape_ok = !tz.is_empty()
        && tz.len() <= 64
        && area_ok
        && !rest.is_empty()
        && rest.iter().all(|p| !p.is_empty())
        && tz
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'));
    if shape_ok {
        Ok(())
    } else {
        Err(Error::InvalidInput(
            "Validation failed: Time zone is invalid".into(),
        ))
    }
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Parse a strict `YYYY-MM-DD` calendar date into `(year, month, day)`.
fn parse_iso_date(raw: &str) -> Option<(i64, i64, i64)> {
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes.iter().enumerate().all(|(i, b)| {
        i == 4 || i == 7 || b.is_ascii_digit()
    }) {
        return None;
    }
    let year: i64 = raw[0..4].parse().ok()?;
    let month: i64 = raw[5..7].parse().ok()?;
    let day: i64 = raw[8..10].parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some((year, month, day))
}

/// Enforce the min-age policy on a captured date of birth.
///
/// `raw` must be a real past calendar date in `YYYY-MM-DD` form showing an
/// age of at least [`MINIMUM_AGE_YEARS`] full years at `now_ms` (UTC day
/// precision). Malformed, impossible (2021-02-29), future, or under-age
/// dates are `Error::InvalidInput`, which the API layer renders as the
/// upstream-shaped `422 {"error":"Validation failed: ..."}`.
pub fn check_date_of_birth(raw: &str, now_ms: u64) -> crate::domain::Result<()> {
    check_date_of_birth_with_min_age(raw, now_ms, MINIMUM_AGE_YEARS as u8)
}

/// [`check_date_of_birth`] with an operator-configured minimum age
/// (`Config::minimum_age_years`, 13–21). Same shapes, parameterized message.
pub fn check_date_of_birth_with_min_age(
    raw: &str,
    now_ms: u64,
    minimum_age_years: u8,
) -> crate::domain::Result<()> {
    use crate::domain::Error;
    let raw = raw.trim();
    let Some((year, month, day)) = parse_iso_date(raw) else {
        return Err(Error::InvalidInput(
            "Validation failed: Date of birth is invalid".into(),
        ));
    };
    let today_days = (now_ms / 86_400_000) as i64;
    if days_from_civil(year, month, day) > today_days {
        return Err(Error::InvalidInput(
            "Validation failed: Date of birth is invalid".into(),
        ));
    }
    // Full-years age from the civil dates (re-derived from day counts is
    // leap-unsafe, so compare month/day directly against the epoch civil).
    let civil = civil_from_days(today_days);
    let mut age = civil.0 - year;
    if (civil.1, civil.2) < (month, day) {
        age -= 1;
    }
    if age < minimum_age_years as i64 {
        return Err(Error::InvalidInput(format!(
            "Validation failed: Date of birth must indicate an age of at least {minimum_age_years} years"
        )));
    }
    Ok(())
}

/// Civil `(year, month, day)` for days since the Unix epoch (inverse of
/// [`days_from_civil`]).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}

// ---------------------------------------------------------------------------
// TOTP second factor (+ single-use hashed backup codes).
// ---------------------------------------------------------------------------

/// TOTP time-step in seconds (RFC 6238 §4, upstream `rotp` default).
pub const TOTP_STEP_SECS: u64 = 30;
/// TOTP code length in digits (RFC 4226 `Truncate`, upstream default 6).
pub const TOTP_DIGITS: usize = 6;
/// Verification window in steps on either side of the current counter
/// (upstream `verify_with_drift_and_prior`, ±30 s clock skew).
pub const TOTP_WINDOW_STEPS: u64 = 1;
/// Raw secret length in bytes (160 bits, RFC 4226 §4 minimum recommendation).
pub const TOTP_SECRET_LEN: usize = 20;
/// How many backup codes one confirmation mints (upstream mints 16
/// recovery codes; 10 keeps the ceremony short while staying brute-force
/// safe at 64 bits each — see [`BACKUP_CODE_BYTES`]).
pub const BACKUP_CODE_COUNT: usize = 10;
/// Random bytes per backup code (64 bits; stored only as SHA-256).
pub const BACKUP_CODE_BYTES: usize = 8;

/// A TOTP second-factor enrollment (the `TOTP_SECRETS` table row, keyed by
/// account id; JSON shape).
///
/// The secret is the live credential: this struct has a hand-written
/// [`std::fmt::Debug`] that redacts the secret and the backup-code hashes,
/// and no code path may log either (see `tests/secret_handling.rs`
/// conventions). Backup codes are stored only as SHA-256 hex digests and
/// each entry is removed on first use (single-use).
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TotpRecord {
    pub account_id: Id,
    /// Base32 (RFC 4648, no padding) encoding of the raw HMAC-SHA1 secret.
    pub secret_base32: String,
    pub created_at_ms: u64,
    /// False until the owner proves possession with a live code (the
    /// `confirm` step). Unconfirmed enrollments never authenticate.
    pub confirmed: bool,
    /// SHA-256 hex digests of the unused backup codes.
    pub backup_hashes: Vec<String>,
    /// Last accepted TOTP counter (replay protection: a code's counter must
    /// strictly increase).
    pub last_counter: u64,
}

impl std::fmt::Debug for TotpRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TotpRecord")
            .field("account_id", &self.account_id)
            .field("secret_base32", &"[redacted]")
            .field("created_at_ms", &self.created_at_ms)
            .field("confirmed", &self.confirmed)
            .field("backup_hashes", &"[redacted]")
            .field("last_counter", &self.last_counter)
            .finish()
    }
}

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 encoding (uppercase, no padding).
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for &byte in bytes {
        acc = (acc << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32_ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32_ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// RFC 4648 base32 decoding (uppercase or lowercase, optional `=` padding).
/// Whitespace is rejected; unknown characters are an error, never skipped.
pub fn base32_decode(text: &str) -> crate::domain::Result<Vec<u8>> {
    use crate::domain::Error;
    let invalid = || Error::InvalidInput("TOTP secret is not valid base32".into());
    let mut stripped = text.trim().to_ascii_uppercase();
    while stripped.ends_with('=') {
        stripped.pop();
    }
    if stripped.is_empty() || stripped.len() % 8 == 1 {
        return Err(invalid());
    }
    let mut out = Vec::with_capacity(stripped.len() * 5 / 8);
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for byte in stripped.bytes() {
        let value = BASE32_ALPHABET
            .iter()
            .position(|&c| c == byte)
            .ok_or_else(invalid)? as u32;
        acc = (acc << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// TOTP counter for `now_ms` (`T - T0 / X` with `T0 = 0`, RFC 6238 §4).
pub fn totp_counter_at(now_ms: u64) -> u64 {
    now_ms / 1000 / TOTP_STEP_SECS
}

/// HOTP value (RFC 4226 §5.3–§5.4): HMAC-SHA1, dynamic truncation, mod 10⁶.
///
/// SHA-1 is mandated here by RFC 6238 (and by every authenticator app);
/// HMAC-SHA1's security does not rely on collision resistance, so the
/// legacy-use algorithm is the correct primitive — not a downgrade.
fn hotp(secret: &[u8], counter: u64) -> u32 {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = ring::hmac::sign(&key, &counter.to_be_bytes());
    let bytes = tag.as_ref();
    let offset = (bytes[bytes.len() - 1] & 0x0f) as usize;
    let code = ((bytes[offset] & 0x7f) as u32) << 24
        | (bytes[offset + 1] as u32) << 16
        | (bytes[offset + 2] as u32) << 8
        | bytes[offset + 3] as u32;
    code % 1_000_000
}

/// The 6-digit code for `secret` at `counter` (zero-padded).
pub fn totp_code_at(secret: &[u8], counter: u64) -> String {
    format!("{:06}", hotp(secret, counter))
}

/// Constant-time byte comparison for secrets (codes, hashes, challenges).
/// Lengths must match first — every compared value here is fixed-shape, so
/// the length check leaks nothing. (`ring::constant_time` is internal-only
/// in this ring version, so the loop below is the primitive; `black_box`
/// keeps the accumulator from being optimized into an early exit.)
fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// Constant-time code comparison (fixed 6-digit shape enforced first; the
/// length check leaks nothing — every real code is 6 chars).
fn totp_codes_equal(a: &str, b: &str) -> bool {
    if a.len() != TOTP_DIGITS || b.len() != TOTP_DIGITS {
        return false;
    }
    constant_time_equal(a.as_bytes(), b.as_bytes())
}

/// Verify a presented TOTP `code` at `now_ms`.
///
/// The code must be 6 ASCII digits matching a counter in
/// `[now - window, now + window]` that strictly exceeds `last_counter`
/// (replay protection). Returns the matched counter for the caller to persist
/// as the new `last_counter`, or `None` when the code is wrong, malformed,
/// stale (replay), or outside the window. Every candidate is evaluated in
/// constant time; unknown-vs-wrong is indistinguishable by construction.
pub fn verify_totp_at(
    secret: &[u8],
    code: &str,
    now_ms: u64,
    last_counter: u64,
) -> Option<u64> {
    if code.len() != TOTP_DIGITS || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = totp_counter_at(now_ms);
    let low = now.saturating_sub(TOTP_WINDOW_STEPS);
    let high = now.saturating_add(TOTP_WINDOW_STEPS);
    let mut matched: Option<u64> = None;
    for counter in low..=high {
        if counter <= last_counter {
            continue;
        }
        if totp_codes_equal(&totp_code_at(secret, counter), code) {
            matched = Some(counter);
        }
    }
    matched
}

/// SHA-256 hex digest of a normalized backup code (the at-rest form).
pub fn hash_backup_code(code: &str) -> String {
    let normalized = code.trim().to_ascii_lowercase();
    hex::encode(ring::digest::digest(&ring::digest::SHA256, normalized.as_bytes()))
}

/// Consume a backup code: on a constant-time hash match the entry is removed
/// (single-use) and `true` is returned; otherwise the list is untouched.
/// The caller persists the mutated record on success.
pub fn consume_backup_code_hash(hashes: &mut Vec<String>, code: &str) -> bool {
    let candidate = hash_backup_code(code);
    let mut index: Option<usize> = None;
    for (i, stored) in hashes.iter().enumerate() {
        let hit = stored.len() == candidate.len()
            && constant_time_equal(stored.as_bytes(), candidate.as_bytes());
        if hit && index.is_none() {
            index = Some(i);
        }
    }
    match index {
        Some(i) => {
            hashes.remove(i);
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// WebAuthn second factor (challenge/response ceremony, ES256).
// ---------------------------------------------------------------------------

/// WebAuthn challenge lifetime (5 minutes; ceremonies are interactive).
pub const WEBAUTHN_CHALLENGE_TTL_MS: u64 = 5 * 60 * 1000;

/// The only COSE algorithm this build verifies: ES256 (P-256 + SHA-256).
/// RS256/EdDSA authenticators are refused with reason at registration — a
/// recorded divergence, never a silent accept (see
/// [`WEBAUTHN_UNSUPPORTED_ALGORITHM`]).
pub const WEBAUTHN_COSE_ES256: i64 = -7;

/// Upper bound on ceremony inputs (federation input is untrusted; these are
/// local but bounded all the same).
pub const WEBAUTHN_MAX_CEREMONY_BYTES: usize = 8192;

/// Which ceremony a challenge was minted for. Registration and
/// authentication challenges never cross-accept (distinct `clientData`
/// `type` values, checked both here and against the challenge row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebauthnPurpose {
    Registration,
    Authentication,
}

impl WebauthnPurpose {
    /// The `clientDataJSON.type` this purpose requires.
    pub fn client_data_type(self) -> &'static str {
        match self {
            WebauthnPurpose::Registration => "webauthn.create",
            WebauthnPurpose::Authentication => "webauthn.get",
        }
    }
}

/// A single-use login challenge (kept server-side; the `clientDataJSON`
/// echoes it back inside the signed payload).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebauthnChallenge {
    pub account_id: Id,
    /// base64url-no-pad of at least 16 random bytes (fed by the application
    /// layer — no RNG in the domain layer, same rule as invite codes).
    pub challenge_b64: String,
    pub purpose: WebauthnPurpose,
    pub created_at_ms: u64,
}

impl WebauthnChallenge {
    /// Mint a challenge over caller-supplied randomness (`random_bytes`
    /// must hold at least 16 bytes).
    pub fn mint(
        account_id: Id,
        purpose: WebauthnPurpose,
        random_bytes: &[u8],
        now_ms: u64,
    ) -> crate::domain::Result<Self> {
        use base64::Engine as _;
        if random_bytes.len() < 16 {
            return Err(crate::domain::Error::Internal(
                "webauthn challenge needs at least 16 random bytes".into(),
            ));
        }
        Ok(WebauthnChallenge {
            account_id,
            challenge_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(random_bytes),
            purpose,
            created_at_ms: now_ms,
        })
    }

    /// True when the challenge is still live at `now_ms`.
    pub fn live_at(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.created_at_ms) <= WEBAUTHN_CHALLENGE_TTL_MS
    }
}

/// Most WebAuthn credentials (security keys) one account may register.
/// Enforced atomically by the storage port; registration beyond it is a
/// `Conflict`, not a silent truncation of the login-time listing.
pub const MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT: usize = 20;

/// A registered WebAuthn credential (the `WEBAUTHN_CREDS` table row, keyed
/// by raw credential-id bytes; JSON shape).
///
/// The credential id and public key are authentication material: redacted
/// from [`std::fmt::Debug`], never logged.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebauthnCredential {
    pub account_id: Id,
    /// Raw credential-id bytes (the table key).
    pub credential_id: Vec<u8>,
    /// P-256 public point, uncompressed SEC1 (`04 || x || y`, 65 bytes).
    pub public_key_uncompressed: Vec<u8>,
    /// Last authenticator sign count (clone detection, §6.1 of the spec).
    pub sign_count: u32,
    pub created_at_ms: u64,
}

impl std::fmt::Debug for WebauthnCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebauthnCredential")
            .field("account_id", &self.account_id)
            .field("credential_id", &"[redacted]")
            .field("public_key_uncompressed", &"[redacted]")
            .field("sign_count", &self.sign_count)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

/// A verified registration: the credential id, P-256 coordinates and
/// authenticator sign count for the caller to persist.
pub struct ParsedWebauthnCredential {
    pub credential_id: Vec<u8>,
    pub x: [u8; 32],
    pub y: [u8; 32],
    pub sign_count: u32,
}

fn webauthn_err(message: impl Into<String>) -> crate::domain::Error {
    crate::domain::Error::InvalidInput(message.into())
}

/// Minimal definite-length CBOR_decoder (CTAP2 attestation objects only):
/// unsigned/nnegative ints, byte/text strings, arrays, maps. Indefinite
/// lengths, tags, floats and simple values are refused — genuine
/// authenticators never emit them here.
#[derive(Debug, Clone, PartialEq)]
enum Cbor {
    Unsigned(u64),
    Negative(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
}

fn cbor_int_arg(input: &[u8]) -> crate::domain::Result<(u64, usize)> {
    let err = || webauthn_err("WebAuthn CBOR is malformed");
    let (&head, rest) = input.split_first().ok_or_else(err)?;
    let info = head & 0x1f;
    match info {
        0..=23 => Ok((info as u64, 1)),
        24 => rest.first().map(|&b| (b as u64, 2)).ok_or_else(err),
        25 => {
            rest.get(..2)
                .map(|b| (u16::from_be_bytes([b[0], b[1]]) as u64, 3))
                .ok_or_else(err)
        }
        26 => {
            rest.get(..4)
                .map(|b| {
                    (
                        u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64,
                        5,
                    )
                })
                .ok_or_else(err)
        }
        27 => {
            rest.get(..8)
                .map(|b| {
                    (
                        u64::from_be_bytes([
                            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                        ]),
                        9,
                    )
                })
                .ok_or_else(err)
        }
        _ => Err(err()),
    }
}

fn cbor_decode(input: &[u8], depth: u8) -> crate::domain::Result<(Cbor, usize)> {
    let err = || webauthn_err("WebAuthn CBOR is malformed");
    if depth > 8 || input.is_empty() {
        return Err(err());
    }
    let major = input[0] >> 5;
    let (arg, head_len) = cbor_int_arg(input)?;
    let body = &input[head_len..];
    match major {
        0 => Ok((Cbor::Unsigned(arg), head_len)),
        1 => {
            let neg = arg
                .checked_add(1)
                .and_then(|n| i64::try_from(n).ok().map(|n| -n))
                .ok_or_else(err)?;
            Ok((Cbor::Negative(neg), head_len))
        }
        2 => {
            let len: usize = arg.try_into().map_err(|_| err())?;
            if body.len() < len || len > WEBAUTHN_MAX_CEREMONY_BYTES {
                return Err(err());
            }
            Ok((Cbor::Bytes(body[..len].to_vec()), head_len + len))
        }
        3 => {
            let len: usize = arg.try_into().map_err(|_| err())?;
            if body.len() < len || len > WEBAUTHN_MAX_CEREMONY_BYTES {
                return Err(err());
            }
            let text =
                std::str::from_utf8(&body[..len]).map_err(|_| err())?;
            Ok((Cbor::Text(text.to_string()), head_len + len))
        }
        4 => {
            if arg > 64 {
                return Err(err());
            }
            let mut items = Vec::with_capacity(arg as usize);
            let mut off = head_len;
            for _ in 0..arg {
                let (item, used) = cbor_decode(&input[off..], depth + 1)?;
                off += used;
                items.push(item);
            }
            Ok((Cbor::Array(items), off))
        }
        5 => {
            if arg > 64 {
                return Err(err());
            }
            let mut pairs = Vec::with_capacity(arg as usize);
            let mut off = head_len;
            for _ in 0..arg {
                let (key, used) = cbor_decode(&input[off..], depth + 1)?;
                off += used;
                let (value, used) = cbor_decode(&input[off..], depth + 1)?;
                off += used;
                pairs.push((key, value));
            }
            Ok((Cbor::Map(pairs), off))
        }
        _ => Err(err()),
    }
}

fn cbor_map_find<'a>(map: &'a [(Cbor, Cbor)], key: &Cbor) -> Option<&'a Cbor> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Parsed authenticator data (`authData`): the RP hash, flags, sign count,
/// and — when attested data is present — the credential id plus the ES256
/// COSE coordinates.
struct ParsedAuthData {
    rp_id_hash: [u8; 32],
    sign_count: u32,
    credential_id: Option<Vec<u8>>,
    cose_x: Option<[u8; 32]>,
    cose_y: Option<[u8; 32]>,
}

const AUTH_FLAG_USER_PRESENT: u8 = 0x01;
const AUTH_FLAG_ATTESTED: u8 = 0x40;

fn parse_auth_data(bytes: &[u8], expect_attested: bool) -> crate::domain::Result<ParsedAuthData> {
    if bytes.len() < 37 || bytes.len() > WEBAUTHN_MAX_CEREMONY_BYTES {
        return Err(webauthn_err("WebAuthn authenticator data is malformed"));
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&bytes[..32]);
    let flags = bytes[32];
    if flags & AUTH_FLAG_USER_PRESENT == 0 {
        return Err(webauthn_err(
            "WebAuthn ceremony is missing the user-presence flag",
        ));
    }
    let sign_count = u32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]);
    let attested = flags & AUTH_FLAG_ATTESTED != 0;
    if expect_attested && !attested {
        return Err(webauthn_err(
            "WebAuthn registration is missing attested credential data",
        ));
    }
    if !attested {
        return Ok(ParsedAuthData {
            rp_id_hash,
            sign_count,
            credential_id: None,
            cose_x: None,
            cose_y: None,
        });
    }
    // Attested credential data: AAGUID(16) + credIdLen(2) + credId + COSE key.
    if bytes.len() < 55 {
        return Err(webauthn_err("WebAuthn attested data is truncated"));
    }
    let id_len = u16::from_be_bytes([bytes[53], bytes[54]]) as usize;
    if id_len == 0 || id_len > 1023 || bytes.len() < 55 + id_len {
        return Err(webauthn_err("WebAuthn credential id is malformed"));
    }
    let credential_id = bytes[55..55 + id_len].to_vec();
    let (cose, _) = cbor_decode(&bytes[55 + id_len..], 0)?;
    let Cbor::Map(ref pairs) = cose else {
        return Err(webauthn_err("WebAuthn credential key is malformed"));
    };
    // COSE keys use small integer labels: `1` = kty, `3` = alg, `-1` =
    // crv, `-2`/`-3` = x/y. Only EC2/P-256/ES256 verifies here.
    let find_int = |label: Cbor| -> Option<i64> {
        match cbor_map_find(pairs, &label) {
            Some(Cbor::Unsigned(n)) => i64::try_from(*n).ok(),
            Some(Cbor::Negative(n)) => Some(*n),
            _ => None,
        }
    };
    let find_bytes = |label: Cbor| -> Option<Vec<u8>> {
        match cbor_map_find(pairs, &label) {
            Some(Cbor::Bytes(b)) => Some(b.clone()),
            _ => None,
        }
    };
    if find_int(Cbor::Unsigned(1)) != Some(2) {
        return Err(webauthn_err(
            "WebAuthn credential key type is not supported (only P-256)",
        ));
    }
    match find_int(Cbor::Unsigned(3)) {
        Some(alg) if alg == WEBAUTHN_COSE_ES256 => {}
        Some(_) => {
            return Err(webauthn_err(
                "WebAuthn credential algorithm is not supported (only ES256)",
            ));
        }
        None => return Err(webauthn_err("WebAuthn credential key is malformed")),
    }
    if find_int(Cbor::Negative(-1)) != Some(1) {
        return Err(webauthn_err(
            "WebAuthn credential curve is not supported (only P-256)",
        ));
    }
    let x = find_bytes(Cbor::Negative(-2));
    let y = find_bytes(Cbor::Negative(-3));
    let (Some(x), Some(y)) = (x, y) else {
        return Err(webauthn_err("WebAuthn credential key is malformed"));
    };
    if x.len() != 32 || y.len() != 32 {
        return Err(webauthn_err("WebAuthn credential key is malformed"));
    }
    let mut cose_x = [0u8; 32];
    let mut cose_y = [0u8; 32];
    cose_x.copy_from_slice(&x);
    cose_y.copy_from_slice(&y);
    Ok(ParsedAuthData {
        rp_id_hash,
        sign_count,
        credential_id: Some(credential_id),
        cose_x: Some(cose_x),
        cose_y: Some(cose_y),
    })
}

/// Parse and check `clientDataJSON`: the ceremony `type`, the echoed
/// challenge (constant-time against the stored challenge — no oracle), and
/// the exact expected origin.
fn parse_client_data(
    client_data_json: &[u8],
    expected_type: &str,
    expected_challenge_b64: &str,
    expected_origin: &str,
) -> crate::domain::Result<()> {
    if client_data_json.len() > WEBAUTHN_MAX_CEREMONY_BYTES {
        return Err(webauthn_err("WebAuthn client data is too large"));
    }
    let value: serde_json::Value =
        serde_json::from_slice(client_data_json).map_err(|_| {
            webauthn_err("WebAuthn client data is not valid JSON")
        })?;
    if value.get("type").and_then(|v| v.as_str()) != Some(expected_type) {
        return Err(webauthn_err("WebAuthn client data has the wrong type"));
    }
    let challenge = value
        .get("challenge")
        .and_then(|v| v.as_str())
        .ok_or_else(|| webauthn_err("WebAuthn client data is missing its challenge"))?;
    let challenge_ok = constant_time_equal(challenge.as_bytes(), expected_challenge_b64.as_bytes());
    if !challenge_ok {
        return Err(webauthn_err(
            "WebAuthn challenge does not match (unknown, expired, or already used)",
        ));
    }
    if value.get("origin").and_then(|v| v.as_str()) != Some(expected_origin) {
        return Err(webauthn_err("WebAuthn origin does not match this server"));
    }
    Ok(())
}

fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Verify an ES256 signature over `message` with an uncompressed P-256 point.
fn verify_es256(uncompressed_point: &[u8], message: &[u8], signature_der: &[u8]) -> bool {
    if uncompressed_point.len() != 65 || uncompressed_point[0] != 0x04 {
        return false;
    }
    let key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::ECDSA_P256_SHA256_FIXED,
        uncompressed_point,
    );
    key.verify(message, signature_der).is_ok()
}

/// Verify a registration ceremony and return the credential to persist.
///
/// Checks, in order: challenge purpose + expiry, `clientDataJSON`
/// (`webauthn.create`, echoed challenge, origin), attestation format
/// (`none` with an empty statement, or `packed` self-attestation verified
/// against the credential key — `x5c` chains and every other format are
/// refused with reason), RP ID hash, user presence, and the ES256 COSE key.
/// Raw cryptographic failures are `InvalidInput` (client error), never
/// silent acceptance.
pub fn verify_webauthn_registration(
    rp_id: &str,
    origin: &str,
    challenge: &WebauthnChallenge,
    now_ms: u64,
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> crate::domain::Result<ParsedWebauthnCredential> {
    if challenge.purpose != WebauthnPurpose::Registration {
        return Err(webauthn_err(
            "WebAuthn challenge was not minted for registration",
        ));
    }
    if !challenge.live_at(now_ms) {
        return Err(webauthn_err("WebAuthn challenge has expired"));
    }
    parse_client_data(
        client_data_json,
        WebauthnPurpose::Registration.client_data_type(),
        &challenge.challenge_b64,
        origin,
    )?;
    if attestation_object.len() > WEBAUTHN_MAX_CEREMONY_BYTES {
        return Err(webauthn_err("WebAuthn attestation object is too large"));
    }
    let (att_obj, _) = cbor_decode(attestation_object, 0)?;
    let Cbor::Map(ref pairs) = att_obj else {
        return Err(webauthn_err("WebAuthn attestation object is malformed"));
    };
    let fmt = match cbor_map_find(pairs, &Cbor::Text("fmt".to_string())) {
        Some(Cbor::Text(fmt)) => fmt.clone(),
        _ => return Err(webauthn_err("WebAuthn attestation object is malformed")),
    };
    let auth_data_raw: Vec<u8> = match cbor_map_find(pairs, &Cbor::Text("authData".to_string())) {
        Some(Cbor::Bytes(bytes)) => bytes.clone(),
        _ => return Err(webauthn_err("WebAuthn attestation object is malformed")),
    };
    let att_stmt = match cbor_map_find(pairs, &Cbor::Text("attStmt".to_string())) {
        Some(Cbor::Map(stmt)) => stmt.clone(),
        _ => return Err(webauthn_err("WebAuthn attestation object is malformed")),
    };
    let parsed = parse_auth_data(&auth_data_raw, true)?;
    if parsed.rp_id_hash != sha256_of(rp_id.as_bytes()) {
        return Err(webauthn_err(
            "WebAuthn credential relies on a different party",
        ));
    }
    let (x, y) = match (parsed.cose_x, parsed.cose_y) {
        (Some(x), Some(y)) => (x, y),
        _ => return Err(webauthn_err("WebAuthn credential key is malformed")),
    };
    let mut point = vec![0x04u8];
    point.extend_from_slice(&x);
    point.extend_from_slice(&y);
    match fmt.as_str() {
        "none" => {
            if !att_stmt.is_empty() {
                return Err(webauthn_err(
                    "WebAuthn none-attestation must carry an empty statement",
                ));
            }
        }
        "packed" => {
            if cbor_map_find(&att_stmt, &Cbor::Text("x5c".to_string())).is_some()
                || cbor_map_find(&att_stmt, &Cbor::Text("ecdaaKeyId".to_string())).is_some()
            {
                return Err(webauthn_err(
                    "WebAuthn attestation chains are not supported (self-attestation only)",
                ));
            }
            let alg = match cbor_map_find(&att_stmt, &Cbor::Text("alg".to_string())) {
                Some(Cbor::Negative(n)) => *n,
                Some(Cbor::Unsigned(n)) => i64::try_from(*n).unwrap_or(i64::MAX),
                _ => {
                    return Err(webauthn_err(
                        "WebAuthn packed attestation is missing its algorithm",
                    ));
                }
            };
            if alg != WEBAUTHN_COSE_ES256 {
                return Err(webauthn_err(
                    "WebAuthn attestation algorithm is not supported (only ES256)",
                ));
            }
            let sig = match cbor_map_find(&att_stmt, &Cbor::Text("sig".to_string())) {
                Some(Cbor::Bytes(sig)) => sig.clone(),
                _ => {
                    return Err(webauthn_err(
                        "WebAuthn packed attestation is missing its signature",
                    ));
                }
            };
            let client_hash = sha256_of(client_data_json);
            let mut message = auth_data_raw.clone();
            message.extend_from_slice(&client_hash);
            if !verify_es256(&point, &message, &sig) {
                return Err(webauthn_err("WebAuthn attestation signature is invalid"));
            }
        }
        _ => {
            return Err(webauthn_err(format!(
                "WebAuthn attestation format {fmt} is not supported"
            )));
        }
    }
    let credential_id = parsed.credential_id.unwrap_or_default();
    if credential_id.is_empty() || credential_id.len() > 1023 {
        return Err(webauthn_err("WebAuthn credential id is malformed"));
    }
    Ok(ParsedWebauthnCredential {
        credential_id,
        x,
        y,
        sign_count: parsed.sign_count,
    })
}

/// Verify an authentication ceremony against a stored credential.
///
/// Checks the challenge (purpose + expiry + echo), `clientDataJSON`
/// (`webauthn.get`, origin), RP ID hash, user presence, and the ES256
/// signature over `authenticatorData || sha256(clientDataJSON)`. Returns the
/// sign count for the caller to persist. Clone detection: when both the
/// stored and presented counts are non-zero the presented count must
/// strictly increase, otherwise the ceremony is refused.
pub fn verify_webauthn_assertion(
    credential: &WebauthnCredential,
    rp_id: &str,
    origin: &str,
    challenge: &WebauthnChallenge,
    now_ms: u64,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature_der: &[u8],
) -> crate::domain::Result<u32> {
    if challenge.purpose != WebauthnPurpose::Authentication {
        return Err(webauthn_err(
            "WebAuthn challenge was not minted for authentication",
        ));
    }
    if !challenge.live_at(now_ms) {
        return Err(webauthn_err("WebAuthn challenge has expired"));
    }
    if challenge.account_id != credential.account_id {
        return Err(webauthn_err("WebAuthn challenge belongs to another account"));
    }
    parse_client_data(
        client_data_json,
        WebauthnPurpose::Authentication.client_data_type(),
        &challenge.challenge_b64,
        origin,
    )?;
    let parsed = parse_auth_data(authenticator_data, false)?;
    if parsed.rp_id_hash != sha256_of(rp_id.as_bytes()) {
        return Err(webauthn_err(
            "WebAuthn credential relies on a different party",
        ));
    }
    let client_hash = sha256_of(client_data_json);
    let mut message = authenticator_data.to_vec();
    message.extend_from_slice(&client_hash);
    if !verify_es256(&credential.public_key_uncompressed, &message, signature_der) {
        // One generic shape: a bad signature reveals nothing about which
        // field the prover got wrong.
        return Err(webauthn_err("WebAuthn authentication failed"));
    }
    let presented = parsed.sign_count;
    if credential.sign_count != 0 && presented != 0 && presented <= credential.sign_count {
        return Err(webauthn_err(
            "WebAuthn sign count did not increase (the authenticator may be cloned)",
        ));
    }
    Ok(presented)
}

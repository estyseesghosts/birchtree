use crate::domain::Id;

use super::roles::UserRole;

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

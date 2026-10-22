use std::collections::HashMap;

use crate::domain::{Account, AccountStats, EmailToken, Error, Id, LocalUser, Result, UserRole};

/// Repository inputs and results shared by the ports below.

/// Input for creating a local account and its credential together.
pub struct NewLocalAccount {
    pub username: String,
    pub normalized_username: String,
    pub domain: String,
    pub display_name: String,
    pub password_hash: String,
    pub email: Option<String>,
    pub actor_uri: String,
    pub now_ms: u64,
    /// When true, a `SendConfirmationEmail` job is committed in the same
    /// transaction as the account (public registration). The CLI bootstrap
    /// leaves this false.
    pub send_confirmation_email: bool,
    /// Whether the account starts approved. Public registration sets this
    /// `false` when the operator requires approval (the pending queue);
    /// operator-created accounts are always approved.
    pub approved: bool,
    /// OAuth application that performed the signup (`None` for CLI-created
    /// and pre-attribution records). Recorded on the credential row for the
    /// email-confirmation ownership check (D12 remainder).
    pub created_by_application: Option<Id>,
    /// Account locale captured at registration (upstream `user.locale`).
    pub locale: Option<String>,
    /// Invite-request text captured at registration (`reason` param, upstream
    /// `user.invite_request.text`).
    pub invite_request: Option<String>,
    /// Invite code consumed at registration (`None` when no usable invite was
    /// supplied). Recorded in `INVITE_USES` in the same transaction as the
    /// account (backs `invited_by_account_id` + the `invited_by` filter).
    pub invite_code: Option<String>,
    /// IANA time-zone name captured at registration (upstream `user.time_zone`).
    pub time_zone: Option<String>,
    /// Date of birth (`YYYY-MM-DD`, upstream `user.date_of_birth`).
    pub date_of_birth: Option<String>,
}

/// Input for registering an OAuth application.
pub struct NewApp {
    pub name: String,
    pub client_id: String,
    pub client_secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub scopes: Vec<String>,
    pub website: Option<String>,
    pub now_ms: u64,
}

/// Input for issuing an access token (hash only; plaintext stays with caller).
pub struct NewToken {
    pub app_id: Id,
    pub user_id: Option<Id>,
    pub token_hash: String,
    pub scopes: Vec<String>,
    pub now_ms: u64,
    pub expires_at_ms: Option<u64>,
}

/// Input for creating an OAuth authorization code (hash only).
pub struct NewAuthCode {
    pub app_id: Id,
    pub user_id: Id,
    pub code_hash: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub now_ms: u64,
    pub ttl_ms: u64,
}

/// Input for creating a login session.
pub struct NewSession {
    pub user_id: Id,
    pub token_hash: String,
    pub now_ms: u64,
    pub ttl_ms: u64,
}

/// Mutable subset of a user's preferences (`PATCH update_credentials`).
///
/// `None` fields are left unchanged.
#[derive(Debug, Default, Clone)]
pub struct UserSettingsUpdate {
    pub default_privacy: Option<String>,
    pub default_sensitive: Option<bool>,
    pub default_language: Option<String>,
    pub default_quote_policy: Option<String>,
    /// D22 home mention policy (`show`/`collapse`/`filter`; `None` = unchanged).
    pub home_mention_policy: Option<String>,
    /// D21 chosen content languages (`None` = unchanged; `Some`, possibly
    /// empty, = replace; an empty list clears the preference like upstream's
    /// `compact_blank.presence` normalization).
    pub chosen_languages: Option<Vec<String>>,
    /// Reading preferences (`None` = unchanged). Birchtree extension:
    /// upstream sets these only through the web settings UI; here
    /// `update_credentials[source][display_media|expand_spoilers|
    /// autoplay_gifs]` stores them. `display_media` is one of
    /// `default`/`show_all`/`hide_all` (upstream `user_settings.rb:38`).
    pub web_display_media: Option<String>,
    pub web_expand_spoilers: Option<bool>,
    pub web_auto_play_gifs: Option<bool>,
}

/// A staged avatar/header image waiting to be referenced by an account row.
///
/// The bytes were already staged under `key` (blob store, outside the
/// transaction); the profile commit only records the reference.
#[derive(Debug, Clone)]
pub struct ProfileImageData {
    pub key: String,
    pub content_type: String,
}

/// Mutable subset of an account profile (`PATCH update_credentials`).
///
/// `None` fields are left unchanged.
#[derive(Debug, Default, Clone)]
pub struct AccountProfileUpdate {
    pub display_name: Option<String>,
    pub note: Option<String>,
    pub locked: Option<bool>,
    pub discoverable: Option<bool>,
    pub indexable: Option<bool>,
    pub hide_collections: Option<bool>,
    /// Replace the whole profile-field list (`fields_attributes` semantics).
    pub fields: Option<Vec<crate::domain::AccountField>>,
    /// Replace the attribution-domain list.
    pub attribution_domains: Option<Vec<String>>,
    /// Automated account flag (`update_credentials[bot]`).
    pub bot: Option<bool>,
    /// Core extension CORE-L: author's default license (`None` leaves it
    /// unchanged; `Some(None)` clears; `Some(Some(id))` sets; validated
    /// before the store call).
    pub default_license: Option<Option<u8>>,
    /// Avatar slot: `None` = unchanged, `Some(None)` = clear back to the
    /// default, `Some(Some(data))` = reference the staged blob.
    pub avatar: Option<Option<ProfileImageData>>,
    /// Avatar alt text (upstream `avatar_description`, max 150 chars;
    /// length is enforced at the endpoint).
    pub avatar_description: Option<String>,
    /// Header slot, same presence semantics as `avatar`.
    pub header: Option<Option<ProfileImageData>>,
    /// Header alt text (upstream `header_description`, max 150 chars).
    pub header_description: Option<String>,
}

/// Accounts, local users, and the unique handle index.
pub trait AccountStore: Send + Sync {
    /// Create an account + credential + handle index entry + follow-up job(s) in
    /// one atomic transaction.
    fn create_local_account(&self, input: NewLocalAccount) -> Result<Account>;

    fn account_by_id(&self, id: Id) -> Result<Option<Account>>;

    fn account_by_handle(&self, normalized_username: &str, domain: &str)
    -> Result<Option<Account>>;

    fn user_by_account_id(&self, account_id: Id) -> Result<Option<LocalUser>>;

    fn local_user_by_handle(
        &self,
        normalized_username: &str,
        domain: &str,
    ) -> Result<Option<LocalUser>>;

    fn account_count(&self) -> Result<u64>;

    /// Accounts by id in ONE transaction (page loader; missing ids are
    /// omitted). Duplicates in `ids` are read once.
    fn accounts_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Account>>;

    /// Apply a profile patch, returning the updated account.
    ///
    /// The commit also enqueues one `FederationEvent` (`profile_update`) so
    /// followers refetch the actor; the event is a no-op for remote rows
    /// (which never reach this path — they sync through `upsert_remote`).
    fn update_account_profile(
        &self,
        account_id: Id,
        update: AccountProfileUpdate,
        now_ms: u64,
    ) -> Result<Account>;

    /// Apply a preferences patch, returning the updated credential record.
    fn update_user_settings(&self, account_id: Id, update: UserSettingsUpdate)
    -> Result<LocalUser>;

    /// Grant or clear a staff role (`admin`, `moderator`, `none`) in ONE
    /// transaction, returning the updated credential record. `NotFound` when
    /// no local user exists for `account_id` (wrong id or remote account).
    fn set_user_role(&self, account_id: Id, role: UserRole) -> Result<LocalUser>;

    /// Record the account this one moved to (federation `Move`, G-D). Remote
    /// rows included. ONE transaction; `None` clears.
    fn set_moved_to(&self, account_id: Id, moved_to: Option<Id>, now_ms: u64) -> Result<Account>;

    /// Overwrite a REMOTE account's counters from validated collection
    /// totals (federation tail refresh). `None` fields keep their row.
    /// Callers gate on remote rows (local counters are authoritative); the
    /// port itself is a plain upsert.
    fn set_remote_stats(
        &self,
        account_id: Id,
        followers: Option<u64>,
        following: Option<u64>,
        statuses: Option<u64>,
    ) -> Result<AccountStats>;

    /// Record one password sign-in event for the admin `active_users`
    /// telemetry (D39). Called once when a local user completes the browser
    /// password login; API calls on an existing token are not sign-ins.
    /// `ip` is the client IP when the adapter supplied one (backs the admin
    /// `ip`/`ips` fields; upstream `user.ips`).
    fn record_sign_in(
        &self,
        account_id: Id,
        now_ms: u64,
        ip: Option<std::net::IpAddr>,
    ) -> Result<()>;

    /// Invite code consumed at registration, if any (fix round P1/P3; backs
    /// `invited_by_account_id` + the v2 `invited_by` filter). `None` when the
    /// account was not invite-created.
    fn invite_code_for_account(&self, account_id: Id) -> Result<Option<String>>;

    /// Accounts created with an invite issued by `inviter_id`, oldest first,
    /// bounded by `cap` (fix round P1/P3; backs the v2 `invited_by` filter).
    fn accounts_for_inviter(&self, inviter_id: Id, cap: usize) -> Result<Vec<Id>>;

    /// Login IPs for `account_id`, newest first, bounded by `cap`
    /// (fix round P1/P3; backs the admin `ips` field as `(ip, used_at)`
    /// pairs; `ip` is the latest entry).
    fn login_ips_for(&self, account_id: Id, cap: usize) -> Result<Vec<(std::net::IpAddr, u64)>>;

    /// Resolve a local account by email address (normalized lowercase; D12
    /// public forgot-password). `None` for unknown addresses — the caller,
    /// not this method, decides what to reveal (no enumeration oracle here).
    /// Scaffold default refuses loudly; RedbStore overrides.
    fn account_by_email(&self, _normalized_email: &str) -> Result<Option<Account>> {
        Err(Error::Internal(
            "account_by_email is not implemented".into(),
        ))
    }

    /// Flip the email-verified flag (D12 confirmation redemption).
    /// Scaffold default refuses loudly; RedbStore overrides.
    fn set_email_verified(&self, _account_id: Id, _verified: bool) -> Result<LocalUser> {
        Err(Error::Internal(
            "set_email_verified is not implemented".into(),
        ))
    }

    /// Store a pending email token (D12 redemption; single-use bearer).
    /// Scaffold default refuses loudly; RedbStore overrides.
    fn store_email_token(&self, _token: &str, _record: EmailToken) -> Result<()> {
        Err(Error::Internal(
            "store_email_token is not implemented".into(),
        ))
    }

    /// Consume a pending email token: returns the record and deletes the row
    /// (`None` for unknown/already-used tokens). Scaffold default refuses
    /// loudly; RedbStore overrides.
    fn redeem_email_token(&self, _token: &str) -> Result<Option<EmailToken>> {
        Err(Error::Internal(
            "redeem_email_token is not implemented".into(),
        ))
    }

    /// Set a local user's password hash in ONE transaction (D12
    /// password-reset redemption). `NotFound` when no local user exists for
    /// `account_id`. Scaffold default refuses loudly; RedbStore overrides.
    fn set_user_password(&self, _account_id: Id, _password_hash: String) -> Result<LocalUser> {
        Err(Error::Internal(
            "set_user_password is not implemented".into(),
        ))
    }
}

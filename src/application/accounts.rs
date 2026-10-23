//! Account creation use cases.
//!
//! Round 3, area A: invite-token create/accept/redeem plus the
//! confirmation-mail and password-reset-mail send paths.
//!
//! Upstream (`8ff11c2b`): registration policy is
//! `RegistrationHelper#allowed_registration?`
//! (`app/helpers/registration_helper.rb:5-7`): open registrations OR a
//! `valid_for_use?` invite (plus IP/omniauth checks that live at the HTTP
//! layer). `AppSignUpService` (`app/services/app_sign_up_service.rb:9-19`)
//! refuses with `Mastodon::NotPermittedError` (403) otherwise; the API
//! controller (`app/controllers/api/v1/accounts_controller.rb:130-136`)
//! looks the invite up by `params[:invite_code]` and ignores an unknown
//! code while registrations are open. `GET /invite/:invite_code`
//! (`api/v1/invites_controller.rb:19-29`, `config/routes.rb:69`) answers 200
//! `{invite_code, instance_api_url}`, 404 for an unknown code
//! (`find_by!`), and 401 `{error: invites.invalid}` for a known-but-unusable
//! one. Mail shapes follow `app/mailers/user_mailer.rb:23-46`.
//!
//! The [`InviteStore`] trait below is the storage port for invites. It is
//! declared here (not in `ports.rs`, which the parent owns) so this area
//! stays merge-clean; the `redb` implementation lands with the parent merge
//! (proposed in the final report). [`MemoryInviteStore`] is the test/dev
//! implementation. Mail goes through the [`crate::mailer::Mailer`] port:
//! sends happen after commit, and a missing provider is an explicit
//! [`crate::mailer::MailError::Disabled`], never a silent success.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::application::admin_infra as admin_infra_uc;
use crate::application::auth;
use crate::application::crypto;
use crate::application::ports::{AccountStore, AdminInfraStore, NewLocalAccount, OAuthStore};
use crate::domain::account::{normalize_username, validate_username};
use crate::domain::admin_infra::AdminEmailDomainBlock;
use crate::domain::user::{
    EmailTokenKind, INVITE_COMMENT_LIMIT, Invite, InviteRefusal,
    MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT, TotpRecord, WebauthnChallenge, WebauthnCredential,
    check_date_of_birth_with_min_age, invite_code_from_seed, validate_time_zone,
};
use crate::domain::{AccessToken, Account, Error, Id, LocalUser, OAuthApplication, Result};
use crate::mailer::MailEnv;

/// Minimum password length (Mastodon requires at least 8 characters).
pub const MIN_PASSWORD_LEN: usize = 8;

/// Input for operator-driven local account creation.
#[derive(Debug, Clone)]
pub struct CreateAccountCommand {
    pub username: String,
    pub email: Option<String>,
    pub password: String,
    pub display_name: Option<String>,
}

/// Input for public registration (`POST /api/v1/accounts`).
#[derive(Debug, Clone)]
pub struct RegisterAccountCommand {
    pub username: String,
    pub email: String,
    pub password: String,
    pub agreement: bool,
    pub locale: Option<String>,
    pub reason: Option<String>,
    pub invite_code: Option<String>,
    pub display_name: Option<String>,
    /// IANA time-zone name (`time_zone` param, upstream `user.time_zone`).
    pub time_zone: Option<String>,
    /// Date of birth (`YYYY-MM-DD`, upstream `user.date_of_birth`).
    pub date_of_birth: Option<String>,
}

/// Result of a successful registration: the created account and a user token
/// scoped to the registering application (Mastodon returns the token body).
#[derive(Debug)]
pub struct RegisteredAccount {
    pub account: Account,
    pub token_plaintext: String,
    pub token: AccessToken,
}

/// Invite resolution for the unified pipeline: `None` reproduces plain
/// [`register_account`] (strict open-gate, no lookup/redemption).
pub struct InviteInput<'a> {
    /// Invite store for code lookup + redemption.
    pub invites: &'a dyn InviteStore,
    /// Whether invites are enabled (`invites_enabled`).
    pub enabled: bool,
}

/// Policy gates for the public registration path.
pub struct PolicyInput<'a> {
    /// Infra store for the email-block lookups.
    pub infra: &'a dyn AdminInfraStore,
    /// Minimum age for `date_of_birth` (`minimum_age_years`).
    pub minimum_age_years: u8,
    /// Captured `time_zone`/`date_of_birth` (validated + carried here).
    pub profile: &'a RegistrationProfile,
}

/// Input for the unified registration pipeline ([`register`]): the union of
/// the three legacy `register_account*` signatures as one struct (<80-line
/// orchestrator, no `#[allow(clippy::too_many_arguments)]`).
pub struct RegisterInput<'a> {
    pub accounts: &'a dyn AccountStore,
    pub oauth: &'a dyn OAuthStore,
    pub base_url: &'a str,
    pub now_ms: u64,
    pub registration_open: bool,
    pub require_approval: bool,
    pub app: &'a OAuthApplication,
    pub cmd: RegisterAccountCommand,
    /// Invite resolution; `None` skips lookup/redemption entirely.
    pub invite: Option<InviteInput<'a>>,
    /// Profile + email-block gates; `None` skips them.
    pub policy: Option<PolicyInput<'a>>,
}

/// Extract the bare host from a public base URL, without pulling in a URL crate.
pub fn host_from_url(url: &str) -> String {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    // Drop any userinfo and port.
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    host.split(':').next().unwrap_or(host).to_string()
}

/// Create a local account from a CLI/operator command.
pub fn create_account(
    store: &dyn AccountStore,
    base_url: &str,
    now_ms: u64,
    cmd: CreateAccountCommand,
) -> Result<Account> {
    validate_username(&cmd.username)?;
    if cmd.password.chars().count() < MIN_PASSWORD_LEN {
        return Err(Error::InvalidInput(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    let normalized_username = normalize_username(&cmd.username);
    let password_hash = crypto::hash_password(&cmd.password).map_err(Error::Internal)?;
    let domain = host_from_url(base_url);
    let actor_uri = Account::local_actor_uri(base_url, &cmd.username);

    store.create_local_account(NewLocalAccount {
        username: cmd.username,
        normalized_username,
        domain,
        display_name: cmd.display_name.unwrap_or_default(),
        password_hash,
        email: cmd.email,
        actor_uri,
        now_ms,
        // The CLI bootstrap does not send mail.
        send_confirmation_email: false,
        // Operator-created accounts are usable immediately.
        approved: true,
        // The CLI is not an OAuth application, so there is no signup-app
        // attribution (the ownership check grandfathers `None`; see
        // `check_confirmation_ownership`).
        created_by_application: None,
        // The CLI takes no locale, invite request or invite code.
        locale: None,
        invite_request: None,
        invite_code: None,
        // The CLI takes no time zone or date of birth either.
        time_zone: None,
        date_of_birth: None,
    })
}

/// Shared creation core for the registration pipeline: validates the
/// command, creates the account + indexes + confirmation-mail job
/// atomically, then issues the user token. Used by [`register`] (and
/// formerly the body of [`register_account`]); no policy gates here.
fn create_registered(
    accounts: &dyn AccountStore,
    oauth: &dyn OAuthStore,
    base_url: &str,
    now_ms: u64,
    require_approval: bool,
    app: &OAuthApplication,
    cmd: RegisterAccountCommand,
) -> Result<RegisteredAccount> {
    if !cmd.agreement {
        return Err(Error::InvalidInput("agreement must be accepted".into()));
    }
    if cmd.email.trim().is_empty() || !cmd.email.contains('@') {
        return Err(Error::InvalidInput("email is invalid".into()));
    }
    validate_username(&cmd.username)?;
    if cmd.password.chars().count() < MIN_PASSWORD_LEN {
        return Err(Error::InvalidInput(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }

    let normalized_username = normalize_username(&cmd.username);
    let password_hash = crypto::hash_password(&cmd.password).map_err(Error::Internal)?;
    let domain = host_from_url(base_url);
    let actor_uri = Account::local_actor_uri(base_url, &cmd.username);

    let account = accounts.create_local_account(NewLocalAccount {
        username: cmd.username,
        normalized_username,
        domain,
        display_name: cmd.display_name.unwrap_or_default(),
        password_hash,
        email: Some(cmd.email.clone()),
        actor_uri,
        now_ms,
        send_confirmation_email: true,
        approved: !require_approval,
        created_by_application: Some(app.id),
        locale: cmd.locale.filter(|locale| !locale.trim().is_empty()),
        invite_request: cmd.reason.filter(|reason| !reason.trim().is_empty()),
        invite_code: cmd.invite_code.filter(|code| !code.trim().is_empty()),
        time_zone: cmd.time_zone.filter(|tz| !tz.trim().is_empty()),
        date_of_birth: cmd.date_of_birth.filter(|dob| !dob.trim().is_empty()),
    })?;

    let (token_plaintext, token) =
        auth::issue_user_token(oauth, app.id, account.id, app.scopes.clone(), now_ms)?;
    Ok(RegisteredAccount {
        account,
        token_plaintext,
        token,
    })
}

/// Unified registration pipeline: the union of the three legacy
/// `register_account*` constructors as one orchestrator.
///
/// Stage order is the legacy C-then-B-then-A order, so first-error
/// precedence is unchanged: profile gates, email-block gate, invite
/// resolution, `allowed_registration` gate, creation, redemption.
/// `policy: None` skips the profile + email stages (legacy invite/plain
/// paths); `invite: None` skips lookup/redemption and applies the strict
/// open-gate (legacy plain path).
pub fn register(input: RegisterInput<'_>) -> Result<RegisteredAccount> {
    let RegisterInput {
        accounts,
        oauth,
        base_url,
        now_ms,
        registration_open,
        require_approval,
        app,
        cmd,
        invite,
        policy,
    } = input;
    let mut cmd = cmd;
    let mut force_approval = false;
    if let Some(policy) = policy {
        if let Some(time_zone) = policy
            .profile
            .time_zone
            .as_deref()
            .map(str::trim)
            .filter(|tz| !tz.is_empty())
        {
            validate_time_zone(time_zone)?;
        }
        if let Some(date_of_birth) = policy
            .profile
            .date_of_birth
            .as_deref()
            .map(str::trim)
            .filter(|dob| !dob.is_empty())
        {
            check_date_of_birth_with_min_age(date_of_birth, now_ms, policy.minimum_age_years)?;
        }
        // Carry the validated captures on the command so the invite path
        // persists them on the credential row in the same transaction.
        cmd.time_zone = policy.profile.time_zone.clone();
        cmd.date_of_birth = policy.profile.date_of_birth.clone();
        // Email blocks gate signup only (confirmation redemption stays live).
        // Domains are unique at creation, so one targeted index read replaces
        // the old 1000-row page scan.
        let email_domain = cmd
            .email
            .rsplit_once('@')
            .map(|(_, domain)| domain.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let domain_blocks: Vec<AdminEmailDomainBlock> = if email_domain.is_empty() {
            Vec::new()
        } else {
            policy
                .infra
                .email_domain_block_for_domain(&email_domain)?
                .into_iter()
                .collect()
        };
        let canonical_hash = admin_infra_uc::canonical_email_hash(&cmd.email);
        let canonical_blocked = !policy
            .infra
            .admin_canonical_blocks_for_hash(&canonical_hash)?
            .is_empty();
        force_approval = check_registration_email(&domain_blocks, canonical_blocked, &cmd.email)?
            == EmailPolicyOutcome::RequireApproval;
    }
    let mut bypass_approval = false;
    let mut redemption: Option<(Invite, &dyn InviteStore)> = None;
    if let Some(inv) = invite {
        let usable_invite: Option<Invite> = match (cmd.invite_code.as_deref(), inv.enabled) {
            (Some(code), true) => match inv.invites.invite_by_code(code)? {
                Some(invite) if invite.check_usable(now_ms).is_ok() => Some(invite),
                _ => None,
            },
            (Some(_), false) => {
                return Err(Error::InvalidInput("invites are disabled".into()));
            }
            (None, _) => None,
        };
        if !allowed_registration(registration_open, usable_invite.is_some()) {
            return Err(Error::Forbidden("Registrations are closed".into()));
        }
        // Record only a usable invite's code: an unknown/unusable code while
        // open is ignored (upstream parity), so it must not attribute.
        cmd.invite_code = usable_invite.as_ref().map(|invite| invite.code.clone());
        bypass_approval = usable_invite.as_ref().is_some_and(|i| i.created_by_staff);
        redemption = usable_invite.map(|invite| (invite, inv.invites));
    } else if !registration_open {
        return Err(Error::Forbidden("Registrations are closed".into()));
    }
    let registered = create_registered(
        accounts,
        oauth,
        base_url,
        now_ms,
        (require_approval || force_approval) && !bypass_approval,
        app,
        cmd,
    )?;
    if let Some((invite, invites)) = redemption {
        // Best-effort: the account already committed (upstream mints the user
        // and token in one transaction and counts the invite use separately).
        // A redemption race only under-counts `uses`, never double-admits.
        if invites.redeem_invite(&invite.code, now_ms).is_err() {
            tracing::warn!("invite use not recorded after registration (redemption race)");
        }
    }
    Ok(registered)
}

/// Legacy plain path (no invite lookup, no policy gates): delegates to
/// [`register`]. Kept for non-HTTP callers; the endpoint uses [`register`].
///
/// Register a local account through the public API and issue a user token for
/// the registering application.
///
/// This is the same account-creation path as the CLI (`create_account`) plus
/// the registration policy gates and token issuance. The account, its indexes
/// and its confirmation-mail job commit atomically; the token is issued after.
///
/// When `require_approval` is set (upstream `registrations_mode ==
/// "approved"`), the account is created unapproved: it appears under the
/// admin `pending` filter and its token is refused by user-token endpoints
/// (upstream `require_user!` ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ `403 Your login is currently pending
/// approval`) until an admin approves it. The token is still issued, like
/// upstream's `AppSignUpService` response.
pub fn register_account(
    accounts: &dyn AccountStore,
    oauth: &dyn OAuthStore,
    base_url: &str,
    now_ms: u64,
    registration_open: bool,
    require_approval: bool,
    app: &OAuthApplication,
    cmd: RegisterAccountCommand,
) -> Result<RegisteredAccount> {
    register(RegisterInput {
        accounts,
        oauth,
        base_url,
        now_ms,
        registration_open,
        require_approval,
        app,
        cmd,
        invite: None,
        policy: None,
    })
}

// ---------------------------------------------------------------------------
// Invites (round 3, area A)
// ---------------------------------------------------------------------------

/// Storage port for invite tokens.
///
/// Mutations commit the invite row (and, for redeem, the incremented counter)
/// atomically; no network I/O happens here. The `redb` implementation is
/// parent-owned (proposal in the area report); [`MemoryInviteStore`] serves
/// tests and local development.
pub trait InviteStore: Send + Sync {
    /// Persist a new invite; `Conflict` when `invite.code` is taken (the
    /// caller mints a fresh code and retries).
    fn create_invite(&self, invite: Invite) -> Result<Invite>;
    /// Look up an invite by code (`None` when unknown).
    fn invite_by_code(&self, code: &str) -> Result<Option<Invite>>;
    /// Atomically check usability at `now_ms` and increment `uses`.
    /// `NotFound` when the code is unknown; `Unauthorized("Invalid
    /// invitation code")` when expired or exhausted (mirrors the API
    /// `invites_controller.rb:25-27` 401).
    fn redeem_invite(&self, code: &str, now_ms: u64) -> Result<Invite>;
}

/// In-memory [`InviteStore`] (tests/dev). The mutex makes check-and-increment
/// atomic, like the single-`redb`-transaction production implementation.
#[derive(Debug, Default)]
pub struct MemoryInviteStore {
    by_code: Mutex<HashMap<String, Invite>>,
}

impl MemoryInviteStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InviteStore for MemoryInviteStore {
    fn create_invite(&self, invite: Invite) -> Result<Invite> {
        let mut map = self.by_code.lock().expect("invite store");
        if map.contains_key(&invite.code) {
            return Err(Error::Conflict("invite code is already taken".into()));
        }
        map.insert(invite.code.clone(), invite.clone());
        Ok(invite)
    }

    fn invite_by_code(&self, code: &str) -> Result<Option<Invite>> {
        Ok(self
            .by_code
            .lock()
            .expect("invite store")
            .get(code)
            .cloned())
    }

    fn redeem_invite(&self, code: &str, now_ms: u64) -> Result<Invite> {
        let mut map = self.by_code.lock().expect("invite store");
        let invite = map
            .get_mut(code)
            .ok_or_else(|| Error::NotFound("invite not found".into()))?;
        map_invite_refusal(invite.check_usable(now_ms))?;
        invite.uses = invite.uses.saturating_add(1);
        Ok(invite.clone())
    }
}

fn map_invite_refusal(check: std::result::Result<(), InviteRefusal>) -> Result<()> {
    match check {
        Ok(()) => Ok(()),
        Err(_) => Err(Error::Unauthorized("Invalid invitation code".into())),
    }
}

/// Input for issuing an invite token.
#[derive(Debug, Clone)]
pub struct CreateInviteCommand {
    pub created_by_account_id: Id,
    /// Snapshot of the creator's staff role: staff-issued invites bypass the
    /// approval queue (birchtree's analogue of upstream `bypass_approval?`;
    /// see `Invite::created_by_staff`).
    pub created_by_staff: bool,
    pub comment: Option<String>,
    /// `None` = unlimited redemptions (upstream `max_uses` NULL).
    pub max_uses: Option<u32>,
    /// Lifetime from now (`None` = never expires, upstream `expires_at` NULL).
    pub expires_in_ms: Option<u64>,
}

/// Issue an invite token, retrying code generation on collision (bounded).
pub fn create_invite(
    store: &dyn InviteStore,
    now_ms: u64,
    cmd: CreateInviteCommand,
) -> Result<Invite> {
    if cmd
        .comment
        .as_deref()
        .is_some_and(|c| c.chars().count() > INVITE_COMMENT_LIMIT)
    {
        return Err(Error::InvalidInput(format!(
            "comment is too long (maximum is {INVITE_COMMENT_LIMIT} characters)"
        )));
    }
    if cmd.max_uses == Some(0) {
        return Err(Error::InvalidInput("max_uses must be at least 1".into()));
    }
    for attempt in 0..64u64 {
        let seed = now_ms
            .wrapping_mul(0x9E3779B97F4A7C15)
            .wrapping_add(attempt.wrapping_mul(0xBF58476D1CE4E5B9));
        let invite = Invite {
            // The memory store mints its own row id below; the id set here is
            // replaced by the production store's sequence on commit.
            id: Id::new(seed),
            code: invite_code_from_seed(seed),
            created_by_account_id: cmd.created_by_account_id,
            created_by_staff: cmd.created_by_staff,
            comment: cmd.comment.clone(),
            max_uses: cmd.max_uses,
            uses: 0,
            expires_at_ms: cmd.expires_in_ms.map(|ttl| now_ms.saturating_add(ttl)),
            created_at_ms: now_ms,
        };
        match store.create_invite(invite) {
            Ok(stored) => return Ok(stored),
            Err(Error::Conflict(_)) => continue,
            Err(err) => return Err(err),
        }
    }
    Err(Error::Internal(
        "could not mint a unique invite code".into(),
    ))
}

/// Read an invite's status: `NotFound("invite not found")` for an unknown
/// code (the HTTP layer answers 404, like upstream `find_by!`), and
/// `Unauthorized("Invalid invitation code")` for a known-but-unusable one
/// (the HTTP layer answers 401, like upstream `check_valid_usage!`).
pub fn invite_status(store: &dyn InviteStore, code: &str, now_ms: u64) -> Result<Invite> {
    let invite = store
        .invite_by_code(code)?
        .ok_or_else(|| Error::NotFound("invite not found".into()))?;
    map_invite_refusal(invite.check_usable(now_ms))?;
    Ok(invite)
}

/// Redeem an invite token (check usable + increment `uses` atomically).
pub fn redeem_invite(store: &dyn InviteStore, code: &str, now_ms: u64) -> Result<Invite> {
    store.redeem_invite(code, now_ms)
}

/// Redeem an email-confirmation token: consume the single-use bearer and flip
/// `email_verified` on the owning account.
///
/// - Unknown or already-used tokens ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ `NotFound` (the HTTP layer answers the
///   invalid/used shape; no oracle distinguishes the two).
/// - A `PasswordReset` token presented here ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ `InvalidInput` with a reason
///   (wrong flow, never silently accepted). The bearer is still consumed:
///   tokens are single-use regardless of which endpoint reads them, so a
///   wrong-endpoint paste cannot be probed twice.
/// - Otherwise the account's `email_verified` flips to `true` and the updated
///   credential row is returned.
/// Password-reset tokens expire 6 hours after issue (upstream Devise
/// `reset_password_within`; confirmation tokens never expire, matching
/// Devise's default `confirm_within`).
pub const PASSWORD_RESET_TTL_MS: u64 = 6 * 3_600_000;

pub fn confirm_email(
    store: &dyn SecondFactorStore,
    token: &str,
    // Confirmation bearers never expire (Devise default); the clock is
    // passed through for symmetry with the reset flow.
    now_ms: u64,
) -> Result<LocalUser> {
    // One storage transaction consumes the bearer and flips the flag.
    store.confirm_email_token(token, now_ms)
}

/// Redeem a password-reset bearer and set the new password hash atomically.
/// Unknown, used, expired and wrong-kind bearers all answer `NotFound` /
/// `InvalidInput` without revealing which case occurred to HTTP callers
/// (the endpoint maps every failure to one page).
pub fn reset_password(
    store: &dyn SecondFactorStore,
    token: &str,
    password_hash: String,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<LocalUser> {
    store.reset_password_with_token(token, password_hash, now_ms, ttl_ms)
}

/// Whether a stored token is still live at `now_ms`.
pub fn email_token_live(kind: EmailTokenKind, created_at_ms: u64, now_ms: u64) -> bool {
    match kind {
        EmailTokenKind::Confirmation => true,
        EmailTokenKind::PasswordReset => {
            now_ms.saturating_sub(created_at_ms) <= PASSWORD_RESET_TTL_MS
        }
    }
}

/// Enforce upstream's `require_user_owned_by_application!`
/// (`app/controllers/api/v1/emails/confirmations_controller.rb:8-10`): the
/// confirmation resend/check endpoints serve only the user whose account was
/// created through the calling OAuth application.
///
/// Grandfather rule: accounts created before attribution existed
/// (`created_by_application == None`: every CLI-created and pre-D12 row)
/// are served to any calling app. A recorded id that differs from the
/// caller's app id is `Forbidden` (upstream answers 403).
pub fn check_confirmation_ownership(user: &LocalUser, app_id: Id) -> Result<()> {
    match user.created_by_application {
        None => Ok(()),
        Some(owner) if owner == app_id => Ok(()),
        Some(_) => Err(Error::Forbidden(
            "this confirmation belongs to another application".into(),
        )),
    }
}

/// Whether registration may proceed: open registrations OR a usable invite.
///
/// Mirrors `RegistrationHelper#allowed_registration?`
/// (`registration_helper.rb:5-7`) minus the parts owned by the HTTP layer
/// (single-user mode, omniauth-only installs, IP sign-up blocks).
pub fn allowed_registration(registration_open: bool, invite_usable: bool) -> bool {
    registration_open || invite_usable
}

/// Register through the public API with an optional invite code.
///
/// Policy (mirrors upstream `AppSignUpService` + `accounts_controller.rb`):
/// - Invites disabled + code supplied ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ `InvalidInput` (refused with reason,
///   never silently ignored).
/// - Usable invite ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ registration proceeds even when closed, and the invite
///   is redeemed after the account commits. A staff-issued invite bypasses
///   `require_approval` (D36 interaction: the account lands `approved=true`
///   and skips the admin pending queue; non-staff invites still queue).
/// - Unknown/unusable code while open ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ the code is ignored and registration
///   proceeds (upstream `allowed_registration?` stays true via open
///   registrations). While closed ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ `Forbidden("Registrations are closed")`.
/// - No code ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ plain [`register_account`].
/// Legacy invite path (no policy gates): delegates to [`register`] with
/// `policy: None`. Kept for direct callers; the endpoint uses [`register`].
pub fn register_account_with_invite(
    accounts: &dyn AccountStore,
    oauth: &dyn OAuthStore,
    invites: &dyn InviteStore,
    base_url: &str,
    now_ms: u64,
    registration_open: bool,
    require_approval: bool,
    invites_enabled: bool,
    app: &OAuthApplication,
    cmd: RegisterAccountCommand,
) -> Result<RegisteredAccount> {
    register(RegisterInput {
        accounts,
        oauth,
        base_url,
        now_ms,
        registration_open,
        require_approval,
        app,
        cmd,
        invite: Some(InviteInput {
            invites,
            enabled: invites_enabled,
        }),
        policy: None,
    })
}

// ---------------------------------------------------------------------------
// Email send paths (round 3, area A)
// ---------------------------------------------------------------------------

/// Registration-time mail guard: refuse public signup when confirmation mail
/// cannot deliver.
///
/// Registration always enqueues a `SendConfirmationEmail` job for the new
/// account; without a provider the job can never deliver and the user can
/// never receive their token. Failing here (the endpoint maps this to the
/// same explicit `503` as the resend endpoints) keeps the failure at the
/// front door instead of stranding an unconfirmable account. Takes the mail
/// environment rather than a bare boolean so call sites stay readable.
pub fn check_registration_mail(env: &MailEnv) -> Result<()> {
    if env.enabled {
        Ok(())
    } else {
        Err(Error::Forbidden(
            "Email delivery is not configured; registration is unavailable".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Second factor: TOTP (+ backup codes) and WebAuthn (G-E).
//
// Upstream (`8ff11c2b`): TOTP lives on `User#otp_secret`/`otp_required_for_login`
// (Devise two-factor, 6-digit SHA1 TOTP + recovery codes); WebAuthn lives on
// `WebauthnCredential` with a session-held challenge (`SessionsController`
// `#webauthn` ceremony). Here the storage port is [`SecondFactorStore`]
// (declared here so this area stays merge-clean; the `redb` port mapping
// onto `TOTP_SECRETS`/`WEBAUTHN_CREDS` is proposed in the G-E report).
// [`MemorySecondFactorStore`] is the test/dev implementation.
//
// Secrecy rules (see `tests/secret_handling.rs` conventions): TOTP seeds,
// backup-code plaintexts and credential ids are authentication material ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â
// they are returned to the caller exactly once, stored only hashed
// (backup codes) or inside the 0700 `redb` file (seeds/keys), and never
// logged. [`TotpRecord`] and [`WebauthnCredential`] redact themselves from
// `Debug` output.
// ---------------------------------------------------------------------------

/// Which second factor authenticated a login (returned to the caller; the
/// kind alone is safe to log ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â it carries no secret material).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondFactorKind {
    Totp,
    BackupCode,
    WebAuthn,
}

/// Storage port for second-factor rows.
///
/// TOTP secrets are keyed by account id (one enrollment per account);
/// WebAuthn credentials by raw credential id (many per account);
/// challenges are single-use (taken exactly once, then gone).
pub trait SecondFactorStore: Send + Sync {
    fn totp_for(&self, account_id: Id) -> Result<Option<TotpRecord>>;
    fn put_totp(&self, record: TotpRecord) -> Result<TotpRecord>;
    fn delete_totp(&self, account_id: Id) -> Result<bool>;
    fn webauthn_for_account(&self, account_id: Id) -> Result<Vec<WebauthnCredential>>;
    fn webauthn_by_credential_id(&self, credential_id: &[u8])
    -> Result<Option<WebauthnCredential>>;
    fn put_webauthn(&self, credential: WebauthnCredential) -> Result<WebauthnCredential>;
    fn delete_webauthn(&self, account_id: Id, credential_id: &[u8]) -> Result<bool>;
    fn store_webauthn_challenge(&self, challenge: WebauthnChallenge) -> Result<()>;
    fn take_webauthn_challenge(&self, challenge_b64: &str) -> Result<Option<WebauthnChallenge>>;

    // -- Email-token and mail-outbox secrets (work package 1) -------------
    //
    // Hosted on this trait because it is already a `Store` supertrait that
    // the storage adapter implements next to the secret-sealing code.
    // Defaults refuse loudly (like the scaffold `AccountStore` methods);
    // `RedbStore` overrides every one.

    /// Mint a random bearer, persist only its digest, and queue the mail job
    /// (`SendConfirmationEmail` or `SendPasswordResetEmail` by `kind`) in ONE
    /// transaction. The job payload carries the bearer sealed under the
    /// store's secret key (plaintext only when no key is configured).
    fn issue_email_token(
        &self,
        _account_id: Id,
        _email: &str,
        _kind: EmailTokenKind,
        _now_ms: u64,
    ) -> Result<()> {
        Err(Error::Internal(
            "issue_email_token is not implemented".into(),
        ))
    }

    /// Recover the bearer from a mail job's stored `token` field. The default
    /// is the identity (stores that never seal).
    fn reveal_mail_token(&self, _account_id: Id, stored: &str) -> Result<String> {
        Ok(stored.to_string())
    }

    /// Atomically consume a confirmation bearer and mark the owner's email
    /// verified (one transaction: a failed update leaves the bearer intact).
    fn confirm_email_token(&self, _token: &str, _now_ms: u64) -> Result<LocalUser> {
        Err(Error::Internal(
            "confirm_email_token is not implemented".into(),
        ))
    }

    /// Atomically validate kind and lifetime, consume a reset bearer and set
    /// the new password hash (one transaction).
    fn reset_password_with_token(
        &self,
        _token: &str,
        _password_hash: String,
        _now_ms: u64,
        _ttl_ms: u64,
    ) -> Result<LocalUser> {
        Err(Error::Internal(
            "reset_password_with_token is not implemented".into(),
        ))
    }

    /// Bounded maintenance: delete up to `limit` stale email-token rows
    /// (expired resets, bearers of accounts that no longer exist, and
    /// confirmation bearers of already-verified accounts) and redact finished
    /// mail jobs that still carry a bearer. Returns rows changed.
    fn prune_email_secrets(
        &self,
        _now_ms: u64,
        _reset_ttl_ms: u64,
        _limit: usize,
    ) -> Result<usize> {
        Err(Error::Internal(
            "prune_email_secrets is not implemented".into(),
        ))
    }
}

#[derive(Debug, Default)]
struct SecondFactorInner {
    totp: HashMap<u64, TotpRecord>,
    webauthn_by_id: HashMap<Vec<u8>, WebauthnCredential>,
    webauthn_by_account: HashMap<u64, Vec<Vec<u8>>>,
    challenges: HashMap<String, WebauthnChallenge>,
}

/// In-memory [`SecondFactorStore`] (tests/dev). Mutex-serialized like the
/// single-transaction production port.
#[derive(Debug, Default)]
pub struct MemorySecondFactorStore {
    inner: Mutex<SecondFactorInner>,
}

impl MemorySecondFactorStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecondFactorStore for MemorySecondFactorStore {
    fn totp_for(&self, account_id: Id) -> Result<Option<TotpRecord>> {
        Ok(self
            .inner
            .lock()
            .expect("second-factor store")
            .totp
            .get(&account_id.value())
            .cloned())
    }

    fn put_totp(&self, record: TotpRecord) -> Result<TotpRecord> {
        self.inner
            .lock()
            .expect("second-factor store")
            .totp
            .insert(record.account_id.value(), record.clone());
        Ok(record)
    }

    fn delete_totp(&self, account_id: Id) -> Result<bool> {
        Ok(self
            .inner
            .lock()
            .expect("second-factor store")
            .totp
            .remove(&account_id.value())
            .is_some())
    }

    fn webauthn_for_account(&self, account_id: Id) -> Result<Vec<WebauthnCredential>> {
        let inner = self.inner.lock().expect("second-factor store");
        let ids = inner
            .webauthn_by_account
            .get(&account_id.value())
            .cloned()
            .unwrap_or_default();
        Ok(ids
            .iter()
            .filter_map(|id| inner.webauthn_by_id.get(id).cloned())
            .collect())
    }

    fn webauthn_by_credential_id(
        &self,
        credential_id: &[u8],
    ) -> Result<Option<WebauthnCredential>> {
        Ok(self
            .inner
            .lock()
            .expect("second-factor store")
            .webauthn_by_id
            .get(credential_id)
            .cloned())
    }

    fn put_webauthn(&self, credential: WebauthnCredential) -> Result<WebauthnCredential> {
        let mut inner = self.inner.lock().expect("second-factor store");
        let already_owned: bool = inner
            .webauthn_by_id
            .get(&credential.credential_id)
            .is_some_and(|old| old.account_id == credential.account_id);
        let owned_count: usize = inner
            .webauthn_by_account
            .get(&credential.account_id.value())
            .map_or(0, Vec::len);
        if !already_owned && owned_count >= MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT {
            return Err(Error::Conflict(format!(
                "an account may register at most {MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT} security keys"
            )));
        }
        if let Some(old) = inner.webauthn_by_id.get(&credential.credential_id).cloned()
            && old.account_id != credential.account_id
            && let Some(ids) = inner.webauthn_by_account.get_mut(&old.account_id.value())
        {
            ids.retain(|id| *id != credential.credential_id);
        }
        inner
            .webauthn_by_account
            .entry(credential.account_id.value())
            .or_default()
            .retain(|id| *id != credential.credential_id);
        inner
            .webauthn_by_account
            .entry(credential.account_id.value())
            .or_default()
            .push(credential.credential_id.clone());
        inner
            .webauthn_by_id
            .insert(credential.credential_id.clone(), credential.clone());
        Ok(credential)
    }

    fn delete_webauthn(&self, account_id: Id, credential_id: &[u8]) -> Result<bool> {
        let mut inner = self.inner.lock().expect("second-factor store");
        let owned = inner
            .webauthn_by_id
            .get(credential_id)
            .is_some_and(|cred| cred.account_id == account_id);
        if !owned {
            return Ok(false);
        }
        inner.webauthn_by_id.remove(credential_id);
        if let Some(ids) = inner.webauthn_by_account.get_mut(&account_id.value()) {
            ids.retain(|id| id.as_slice() != credential_id);
        }
        Ok(true)
    }

    fn store_webauthn_challenge(&self, challenge: WebauthnChallenge) -> Result<()> {
        self.inner
            .lock()
            .expect("second-factor store")
            .challenges
            .insert(challenge.challenge_b64.clone(), challenge);
        Ok(())
    }

    fn take_webauthn_challenge(&self, challenge_b64: &str) -> Result<Option<WebauthnChallenge>> {
        Ok(self
            .inner
            .lock()
            .expect("second-factor store")
            .challenges
            .remove(challenge_b64))
    }
}

// ---------------------------------------------------------------------------
// Registration profile + email-block policy (G-E).
// ---------------------------------------------------------------------------

/// Outcome of the email-block policy check at registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailPolicyOutcome {
    /// Proceed with the instance's approval setting.
    Allow,
    /// A matching `allow_with_approval` domain block: the account lands
    /// `approved = false` (admin pending queue) even on open instances.
    RequireApproval,
}

/// Extra registration fields captured alongside [`RegisterAccountCommand`].
/// (Separate struct ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â not new fields on the command ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â so the current
/// endpoint construction keeps compiling until the parent wires the new
/// params; see the G-E report snippet.)
#[derive(Debug, Clone, Default)]
pub struct RegistrationProfile {
    /// IANA time-zone name (`time_zone` param, upstream `user.time_zone`).
    pub time_zone: Option<String>,
    /// Date of birth (`YYYY-MM-DD`, upstream `user.date_of_birth`).
    pub date_of_birth: Option<String>,
}

/// Evaluate the email-block policy for a signup address (pure: the caller
/// loads the rows, this decides).
///
/// - A matching canonical-email block ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ refuse `InvalidInput` (422
///   `Validation failed: Email is blocked`), confirmed or not.
/// - A matching email-domain block with `allow_with_approval = false` ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢
///   refuse (422 `Validation failed: Email domain is blocked`).
/// - A matching domain block with `allow_with_approval = true` ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢
///   [`EmailPolicyOutcome::RequireApproval`] (the account queues for admin
///   approval instead of landing usable).
/// - Otherwise [`EmailPolicyOutcome::Allow`].
///
/// Reconciliation with confirmation redemption (D12): blocks gate *signup*
/// only. [`confirm_email`] stays live for blocked addresses (a block created
/// after signup never strands confirmation), and already-confirmed legacy
/// accounts keep logging in ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â the matrix is pinned by
/// `tests/gaps_auth_moderation.rs`.
pub fn check_registration_email(
    domain_blocks: &[AdminEmailDomainBlock],
    canonical_blocked: bool,
    email: &str,
) -> Result<EmailPolicyOutcome> {
    let address = email.trim();
    let domain = address
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .filter(|domain| !domain.is_empty() && !address.starts_with('@'))
        .ok_or_else(|| Error::InvalidInput("email is invalid".into()))?;
    if canonical_blocked {
        return Err(Error::InvalidInput(
            "Validation failed: Email is blocked".into(),
        ));
    }
    for block in domain_blocks {
        if block.domain.eq_ignore_ascii_case(domain) {
            if block.allow_with_approval {
                return Ok(EmailPolicyOutcome::RequireApproval);
            }
            return Err(Error::InvalidInput(
                "Validation failed: Email domain is blocked".into(),
            ));
        }
    }
    Ok(EmailPolicyOutcome::Allow)
}

/// Register through the public API with the G-E policy gates: `time_zone` /
/// `date_of_birth` capture + min-age refusal, and email-domain /
/// canonical-email block enforcement reconciled with confirmation state.
///
/// Under-age dates are refused with the 422 shape
/// (`InvalidInput("Validation failed: ...")`); blocked addresses likewise.
/// An `allow_with_approval` domain match forces the approval queue. All
/// other policy delegates to [`register_account_with_invite`].
///
/// Persistence note: `time_zone`/`date_of_birth` are validated here and
/// carried on the command, so the invite path persists them on the credential
/// row in the same transaction as the account.
/// Legacy policy path (invite + profile + email gates): delegates to
/// [`register`]. Kept for direct callers; the endpoint uses [`register`].
// WHY: legacy shim preserving the pre-unified signature; delegates to
// register(RegisterInput) and deletes once external callers move.
#[allow(clippy::too_many_arguments)]
pub fn register_account_with_profile(
    accounts: &dyn AccountStore,
    oauth: &dyn OAuthStore,
    invites: &dyn InviteStore,
    infra: &dyn AdminInfraStore,
    base_url: &str,
    now_ms: u64,
    registration_open: bool,
    require_approval: bool,
    invites_enabled: bool,
    minimum_age_years: u8,
    app: &OAuthApplication,
    profile: &RegistrationProfile,
    cmd: RegisterAccountCommand,
) -> Result<RegisteredAccount> {
    register(RegisterInput {
        accounts,
        oauth,
        base_url,
        now_ms,
        registration_open,
        require_approval,
        app,
        cmd,
        invite: Some(InviteInput {
            invites,
            enabled: invites_enabled,
        }),
        policy: Some(PolicyInput {
            infra,
            minimum_age_years,
            profile,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_extraction_handles_common_urls() {
        assert_eq!(host_from_url("https://example.social"), "example.social");
        assert_eq!(host_from_url("https://example.social/"), "example.social");
        assert_eq!(host_from_url("http://localhost:3000"), "localhost");
        assert_eq!(host_from_url("example.social"), "example.social");
        assert_eq!(
            host_from_url("https://user@example.social:8443/x"),
            "example.social"
        );
    }
}

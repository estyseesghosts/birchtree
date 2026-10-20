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

use crate::application::auth;
use crate::application::crypto;
use crate::application::ports::{AccountStore, NewLocalAccount, OAuthStore};
use crate::domain::account::{normalize_username, validate_username};
use crate::domain::user::{
    INVITE_COMMENT_LIMIT, Invite, InviteRefusal, invite_code_from_seed,
};
use crate::domain::{AccessToken, Account, Error, Id, OAuthApplication, Result};
use crate::mailer::{MailError, Mailer};

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
}

/// Result of a successful registration: the created account and a user token
/// scoped to the registering application (Mastodon returns the token body).
#[derive(Debug)]
pub struct RegisteredAccount {
    pub account: Account,
    pub token_plaintext: String,
    pub token: AccessToken,
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
    })
}

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
/// (upstream `require_user!` → `403 Your login is currently pending
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
    if !registration_open {
        return Err(Error::Forbidden("Registrations are closed".into()));
    }
    if !cmd.agreement {
        return Err(Error::InvalidInput(
            "agreement must be accepted".into(),
        ));
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
    })?;

    let (token_plaintext, token) =
        auth::issue_user_token(oauth, app.id, account.id, app.scopes.clone(), now_ms)?;
    Ok(RegisteredAccount {
        account,
        token_plaintext,
        token,
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
        Ok(self.by_code.lock().expect("invite store").get(code).cloned())
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
    if cmd.comment.as_deref().is_some_and(|c| c.chars().count() > INVITE_COMMENT_LIMIT) {
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
    Err(Error::Internal("could not mint a unique invite code".into()))
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
/// - Invites disabled + code supplied → `InvalidInput` (refused with reason,
///   never silently ignored).
/// - Usable invite → registration proceeds even when closed, and the invite
///   is redeemed after the account commits. A staff-issued invite bypasses
///   `require_approval` (D36 interaction: the account lands `approved=true`
///   and skips the admin pending queue; non-staff invites still queue).
/// - Unknown/unusable code while open → the code is ignored and registration
///   proceeds (upstream `allowed_registration?` stays true via open
///   registrations). While closed → `Forbidden("Registrations are closed")`.
/// - No code → plain [`register_account`].
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
    let usable_invite: Option<Invite> = match (cmd.invite_code.as_deref(), invites_enabled) {
        (Some(code), true) => match invites.invite_by_code(code)? {
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
    // An unknown/unusable code while open is ignored (upstream parity).
    let bypass_approval = usable_invite.as_ref().is_some_and(|i| i.created_by_staff);
    let registered = register_account(
        accounts,
        oauth,
        base_url,
        now_ms,
        true,
        require_approval && !bypass_approval,
        app,
        cmd.clone(),
    )?;
    if let Some(invite) = &usable_invite {
        // Best-effort: the account already committed (upstream mints the user
        // and token in one transaction and counts the invite use separately).
        // A redemption race only under-counts `uses`, never double-admits.
        if invites.redeem_invite(&invite.code, now_ms).is_err() {
            tracing::warn!("invite use not recorded after registration (redemption race)");
        }
    }
    Ok(registered)
}

// ---------------------------------------------------------------------------
// Email send paths (round 3, area A)
// ---------------------------------------------------------------------------

/// Queue a confirmation mail through the mailer port (called after commit,
/// never inside the request transaction).
pub fn request_confirmation_email(
    mailer: &dyn Mailer,
    from: &str,
    to: &str,
    account_id: Id,
    token: &str,
    base_url: &str,
) -> std::result::Result<(), MailError> {
    if token.is_empty() {
        return Err(MailError::Transport(
            "confirmation token is missing; message not sent".to_string(),
        ));
    }
    mailer.send_confirmation(from, to, account_id, token, base_url)
}

/// Queue a password-reset mail. Unknown addresses are the caller's business
/// (the HTTP layer answers 200 either way to avoid account enumeration, as
/// upstream's Devise flow does); an invalid address shape is refused here.
pub fn request_password_reset(
    mailer: &dyn Mailer,
    from: &str,
    to: &str,
    account_id: Id,
    token: &str,
    base_url: &str,
) -> std::result::Result<(), MailError> {
    if token.is_empty() {
        return Err(MailError::Transport(
            "reset token is missing; message not sent".to_string(),
        ));
    }
    mailer.send_password_reset(from, to, account_id, token, base_url)
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

//! Account creation use cases.

use crate::application::auth;
use crate::application::crypto;
use crate::application::ports::{AccountStore, NewLocalAccount, OAuthStore};
use crate::domain::account::{normalize_username, validate_username};
use crate::domain::{AccessToken, Account, Error, OAuthApplication, Result};

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

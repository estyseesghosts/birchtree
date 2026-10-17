//! Account creation use cases.

use crate::application::crypto;
use crate::application::ports::{AccountStore, NewLocalAccount};
use crate::domain::account::{normalize_username, validate_username};
use crate::domain::{Account, Error, Result};

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

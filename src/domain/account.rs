use crate::domain::error::{Error, Result};
use crate::domain::Id;

/// Maximum local username length (Mastodon uses 30).
pub const MAX_USERNAME_LEN: usize = 30;

/// Normalize a local username to its comparison form.
///
/// Mastodon stores a case-insensitive, ASCII-lowercased form for uniqueness.
/// Unicode is intentionally not case-folded here; usernames are ASCII.
pub fn normalize_username(username: &str) -> String {
    username.to_ascii_lowercase()
}

/// Validate a local username. Mirrors Mastodon's
/// `/\A[a-z0-9_]+([a-z0-9_.-]+[a-z0-9_]+)?\z/i` rule plus the length bound.
pub fn validate_username(username: &str) -> Result<()> {
    let len = username.chars().count();
    if len == 0 || len > MAX_USERNAME_LEN {
        return Err(Error::InvalidInput(format!(
            "username must be 1-{MAX_USERNAME_LEN} characters"
        )));
    }
    let bytes = username.as_bytes();
    let is_ok = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let is_mid = |b: u8| is_ok(b) || b == b'.' || b == b'-';

    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if !is_ok(first) || !is_ok(last) {
        return Err(Error::InvalidInput(
            "username must start and end with a letter, digit, or underscore".into(),
        ));
    }
    if bytes.iter().any(|&b| !is_mid(b)) {
        return Err(Error::InvalidInput(
            "username may only contain letters, digits, underscore, dot, and hyphen".into(),
        ));
    }
    Ok(())
}

/// Combine a normalized username and domain into the canonical handle key.
pub fn handle_key(normalized_username: &str, domain: &str) -> String {
    format!("{normalized_username}@{domain}")
}

/// ActivityPub identity and client-visible profile.
///
/// Distinct from `crate::domain::user::LocalUser` (credentials/login) and from
/// OAuth clients. An account is not conflated with its credential.
/// One profile field (upstream `Account::Field`: name/value pair;
/// `verified_at` is set by the proof flow, which is out of profile).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountField {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<u64>,
}

/// A local or remote ActivityPub actor with the profile attributes the API
/// renders (`#[serde(default)]` fields are filled by registration/profile
/// stores; older rows decode with the defaults).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub id: Id,
    /// Case-preserving username.
    pub username: String,
    /// ASCII-lowercased comparison form used for the unique handle index.
    pub normalized_username: String,
    /// Local domain, or the remote host for federated accounts.
    pub domain: String,
    pub display_name: String,
    pub note: String,
    pub actor_uri: String,
    pub inbox_uri: Option<String>,
    pub shared_inbox_uri: Option<String>,
    pub local: bool,
    pub locked: bool,
    pub suspended: bool,
    /// Moderation states (Phase 7.3 admin API). Older rows decode as false
    /// (`#[serde(default)]`).
    #[serde(default)]
    pub silenced: bool,
    #[serde(default)]
    pub sensitized: bool,
    pub moved_to: Option<Id>,
    /// Automated account flag (upstream `Account#bot`, PATCH
    /// `update_credentials[bot]`).
    #[serde(default)]
    pub bot: bool,
    /// Profile-directory / search visibility (upstream nullable; `None` =
    /// never set, rendered `null`).
    #[serde(default)]
    pub discoverable: Option<bool>,
    /// Search-index opt-in (upstream default `false`).
    #[serde(default)]
    pub indexable: bool,
    /// Featured-collections visibility (upstream nullable; `None` = never
    /// set, rendered `null`).
    #[serde(default)]
    pub hide_collections: Option<bool>,
    /// Profile fields (upstream `fields`, max 4 via `update_credentials`).
    #[serde(default)]
    pub fields: Vec<AccountField>,
    /// Domains attributed to this account (upstream `attribution_domains`).
    #[serde(default)]
    pub attribution_domains: Vec<String>,
    pub created_at_ms: u64,
}

impl Account {
    /// Canonical actor URL for a local account.
    pub fn local_actor_uri(base_url: &str, username: &str) -> String {
        format!("{}/users/{}", base_url.trim_end_matches('/'), username)
    }

    /// The `acct` value a local account presents (`username`, no domain) as
    /// distinct from the fully-qualified remote form.
    pub fn acct(&self) -> String {
        if self.local {
            self.username.clone()
        } else {
            format!("{}@{}", self.username, self.domain)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_validation_matches_mastodon_shape() {
        for ok in ["alice", "a_b", "a.b", "a-b", "user123", "A.B_C"] {
            assert!(validate_username(ok).is_ok(), "expected {ok} valid");
        }
        for bad in ["", ".a", "a.", "-a", "a b", "a@b", "a/b", &"x".repeat(31)] {
            assert!(validate_username(bad).is_err(), "expected {bad:?} invalid");
        }
    }

    #[test]
    fn normalize_is_ascii_lowercase() {
        assert_eq!(normalize_username("Alice_99"), "alice_99");
    }

    #[test]
    fn handle_key_combines_parts() {
        assert_eq!(handle_key("alice", "example.social"), "alice@example.social");
    }

    #[test]
    fn acct_is_local_or_qualified() {
        let mut a = Account {
            id: Id::new(1),
            username: "alice".into(),
            normalized_username: "alice".into(),
            domain: "example.social".into(),
            display_name: String::new(),
            note: String::new(),
            actor_uri: String::new(),
            inbox_uri: None,
            shared_inbox_uri: None,
            local: true,
            locked: false,
            suspended: false,
            silenced: false,
            sensitized: false,
            moved_to: None,
            bot: false,
            discoverable: None,
            indexable: false,
            hide_collections: None,
            fields: Vec::new(),
            attribution_domains: Vec::new(),
            created_at_ms: 0,
        };
        assert_eq!(a.acct(), "alice");
        a.local = false;
        assert_eq!(a.acct(), "alice@example.social");
    }
}

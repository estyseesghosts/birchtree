//! Federation domain values: actor signing keys.
//!
//! The ActivityPub wire shapes live in `activitypub/`; this module holds only
//! what the domain needs to decide who signed what.

use std::fmt;

use crate::domain::Id;

/// The RSA key pair (or, for a remote account, the public half) of one actor.
///
/// `key_id` is the actor URL with the fragment `#main-key`, as Mastodon
/// publishes it. A remote account's record carries no private key.
#[derive(Clone)]
pub struct ActorKeys {
    pub account_id: Id,
    pub key_id: String,
    /// SPKI PEM (`BEGIN PUBLIC KEY`), as published in the actor document.
    pub public_key_pem: String,
    /// PKCS#8 PEM (`BEGIN PRIVATE KEY`). Present for local accounts only.
    pub private_key_pem: Option<String>,
    /// When a remote account's document was last fetched (ms). `None` for local accounts.
    pub refreshed_at_ms: Option<u64>,
}

impl fmt::Debug for ActorKeys {
    /// The private key is never printed, so a logged or panicked value cannot leak it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActorKeys")
            .field("account_id", &self.account_id)
            .field("key_id", &self.key_id)
            .field("public_key_pem", &"<public>")
            .field(
                "private_key_pem",
                &self.private_key_pem.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// When a remote status's thread was last refreshed by an authenticated read,
/// and the refresh's ID (for the `Mastodon-Async-Refresh` header).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefreshRecord {
    pub refreshed_at_ms: u64,
    pub refresh_id: String,
}

/// The key ID an actor publishes for its main key.
pub fn main_key_id(actor_uri: &str) -> String {
    format!("{actor_uri}#main-key")
}

/// The actor URL a key ID belongs to: the key ID with its fragment removed.
/// Returns `None` when the key ID has no fragment.
pub fn actor_of_key_id(key_id: &str) -> Option<&str> {
    key_id.split_once('#').map(|(actor, _)| actor).filter(|a| !a.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_id_round_trips_to_its_actor() {
        let actor = "https://example.social/users/alice";
        assert_eq!(main_key_id(actor), "https://example.social/users/alice#main-key");
        assert_eq!(actor_of_key_id(&main_key_id(actor)), Some(actor));
        assert_eq!(actor_of_key_id("https://example.social/users/alice"), None);
        assert_eq!(actor_of_key_id("#main-key"), None);
    }

    #[test]
    fn debug_output_redacts_the_private_key() {
        let keys = ActorKeys {
            account_id: Id::new(7),
            key_id: "https://example.social/users/a#main-key".into(),
            public_key_pem: "PUB".into(),
            private_key_pem: Some("SECRET-PEM".into()),
            refreshed_at_ms: None,
        };
        let shown = format!("{keys:?}");
        assert!(!shown.contains("SECRET-PEM"));
        assert!(shown.contains("<redacted>"));
    }
}

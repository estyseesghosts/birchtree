use crate::domain::Id;

/// ActivityPub identity and client-visible profile.
///
/// Distinct from `crate::domain::user::LocalUser` (credentials/login) and from
/// OAuth clients. An account is not conflated with its credential.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: Id,
    pub username: String,      // case-preserving
    pub normalized_username: String,
    pub domain: String,
    pub actor_uri: String,
    pub inbox_uri: Option<String>,
    pub shared_inbox_uri: Option<String>,
    pub local: bool,
    pub suspended: bool,
    pub moved_to: Option<Id>,
}

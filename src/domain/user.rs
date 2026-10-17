use crate::domain::Id;

/// Local login credentials and preferences, separate from the ActivityPub actor.
#[derive(Debug, Clone)]
pub struct LocalUser {
    pub id: Id,
    pub account_id: Id,
    pub password_hash: String,
    pub email: Option<String>,
    pub email_verified: bool,
}

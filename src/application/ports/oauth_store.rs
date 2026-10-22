use super::account_store::{NewApp, NewAuthCode, NewToken};
use crate::domain::{AccessToken, AuthorizationCode, Id, OAuthApplication, Result};

/// OAuth applications, authorization codes and access tokens.
pub trait OAuthStore: Send + Sync {
    fn register_app(&self, input: NewApp) -> Result<OAuthApplication>;
    fn app_by_client_id(&self, client_id: &str) -> Result<Option<OAuthApplication>>;
    /// Resolve an application by its numeric id (used to attach the owning app
    /// to an access token).
    fn app_by_id(&self, id: Id) -> Result<Option<OAuthApplication>>;
    fn issue_access_token(&self, input: NewToken) -> Result<AccessToken>;
    fn token_by_plaintext(&self, plaintext: &str) -> Result<Option<AccessToken>>;
    fn revoke_token(&self, plaintext: &str) -> Result<bool>;
    /// D24: revoke every live token whose user is `account_id` (suspension /
    /// disablement kill), returning the revoked token ids so the adapter can
    /// close their open streams (`StreamEvent::TokenRevoked` per id, after
    /// commit). The scan is bounded (see the `redb` implementation's cap);
    /// ids are returned newest-first.
    fn revoke_account_tokens(&self, account_id: Id) -> Result<Vec<Id>>;

    fn create_authorization_code(&self, input: NewAuthCode) -> Result<AuthorizationCode>;
    fn authorization_code_by_plaintext(&self, plaintext: &str)
    -> Result<Option<AuthorizationCode>>;
    /// Atomically mark the code used and issue its token. Returns an error if
    /// the code is missing, already used, or expired — so a replay can never
    /// mint a second token.
    fn redeem_authorization_code(
        &self,
        plaintext: &str,
        token: NewToken,
        now_ms: u64,
    ) -> Result<AccessToken>;
}

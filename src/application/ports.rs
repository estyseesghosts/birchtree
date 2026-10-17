use std::sync::Arc;

use crate::domain::{
    AccessToken, Account, AuthorizationCode, Id, Job, JobKind, LocalUser, LoginSession,
    OAuthApplication, Result,
};

// Re-export the two kernel ports under their canonical names.
pub use crate::domain::{Clock, IdGenerator};

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
}

/// Accounts, local users, and the unique handle index.
pub trait AccountStore: Send + Sync {
    /// Create an account + credential + handle index entry + follow-up job(s) in
    /// one atomic transaction.
    fn create_local_account(&self, input: NewLocalAccount) -> Result<Account>;

    fn account_by_id(&self, id: Id) -> Result<Option<Account>>;

    fn account_by_handle(&self, normalized_username: &str, domain: &str) -> Result<Option<Account>>;

    fn user_by_account_id(&self, account_id: Id) -> Result<Option<LocalUser>>;

    fn local_user_by_handle(
        &self,
        normalized_username: &str,
        domain: &str,
    ) -> Result<Option<LocalUser>>;

    fn account_count(&self) -> Result<u64>;

    /// Apply a profile patch, returning the updated account.
    fn update_account_profile(
        &self,
        account_id: Id,
        update: AccountProfileUpdate,
    ) -> Result<Account>;

    /// Apply a preferences patch, returning the updated credential record.
    fn update_user_settings(&self, account_id: Id, update: UserSettingsUpdate) -> Result<LocalUser>;
}

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

    fn create_authorization_code(&self, input: NewAuthCode) -> Result<AuthorizationCode>;
    fn authorization_code_by_plaintext(&self, plaintext: &str) -> Result<Option<AuthorizationCode>>;
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

/// Server-side login sessions for the OAuth approval flow.
pub trait SessionStore: Send + Sync {
    fn create_session(&self, input: NewSession) -> Result<LoginSession>;
    fn session_by_plaintext(&self, plaintext: &str) -> Result<Option<LoginSession>>;
    fn revoke_session(&self, plaintext: &str) -> Result<bool>;
}

/// Outbound mail (verification messages). An adapter/job boundary: mail is
/// never sent inside a request transaction.
pub trait MailSender: Send + Sync {
    /// Send an email-confirmation message for a newly registered account.
    fn send_confirmation(&self, to: &str, account_id: Id, token: &str) -> Result<()>;
}

/// Durable job queue.
pub trait JobQueue: Send + Sync {
    fn enqueue(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
        now_ms: u64,
    ) -> Result<Job>;
    /// Claim up to `limit` due jobs under a lease.
    fn claim_due(&self, now_ms: u64, limit: usize) -> Result<Vec<Job>>;
    fn complete(&self, id: Id) -> Result<()>;
    /// Record a failed attempt; schedules a retry or quarantines.
    fn retry(&self, id: Id, error: &str, now_ms: u64) -> Result<Job>;
    fn quarantine(&self, id: Id, error: &str) -> Result<()>;
    fn get(&self, id: Id) -> Result<Option<Job>>;
}

/// Readiness probe used by the runtime.
pub trait ReadyCheck: Send + Sync {
    /// True when the store is open and can serve a read.
    fn is_ready(&self) -> bool;
}

/// Bundled persistence surface assembled into a `Context`.
///
/// Implemented once by the `redb` adapter; application code depends on the
/// individual narrow traits above, never on `redb` types.
pub trait Store: AccountStore + OAuthStore + SessionStore + JobQueue + ReadyCheck {}
impl<T> Store for T where T: AccountStore + OAuthStore + SessionStore + JobQueue + ReadyCheck {}

/// Snapshot of kernel dependencies assembled in `application::context`.
#[derive(Clone)]
pub struct ContextDeps {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
}

/// Dependency-assembly surface for adapters.
pub trait Ports: Send + Sync {
    fn clock(&self) -> Arc<dyn Clock>;
    fn ids(&self) -> Arc<dyn IdGenerator>;
}

impl Ports for ContextDeps {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
    fn ids(&self) -> Arc<dyn IdGenerator> {
        self.ids.clone()
    }
}

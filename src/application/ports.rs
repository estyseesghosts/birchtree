use std::sync::Arc;

use crate::domain::{
    AccessToken, Account, Id, Job, JobKind, LocalUser, LoginSession, OAuthApplication, Result,
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

/// Input for creating a login session.
pub struct NewSession {
    pub user_id: Id,
    pub token_hash: String,
    pub now_ms: u64,
    pub ttl_ms: u64,
}

/// Accounts, local users, and the unique handle index.
pub trait AccountStore: Send + Sync {
    /// Create an account + credential + handle index entry + follow-up job in
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
}

/// OAuth applications and access tokens.
pub trait OAuthStore: Send + Sync {
    fn register_app(&self, input: NewApp) -> Result<OAuthApplication>;
    fn app_by_client_id(&self, client_id: &str) -> Result<Option<OAuthApplication>>;
    fn issue_access_token(&self, input: NewToken) -> Result<AccessToken>;
    fn token_by_plaintext(&self, plaintext: &str) -> Result<Option<AccessToken>>;
    fn revoke_token(&self, plaintext: &str) -> Result<bool>;
}

/// Server-side login sessions for the OAuth approval flow.
pub trait SessionStore: Send + Sync {
    fn create_session(&self, input: NewSession) -> Result<LoginSession>;
    fn session_by_plaintext(&self, plaintext: &str) -> Result<Option<LoginSession>>;
    fn revoke_session(&self, plaintext: &str) -> Result<bool>;
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

//! Per-route OAuth authentication and scope enforcement.
//!
//! Two extractors are provided:
//!
//! - [`Auth`] requires a valid bearer token (401 if missing/invalid).
//! - [`OptionalAuth`] permits anonymous access but, when a token *is* present,
//!   it must be valid (Mastodon's `authorize_if_got_token!`).
//!
//! Scope checks are operation-specific, so a handler calls
//! [`Authenticated::require_scopes`] with the Mastodon scope alternatives it
//! accepts (e.g. `["read", "read:accounts"]`).

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::api::mastodon::ApiState;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::application::ports::{AccountStore, Store};
use crate::domain::{AccessToken, Id, OAuthApplication, UserRole};

/// A validated bearer token and the application that owns it.
#[derive(Clone)]
pub struct Authenticated {
    pub token: AccessToken,
    pub app: OAuthApplication,
    /// Whether the token owner's account is approved. Resolved once per
    /// request in [`resolve`]; app-only tokens and tokens whose credential
    /// row is gone read `true` (fail-open: approval gating only ever
    /// restricts accounts the approval queue actually holds).
    pub approved: bool,
}

impl Authenticated {
    /// True when the token carries at least one of `required`.
    pub fn has_any_scope(&self, required: &[&str]) -> bool {
        required.is_empty()
            || required
                .iter()
                .any(|scope| self.token.scopes.iter().any(|s| s == scope))
    }

    /// Enforce Mastodon scope semantics: the token must contain at least one of
    /// the accepted scopes (broad or granular).
    pub fn require_scopes(&self, required: &[&str]) -> ApiResult<()> {
        if self.has_any_scope(required) {
            Ok(())
        } else {
            Err(ApiError::insufficient_scope())
        }
    }

    /// The resource-owner account id, or Mastodon's user-required error for an
    /// app-only (client-credentials) token.
    ///
    /// D36: an unapproved (pending) account's token is refused here with
    /// upstream's `require_user!` response for `:pending_approval`
    /// (`403 Your login is currently pending approval`). Email confirmation
    /// stays unenforced (the separate Phase 2 gap).
    pub fn require_user(&self) -> ApiResult<Id> {
        let user_id = self.token.user_id.ok_or_else(ApiError::requires_user)?;
        if !self.approved {
            return Err(ApiError::forbidden("Your login is currently pending approval"));
        }
        Ok(user_id)
    }

    /// Role-before-scope gate for the admin API (`/api/v1/admin/*` and
    /// `/api/v2/admin/accounts`).
    ///
    /// Check order mirrors upstream: `Admin::PermissionsConcern` runs its
    /// `before_action` (role) before each controller's scope `before_action`,
    /// and `verify_authorized` enforces the per-action policy last. Here:
    ///
    /// 1. `require_user` — an app-only token is rejected with `422
    ///    {"error":"This method requires an authenticated user"}`;
    /// 2. role — an account without a sufficient role is rejected with `403
    ///    {"error":"Forbidden"}` (upstream renders the Rack status text);
    /// 3. scope — a token without one of `scopes` is rejected with `403
    ///    {"error":"This action is outside the authorized scopes"}`.
    ///
    /// `area` selects the permission split from `birchtree.md` §7 Phase 7.3:
    /// `Accounts` and `Reports` admit moderators and admins; `AdminOnly`
    /// (domain allows/blocks, email-domain blocks, IP blocks, canonical email
    /// blocks, trends approvals, measures/dimensions/retention, tags) admits
    /// admins only. Returns the caller's role for handlers that need it.
    ///
    /// Usage (other subagents: copy this shape):
    ///
    /// ```ignore
    /// use crate::api::mastodon::auth::{AdminArea, Auth};
    ///
    /// async fn show(
    ///     State(state): State<ApiState>,
    ///     auth: Auth,
    ///     Path(id): Path<String>,
    /// ) -> Result<Json<Value>, ApiError> {
    ///     let role = auth.0.require_admin(
    ///         &*state.context.store,
    ///         AdminArea::Accounts,
    ///         &["admin:read", "admin:read:accounts"],
    ///     )?;
    ///     let _ = role; // `UserRole::Admin` or `UserRole::Moderator` here.
    ///     // ... handler body ...
    /// }
    /// ```
    pub fn require_admin(
        &self,
        store: &dyn AccountStore,
        area: AdminArea,
        scopes: &[&str],
    ) -> ApiResult<UserRole> {
        let user_id = self.require_user()?;
        let role = store
            .user_by_account_id(user_id)
            .map_err(ApiError::from)?
            .map(|user| user.role)
            .unwrap_or(UserRole::None);
        let allowed = match role {
            UserRole::Admin => true,
            UserRole::Moderator => matches!(area, AdminArea::Accounts | AdminArea::Reports),
            UserRole::None => false,
        };
        if !allowed {
            return Err(ApiError::role_forbidden());
        }
        self.require_scopes(scopes)?;
        Ok(role)
    }
}

/// Which admin area a route belongs to, for [`Authenticated::require_admin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminArea {
    /// Account moderation routes (`admin:*:accounts`): moderators and admins.
    Accounts,
    /// Moderator queue (`admin:*:reports`): moderators and admins.
    Reports,
    /// Everything else (domain allows/blocks, email-domain blocks, IP blocks,
    /// canonical email blocks, trends approvals, measures/dimensions/retention,
    /// tags): admins only.
    AdminOnly,
}

fn bearer_token(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(axum::http::header::AUTHORIZATION)?;
    let value = value.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
        Some(token.trim().to_string())
    } else {
        None
    }
}

async fn resolve(state: &ApiState, plaintext: &str) -> ApiResult<Authenticated> {
    let store: &dyn Store = &*state.context.store;
    let token = store
        .token_by_plaintext(plaintext)
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::invalid_token)?;
    if !token.is_valid_at(state.context.now_ms()) {
        return Err(ApiError::invalid_token());
    }
    let app = store
        .app_by_id(token.app_id)
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::invalid_token)?;
    // D36: resolve the approval state once per request so `require_user`
    // gates pending accounts exactly where upstream's `require_user!` does.
    let approved = match token.user_id {
        None => true,
        Some(user_id) => store
            .user_by_account_id(user_id)
            .map_err(ApiError::from)?
            .map(|user| user.approved)
            .unwrap_or(true),
    };
    Ok(Authenticated { token, app, approved })
}

#[axum::async_trait]
impl FromRequestParts<ApiState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let plaintext = bearer_token(parts).ok_or_else(ApiError::invalid_token)?;
        Ok(Auth(resolve(state, &plaintext).await?))
    }
}

#[axum::async_trait]
impl FromRequestParts<ApiState> for OptionalAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        match bearer_token(parts) {
            Some(plaintext) => Ok(OptionalAuth(Some(resolve(state, &plaintext).await?))),
            None => Ok(OptionalAuth(None)),
        }
    }
}

/// Required bearer token.
pub struct Auth(pub Authenticated);

/// Optional bearer token.
pub struct OptionalAuth(pub Option<Authenticated>);

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
use crate::application::ports::Store;
use crate::domain::{AccessToken, Id, OAuthApplication};

/// A validated bearer token and the application that owns it.
#[derive(Clone)]
pub struct Authenticated {
    pub token: AccessToken,
    pub app: OAuthApplication,
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
    pub fn require_user(&self) -> ApiResult<Id> {
        self.token.user_id.ok_or_else(ApiError::requires_user)
    }
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
    Ok(Authenticated { token, app })
}

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

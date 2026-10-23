use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::support::credential_response;
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::application::ports::Store;

/// `GET /api/v1/accounts/{id}` — public account profile.
pub async fn show(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<crate::domain::Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.account_by_id(id) {
        Ok(Some(account)) => {
            // D36: unapproved local accounts are invisible (upstream
            // `check_account_approval` → 404). Remote accounts have no
            // credential row and always render.
            if account.local {
                match store.user_by_account_id(id) {
                    Ok(Some(user)) if !user.approved => {
                        return ApiError::not_found().into_response();
                    }
                    Ok(_) => {}
                    Err(err) => return ApiError::from(err).into_response(),
                }
            }
            let stats = store.account_stats(id).ok();
            axum::Json(dto::account_json_with_stats(
                &state.server,
                &account,
                stats.as_ref(),
            ))
            .into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/verify_credentials`.
pub async fn verify_credentials(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["profile", "read", "read:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    credential_response(&state, account_id)
}

pub async fn identity_proofs_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let _account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(target) = id.parse::<crate::domain::Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.account_by_id(target) {
        Ok(Some(_)) => axum::Json(json!([])).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

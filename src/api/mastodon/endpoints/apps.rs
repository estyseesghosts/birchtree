//! `POST /api/v1/apps` — OAuth application registration.
//!
//! Mastodon creates a confidential Doorkeeper application and returns the
//! `CredentialApplicationSerializer` body including the one-time client secret.
//! Registration is unauthenticated.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::api::forms::{self, FormBody};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::oauth::scopes;
use crate::application::auth;
use crate::application::ports::Store;

/// The default scopes when the client does not request any.
const DEFAULT_SCOPES: &str = scopes::DEFAULT_SCOPE;

/// `POST /api/v1/apps`.
pub async fn create_app(State(state): State<ApiState>, FormBody(pairs): FormBody) -> Response {
    match create(&state, &pairs) {
        Ok(value) => (StatusCode::OK, axum::Json(value)).into_response(),
        Err(err) => err.into_response(),
    }
}

fn create(state: &ApiState, pairs: &[(String, String)]) -> ApiResult<serde_json::Value> {
    let name = forms::get(&pairs, "client_name").unwrap_or("").trim();
    if name.is_empty() {
        return Err(ApiError::unprocessable(
            "Validation failed: Client name can't be blank",
        ));
    }

    // `redirect_uris` may arrive as a single value or a `[]` array; each value
    // may itself contain newline/whitespace-separated URIs (Doorkeeper splits
    // on whitespace).
    let redirect_uris: Vec<String> = forms::all(&pairs, "redirect_uris")
        .into_iter()
        .flat_map(|value| value.split_whitespace().map(str::to_string))
        .filter(|uri| !uri.is_empty())
        .collect();
    if redirect_uris.is_empty() {
        return Err(ApiError::unprocessable(
            "Validation failed: Redirect uri can't be blank",
        ));
    }

    let requested = match forms::get(&pairs, "scopes") {
        Some(raw) if !raw.trim().is_empty() => scopes::parse(raw),
        _ => scopes::parse(DEFAULT_SCOPES),
    };
    if let Some(unknown) = requested.iter().find(|s| !scopes::is_known(s)) {
        return Err(ApiError::unprocessable(format!(
            "Validation failed: Scopes contains an unknown scope: {unknown}"
        )));
    }

    let website = forms::get(&pairs, "website")
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string);

    let store: &dyn Store = &*state.context.store;
    let registered = auth::register_app(
        store,
        name.to_string(),
        redirect_uris,
        requested,
        website,
        // Mastodon registers confidential applications: every app receives a
        // client secret (returned once, here).
        true,
        state.context.now_ms(),
    )
    .map_err(ApiError::from)?;

    Ok(dto::app_json(
        &registered.app,
        registered.client_secret.as_deref(),
    ))
}

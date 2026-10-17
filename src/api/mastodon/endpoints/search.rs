//! Search endpoints (Phase 4): `GET /api/v2/search`.
//!
//! Mastodon v4.7.3 removed `GET /api/v1/search`, so only v2 exists here. All
//! four result keys are always present; an unknown `type` returns empty
//! arrays; unauthenticated pagination/resolve answer the pinned 401 bodies.

use axum::extract::State;
use axum::http::Uri;
use axum::response::{IntoResponse, Response};

use crate::api::forms;
use crate::api::mastodon::auth::OptionalAuth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::search;
use crate::application::ports::Store;
use crate::domain::Id;

/// Read scopes for authenticated search.
const READ_SCOPES: &[&str] = &["read", "read:search"];

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// `GET /api/v2/search`.
pub async fn index(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    if let Some(authenticated) = &auth.0
        && let Err(err) = authenticated.require_scopes(READ_SCOPES)
    {
        return err.into_response();
    }
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };

    // `params.require(:q)` → 400 with the ParameterMissing message.
    let query = match forms::get(&pairs, "q") {
        Some(raw) => match search::require_query(Some(raw)) {
            Ok(query) => query.to_string(),
            Err(_) => {
                return ApiError::bad_request("param is missing or the value is empty: q")
                    .into_response();
            }
        },
        None => {
            return ApiError::bad_request("param is missing or the value is empty: q")
                .into_response();
        }
    };

    // Pagination: negative limit/offset → 400 (Mastodon
    // `pagination_params`); `offset` without auth → 401.
    for key in ["limit", "offset"] {
        if let Some(raw) = forms::get(&pairs, key)
            && raw.trim().starts_with('-')
        {
            return ApiError::bad_request(
                "Pagination values for `offset` and `limit` must be positive",
            )
            .into_response();
        }
    }
    let offset: usize = forms::get(&pairs, "offset")
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0);
    if offset > 0 && viewer.is_none() {
        return ApiError::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "Search queries pagination is not supported without authentication",
        )
        .into_response();
    }
    if bool_cast(forms::get(&pairs, "resolve").unwrap_or("false")) && viewer.is_none() {
        return ApiError::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "Search queries that resolve remote resources are not supported without authentication",
        )
        .into_response();
    }

    let limit_raw = forms::get(&pairs, "limit");
    let limit: usize = match limit_raw {
        None => search::DEFAULT_LIMIT,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            if value < 0 {
                return ApiError::bad_request(
                    "Pagination values for `offset` and `limit` must be positive",
                )
                .into_response();
            }
            (value as usize).min(search::MAX_LIMIT)
        }
    };

    let kind = forms::get(&pairs, "type").map(str::to_string);
    let following = bool_cast(forms::get(&pairs, "following").unwrap_or("false"));
    let account_id = match forms::get(&pairs, "account_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => None,
        None => None,
    };
    let max_id = match forms::get(&pairs, "max_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        _ => None,
    };
    let min_id = match forms::get(&pairs, "min_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        _ => None,
    };

    let store: &dyn Store = &*state.context.store;
    match search::search(
        store,
        viewer,
        &query,
        kind.as_deref(),
        offset,
        limit,
        following,
        account_id,
        max_id,
        min_id,
    ) {
        Ok(results) => {
            let accounts: Vec<serde_json::Value> = results
                .accounts
                .iter()
                .map(|account| dto::account_json(&state.server, account))
                .collect();
            let statuses = match results.statuses.is_empty() {
                true => Vec::new(),
                false => {
                    match crate::api::mastodon::status_view::load_status_views(
                        &state.server,
                        store,
                        viewer,
                        results.statuses,
                        state.context.now_ms(),
                    ) {
                        Ok(views) => views
                            .iter()
                            .map(|view| {
                                dto::status_json(
                                    &state.server,
                                    viewer,
                                    state.context.now_ms(),
                                    view,
                                    dto::StatusSourceMode::Content,
                                )
                            })
                            .collect(),
                        Err(err) => return err.into_response(),
                    }
                }
            };
            let hashtags: Vec<serde_json::Value> = results
                .hashtags
                .iter()
                .map(|tag| dto::hashtag_json(&state.server, tag, viewer))
                .collect();
            axum::Json(dto::search_json(accounts, statuses, hashtags)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

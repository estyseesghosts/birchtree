//! Marker endpoints (`GET`/`POST /api/v1/markers`).
//!
//! Scopes are `read`/`write:statuses` (Mastodon's markers controller), the
//! response maps timeline name → `{last_read_id, version, updated_at}`, and
//! a stale `lock_version` answers 409 `{"error":"Conflict during update,
//! please try again"}`.

use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value};

use crate::api::forms;
use crate::api::json_body;
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::markers;
use crate::application::ports::Store;
use crate::domain::{Error as DomainError, Id, Marker};

/// `GET /api/v1/markers?timeline[]=home&timeline[]=notifications`.
pub async fn index(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    let Some(authenticated) = &auth.0 else {
        return ApiError::invalid_token().into_response();
    };
    if let Err(err) = authenticated.require_scopes(&["read", "read:statuses"]) {
        return err.into_response();
    }
    let Some(account_id) = authenticated.token.user_id else {
        return ApiError::requires_user().into_response();
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let timelines: Vec<String> = forms::all(&pairs, "timeline")
        .iter()
        .map(|raw| raw.trim().to_string())
        .collect();
    let store: &dyn Store = &*state.context.store;
    let refs: Vec<&str> = timelines.iter().map(String::as_str).collect();
    match markers::index(store, account_id, &refs) {
        Ok(rows) => {
            let mut map = Map::new();
            for row in rows {
                map.insert(row.timeline.clone(), dto::marker_json(&row));
            }
            axum::Json(Value::Object(map)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/markers` — upsert `home[last_read_id]` /
/// `notifications[last_read_id]`.
pub async fn create(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
    headers: HeaderMap,
    request_body: axum::body::Body,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:statuses"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    // Mastodon reads `home[last_read_id]`/`notifications[last_read_id]`
    // params from the query string OR the form body; parse both.
    let mut pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    if let Ok(body) = axum::body::to_bytes(request_body, 64 * 1024).await {
        pairs.extend(json_body::body_pairs(&headers, &body));
    }
    let mut updates: Vec<(String, Id, Option<u64>)> = Vec::new();
    for timeline in Marker::TIMELINES {
        let key = format!("{timeline}[last_read_id]");
        let Some(raw) = forms::get(&pairs, &key) else {
            continue;
        };
        let Ok(last_read_id) = raw.parse::<Id>() else {
            return ApiError::unprocessable(format!(
                "Validation failed: Last read id is invalid: {raw}"
            ))
            .into_response();
        };
        updates.push((timeline.to_string(), last_read_id, None));
    }
    if updates.is_empty() {
        // Mastodon `params.slice(*TIMELINES)` with nothing usable renders an
        // empty map (200).
        return axum::Json(Map::<String, Value>::new()).into_response();
    }
    let store: &dyn Store = &*state.context.store;
    match markers::update(store, account_id, &updates, state.context.now_ms()) {
        Ok(rows) => {
            let mut map = Map::new();
            for row in rows {
                map.insert(row.timeline.clone(), dto::marker_json(&row));
            }
            axum::Json(Value::Object(map)).into_response()
        }
        Err(DomainError::Conflict(message)) => {
            ApiError::new(axum::http::StatusCode::CONFLICT, message).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

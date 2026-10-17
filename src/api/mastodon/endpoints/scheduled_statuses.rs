//! Scheduled status endpoints (`Api::V1::ScheduledStatusesController`).
//!
//! Created only via `POST /api/v1/statuses` with `scheduled_at`; these
//! routes list, inspect, reschedule and delete. `params` is the replayable
//! creation params; `media_attachments` is the full media serializer.

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::api::mastodon::ApiState;
use crate::application::scheduled_statuses;
use crate::application::ports::Store;
use crate::domain::Id;

/// Read scopes for scheduled-status reads.
const READ_SCOPES: &[&str] = &["read", "read:statuses"];
/// Write scopes for reschedule/delete.
const WRITE_SCOPES: &[&str] = &["write", "write:statuses"];

/// Render one scheduled status with its media attachments.
fn render(state: &ApiState, scheduled: &crate::domain::ScheduledStatus) -> Response {
    let store: &dyn Store = &*state.context.store;
    let media = store
        .attachable_media(scheduled.account_id, &scheduled.media_ids, None)
        .unwrap_or_default();
    axum::Json(dto::scheduled_status_json(
        &state.server,
        scheduled,
        &media,
    ))
    .into_response()
}

/// `GET /api/v1/scheduled_statuses` — id-descending with `Link` cursors.
pub async fn index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let page = match pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        20,
        40,
    ) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match scheduled_statuses::index(
        store,
        viewer,
        page.max_id,
        page.since_id,
        page.min_id,
        page.limit,
    ) {
        Ok(rows) => {
            let body: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    let media = store
                        .attachable_media(row.account_id, &row.media_ids, None)
                        .unwrap_or_default();
                    dto::scheduled_status_json(&state.server, row, &media)
                })
                .collect();
            let mut response = axum::Json(body).into_response();
            if let Some(link) =
                pagination::link_header(&uri, rows.last().map(|r| r.id), rows.first().map(|r| r.id))
            {
                if let Ok(value) =
                    axum::http::header::HeaderValue::from_str(&link)
                {
                    response
                        .headers_mut()
                        .insert(axum::http::header::LINK, value);
                }
            }
            response
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/scheduled_statuses/{id}`.
pub async fn show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match scheduled_statuses::show(store, viewer, id) {
        Ok(row) => render(&state, &row),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT/PATCH /api/v1/scheduled_statuses/{id}` — `scheduled_at` only.
pub async fn update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let Some(raw_at) = forms::get(&body, "scheduled_at").map(str::trim) else {
        return ApiError::unprocessable("Validation failed: Scheduled at can't be blank")
            .into_response();
    };
    let now_ms = state.context.now_ms();
    let parsed = match scheduled_statuses::parse_scheduled_at(Some(raw_at), now_ms) {
        Ok(parsed) => parsed,
        Err(_) => {
            return ApiError::unprocessable("Record invalid").into_response();
        }
    };
    let Some(scheduled_at_ms) = parsed else {
        return ApiError::unprocessable("Validation failed: Scheduled at can't be blank")
            .into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match scheduled_statuses::reschedule(store, viewer, id, scheduled_at_ms, now_ms) {
        Ok(row) => render(&state, &row),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/scheduled_statuses/{id}` — `200 {}`.
pub async fn destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match scheduled_statuses::destroy(store, viewer, id) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

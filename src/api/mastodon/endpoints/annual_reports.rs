//! Annual report reads and generation requests (Phase 7.3).
//!
//! The `:id` route parameter is the report year (upstream parses it with
//! `to_i`; an unparseable year answers 404). Reads use `read|read:accounts`
//! and generation/mark-read use `write|write:accounts`; app-only tokens get
//! the user-required 422. Generation itself runs as a durable job.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::{status_view, ApiState};
use crate::application::annual_reports::{self, GenerateOutcome};
use crate::application::ports::Store;

/// Read scopes for the index/show/state reads.
const READ_SCOPES: &[&str] = &["read", "read:accounts"];
/// Write scopes for generate and mark-read.
const WRITE_SCOPES: &[&str] = &["write", "write:accounts"];

/// The `:id` is a year; anything else is upstream's `RecordNotFound` 404.
fn parse_year(raw: &str) -> Result<i32, ApiError> {
    raw.trim().parse::<i32>().map_err(|_| ApiError::not_found())
}

/// `GET /api/v1/annual_reports`: the viewer's unread reports with the
/// referenced accounts and top statuses embedded.
pub async fn index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let reports = match annual_reports::index_reports(store, viewer) {
        Ok(reports) => reports,
        Err(err) => return ApiError::from(err).into_response(),
    };
    render_collection(&state, store, viewer, reports)
}

/// `GET /api/v1/annual_reports/:id` (the `:id` is the year).
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
    let year = match parse_year(&raw) {
        Ok(year) => year,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let report = match annual_reports::show_report(store, viewer, year) {
        Ok(report) => report,
        Err(err) => return ApiError::from(err).into_response(),
    };
    render_collection(&state, store, viewer, vec![report])
}

/// `GET /api/v1/annual_reports/:id/state`: `available`, `generating`,
/// `eligible` or `ineligible`.
pub async fn show_state(
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
    let year = match parse_year(&raw) {
        Ok(year) => year,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match annual_reports::report_state(store, viewer, year) {
        Ok(state) => axum::Json(serde_json::json!({ "state": state.as_str() })).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/annual_reports/:id/generate`: enqueue generation as a
/// durable job. An existing report answers `200 {}`; a running or fresh
/// request answers `202` with an empty body.
pub async fn generate(
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
    let year = match parse_year(&raw) {
        Ok(year) => year,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match annual_reports::request_generate(store, viewer, year, state.context.now_ms()) {
        Ok(GenerateOutcome::Exists) => axum::Json(serde_json::json!({})).into_response(),
        Ok(GenerateOutcome::Running) | Ok(GenerateOutcome::Enqueued) => {
            StatusCode::ACCEPTED.into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/annual_reports/:id/read`: mark the report read (`200 {}`).
pub async fn mark_read(
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
    let year = match parse_year(&raw) {
        Ok(year) => year,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match annual_reports::mark_viewed(store, viewer, year, state.context.now_ms()) {
        Ok(Some(_)) => axum::Json(serde_json::json!({})).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Render one or more reports with the referenced accounts (the viewer, the
/// only referenced account in this server's report data) and top statuses.
fn render_collection(
    state: &ApiState,
    store: &dyn Store,
    viewer: crate::domain::Id,
    reports: Vec<crate::domain::AnnualReport>,
) -> Response {
    let mut status_ids: Vec<crate::domain::Id> = Vec::new();
    let mut statuses = Vec::new();
    for report in &reports {
        match annual_reports::report_statuses(store, report) {
            Ok(found) => {
                for status in found {
                    if !status_ids.contains(&status.id) {
                        status_ids.push(status.id);
                        statuses.push(status);
                    }
                }
            }
            Err(err) => return ApiError::from(err).into_response(),
        }
    }
    let accounts = if reports.is_empty() {
        Vec::new()
    } else {
        match store.account_by_id(viewer) {
            Ok(Some(account)) => vec![dto::account_json(&state.server, &account)],
            Ok(None) => Vec::new(),
            Err(err) => return ApiError::from(err).into_response(),
        }
    };
    let views = match status_view::load_status_views(
        &state.server,
        store,
        Some(viewer),
        statuses,
        state.context.now_ms(),
    ) {
        Ok(views) => views,
        Err(err) => return err.into_response(),
    };
    let status_values: Vec<serde_json::Value> = views
        .iter()
        .map(|view| {
            dto::status_json(
                &state.server,
                Some(viewer),
                state.context.now_ms(),
                view,
                dto::StatusSourceMode::Content,
            )
        })
        .collect();
    axum::Json(dto::annual_reports_json(&reports, accounts, status_values)).into_response()
}

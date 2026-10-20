//! `POST /api/v1/reports`: file a report about an account and its statuses. Phase 6
//! stores the report and, with `forward=true` and a remote target, sends a Flag to
//! the target's server. The report listing and its moderation are Phase 7.3.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::federation_ports::NewReport;
use crate::application::ports::Store;
use crate::domain::Id;

const REPORT_SCOPES: &[&str] = &["write", "write:reports"];

/// Mastodon's comment limit for a report.
const COMMENT_LIMIT: usize = 1000;

/// `POST /api/v1/reports`.
pub async fn create(State(state): State<ApiState>, auth: Auth, LenientForm(pairs): LenientForm) -> Response {
    if let Err(err) = auth.0.require_scopes(REPORT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let Some(account_raw) = forms::get(&pairs, "account_id") else {
        return ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, "Validation failed: Account can't be blank").into_response();
    };
    let Ok(target_id) = account_raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let target = match store.account_by_id(target_id) {
        Ok(Some(account)) => account,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    if target.id == viewer {
        return ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, "Validation failed: You cannot report yourself").into_response();
    }
    let comment = forms::get(&pairs, "comment").unwrap_or_default().to_string();
    if comment.chars().count() > COMMENT_LIMIT {
        return ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, "Validation failed: Comment is too long").into_response();
    }
    let mut status_ids = Vec::new();
    for raw in forms::all(&pairs, "status_ids[]") {
        let Ok(id) = raw.parse::<Id>() else {
            return ApiError::not_found().into_response();
        };
        match store.status_by_id(id) {
            Ok(Some(status)) if status.account_id == target.id => status_ids.push(id),
            Ok(_) => return ApiError::not_found().into_response(),
            Err(err) => return ApiError::from(err).into_response(),
        }
    }
    let forward = forms::get(&pairs, "forward").is_some_and(|v| v == "true");
    let forwarded = forward && !target.local;
    let now = state.context.now_ms();
    let report = match store.insert_report(
        NewReport {
            reporter_id: viewer,
            target_account_id: target.id,
            status_ids: status_ids.clone(),
            comment,
            forwarded,
            activity_uri: None,
        },
        now,
    ) {
        Ok(report) => report,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let created = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(report.created_at_ms as i64)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    axum::Json(json!({
        "id": report.id.value().to_string(),
        "action_taken": false,
        "action_taken_at": null,
        "category": "other",
        "comment": report.comment,
        "forwarded": forwarded,
        "created_at": created,
        "status_ids": status_ids.iter().map(|i| i.value().to_string()).collect::<Vec<_>>(),
        "rule_ids": null,
        "target_account": dto::account_json(&state.server, &target),
    }))
    .into_response()
}

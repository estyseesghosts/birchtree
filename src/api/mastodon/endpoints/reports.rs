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

/// Report categories (upstream `Report` enum: `other`, `spam`, `legal`,
/// `violation`). `rule_ids` present forces `violation`
/// (`ReportService#call`).
const VALID_CATEGORIES: &[&str] = &["other", "spam", "legal", "violation"];

/// Mastodon's domain normalization for report forwarding
/// (`TagManager#normalize_domain`): lowercase.
fn normalize_forward_domain(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// The report category for a create request (upstream `ReportService#call`):
/// any `rule_ids` force `violation`, otherwise the requested category or
/// `other`. Unknown categories are an error (upstream's enum would raise;
/// failing closed keeps the row honest).
pub fn report_category(category_raw: Option<&str>, has_rule_ids: bool) -> Result<String, &'static str> {
    if has_rule_ids {
        return Ok("violation".to_string());
    }
    match category_raw {
        None | Some("") => Ok("other".to_string()),
        Some(raw) if VALID_CATEGORIES.contains(&raw) => Ok(raw.to_string()),
        Some(_) => Err("Validation failed: Category is invalid"),
    }
}

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
    // `forward_to_domains[]` (upstream `report_params`): the report is
    // forwarded only when the target's domain is listed (default: just the
    // target's domain, so the default behavior is unchanged).
    let mut forward_to_domains: Vec<String> = forms::all(&pairs, "forward_to_domains[]")
        .into_iter()
        .map(normalize_forward_domain)
        .filter(|domain| !domain.is_empty())
        .collect();
    if forward_to_domains.is_empty() {
        forward_to_domains.push(target.domain.clone());
    }
    let forwarded = forward && !target.local && forward_to_domains.contains(&target.domain);
    // `rule_ids[]` (upstream `report_params` + `ReportService#call`): present
    // ids force the `violation` category and must name real server rules
    // (upstream `validate_rule_ids` → 422 otherwise).
    let mut rule_ids: Vec<Id> = Vec::new();
    for raw in forms::all(&pairs, "rule_ids[]") {
        let Ok(id) = raw.parse::<Id>() else {
            return ApiError::not_found().into_response();
        };
        rule_ids.push(id);
    }
    if !rule_ids.is_empty() {
        let known = match store.admin_rules_by_ids(&rule_ids) {
            Ok(rules) => rules,
            Err(err) => return ApiError::from(err).into_response(),
        };
        if known.len() != rule_ids.len() {
            return ApiError::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "Validation failed: Rule ids is invalid",
            )
            .into_response();
        }
    }
    // `category` (upstream `report_params` + `ReportService#call`): one of
    // the enum values; any `rule_ids` force `violation`.
    let category = match report_category(forms::get(&pairs, "category"), !rule_ids.is_empty()) {
        Ok(category) => category,
        Err(message) => {
            return ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, message).into_response();
        }
    };
    // `collection_ids[]` (upstream `report_params`): accepted and validated
    // as numeric, then persisted on the report row. Unknown collections
    // cannot be checked without that store, so every numeric id is accepted.
    let mut collection_ids: Vec<Id> = Vec::new();
    for raw in forms::all(&pairs, "collection_ids[]") {
        let Ok(id) = raw.parse::<Id>() else {
            return ApiError::not_found().into_response();
        };
        collection_ids.push(id);
    }
    let now = state.context.now_ms();
    let report = match store.insert_report(
        NewReport {
            reporter_id: viewer,
            target_account_id: target.id,
            status_ids: status_ids.clone(),
            comment,
            forwarded,
            activity_uri: None,
            category: category.clone(),
            rule_ids: rule_ids.iter().map(|id| id.to_string()).collect(),
            forward_to_domains,
            collection_ids,
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
        // The computed category and validated rule ids (upstream's create
        // response renders the stored report's; persisted on the row since
        // the fix round, rendered from the accepted values here).
        "category": category,
        "comment": report.comment,
        "forwarded": forwarded,
        "forward_to_domains": report.forward_to_domains,
        "created_at": created,
        "status_ids": status_ids.iter().map(|i| i.value().to_string()).collect::<Vec<_>>(),
        "rule_ids": rule_ids.iter().map(|i| i.value().to_string()).collect::<Vec<_>>(),
        "target_account": dto::account_json(&state.server, &target),
    }))
    .into_response()
}

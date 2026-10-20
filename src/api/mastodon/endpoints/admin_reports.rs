//! Moderator report queue (`GET /api/v1/admin/reports*`), wired to the
//! records `POST /api/v1/reports` creates.
//!
//! Contracts follow Mastodon v4.7.3
//! (`app/controllers/api/v1/admin/reports_controller.rb`): user token +
//! moderator-or-admin role (role check before scope check),
//! `admin:read`/`admin:read:reports` for reads and
//! `admin:write`/`admin:write:reports` for writes, `Link` cursors, and
//! `REST::Admin::ReportSerializer` rows.
//!
//! The role gate lives in [`gate`] (one small function; if subagent A's
//! `Authenticated::require_admin` shape changes, only it needs an edit).

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::{AdminArea, Auth, Authenticated};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::pagination;
use crate::api::mastodon::status_view::load_status_views;
use crate::api::mastodon::ApiState;
use crate::application::admin as admin_uc;
use crate::application::ports::{AccountStore, AdminInfraStore, AdminReportFilter, AdminReportPatch, Store};
use crate::domain::{Account, Id, LocalUser, Report, UserRole};

use super::admin_accounts::admin_account_json;

const READ_SCOPES: &[&str] = &["admin:read", "admin:read:reports"];
const WRITE_SCOPES: &[&str] = &["admin:write", "admin:write:reports"];

/// Role-before-scope gate for every route in this module: app-only tokens
/// get `422`, role-less users `403 {"error":"Forbidden"}`, and in-scope
/// failures `403` with the scopes message. Returns the actor and its role.
fn gate(
    auth: &Authenticated,
    store: &dyn Store,
    scopes: &[&str],
) -> ApiResult<(Id, UserRole)> {
    let store: &dyn AccountStore = store;
    let role = auth.require_admin(store, AdminArea::Reports, scopes)?;
    let viewer = auth.require_user()?;
    Ok((viewer, role))
}

/// Mastodon's ISO-8601 with milliseconds (`created_at`/`updated_at` on the wire).
fn iso(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

fn parse_id(raw: &str) -> ApiResult<Id> {
    raw.parse::<Id>().map_err(|_| ApiError::not_found())
}

fn parse_page(uri: &Uri) -> ApiResult<pagination::Page> {
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        100,
        200,
    )
}

/// Load an account + credential for serializer nesting (`None` account when
/// the row is gone; upstream would 500, but an absent row must not hide the
/// whole queue entry).
fn nest_account(
    store: &dyn Store,
    id: Id,
) -> ApiResult<(Option<Account>, Option<LocalUser>)> {
    let account = store.account_by_id(id).map_err(ApiError::from)?;
    let user = match &account {
        Some(_) => store.user_by_account_id(id).map_err(ApiError::from)?,
        None => None,
    };
    Ok((account, user))
}

/// `REST::Admin::ReportSerializer` (verified field list from
/// `app/serializers/rest/admin/report_serializer.rb`).
///
/// `statuses` are full `REST::StatusSerializer` rows for the moderator viewer.
/// `rules` renders the attached server rules (D37; upstream `Report#rules`
/// through `REST::RuleSerializer`: `id`/`text` here — no translations,
/// which have no model). Ids with no rule row are skipped (a deleted rule
/// leaves a stale id on the report, documented).
fn report_json(
    state: &ApiState,
    store: &dyn Store,
    actor: Id,
    report: &Report,
) -> ApiResult<Value> {
    let (reporter, reporter_user) = nest_account(store, report.reporter_id)?;
    let (target, target_user) = nest_account(store, report.target_account_id)?;
    let assigned = match report.assigned_account_id {
        Some(id) => nest_account(store, id)?.0,
        None => None,
    };
    let assigned_user = match report.assigned_account_id {
        Some(id) => store.user_by_account_id(id).map_err(ApiError::from)?,
        None => None,
    };
    let action_by = match report.action_taken_by_account_id {
        Some(id) => nest_account(store, id)?.0,
        None => None,
    };
    let action_by_user = match report.action_taken_by_account_id {
        Some(id) => store.user_by_account_id(id).map_err(ApiError::from)?,
        None => None,
    };
    let admin_json = |account: Option<Account>, user: Option<LocalUser>| match account {
        Some(account) => admin_account_json(&state.server, &account, user.as_ref()),
        None => Value::Null,
    };

    let statuses = store
        .statuses_by_ids(&report.status_ids)
        .map_err(ApiError::from)?;
    let now = state.context.now_ms();
    let views =
        load_status_views(&state.server, store, Some(actor), statuses, now).map_err(|err| err)?;
    let status_json: Vec<Value> = views
        .iter()
        .map(|view| {
            dto::status_json(
                &state.server,
                Some(actor),
                now,
                view,
                dto::StatusSourceMode::Content,
            )
        })
        .collect();

    let rule_ids: Vec<Id> = report
        .rule_ids
        .iter()
        .filter_map(|raw| raw.parse::<Id>().ok())
        .collect();
    let infra: &dyn AdminInfraStore = store;
    let rules = infra
        .admin_rules_by_ids(&rule_ids)
        .map_err(ApiError::from)?;
    let rules_json: Vec<Value> = rules
        .iter()
        .map(|rule| json!({ "id": rule.id.to_string(), "text": rule.text }))
        .collect();

    Ok(json!({
        "id": report.id.to_string(),
        "action_taken": report.action_taken,
        "action_taken_at": report.action_taken_at_ms.map(iso).map(Value::String).unwrap_or(Value::Null),
        "category": report.category,
        "comment": report.comment,
        "forwarded": report.forwarded,
        "created_at": iso(report.created_at_ms),
        "updated_at": iso(report.updated_at_ms),
        "account": admin_json(reporter, reporter_user),
        "target_account": admin_json(target, target_user),
        "assigned_account": admin_json(assigned, assigned_user),
        "action_taken_by_account": admin_json(action_by, action_by_user),
        "statuses": status_json,
        "rules": rules_json,
    }))
}

/// `GET /api/v1/admin/reports` — `resolved`/`unresolved`/`account_id`/
/// `target_account_id` filters with `Link` cursors.
pub async fn index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    let (actor, _) = match gate(&auth.0, &*state.context.store, READ_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let resolved = if forms::get(&pairs, "resolved").is_some_and(|v| !v.is_empty()) {
        Some(true)
    } else if forms::get(&pairs, "unresolved").is_some_and(|v| !v.is_empty()) {
        Some(false)
    } else {
        None
    };
    let reporter_id = match forms::get(&pairs, "account_id") {
        None => None,
        Some(raw) => match parse_id(raw) {
            Ok(id) => Some(id),
            Err(err) => return err.into_response(),
        },
    };
    let target_account_id = match forms::get(&pairs, "target_account_id") {
        None => None,
        Some(raw) => match parse_id(raw) {
            Ok(id) => Some(id),
            Err(err) => return err.into_response(),
        },
    };
    let store: &dyn Store = &*state.context.store;
    // Upstream's `to_a_paginated_by_id`: a present `min_id` pages ascending.
    let rows = match admin_uc::list_reports(
        store,
        AdminReportFilter {
            resolved,
            reporter_id,
            target_account_id,
        },
        page.max_id,
        page.since_id,
        page.min_id,
        page.min_id.is_some(),
        page.limit,
    ) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut body = Vec::with_capacity(rows.len());
    for report in &rows {
        match report_json(&state, store, actor, report) {
            Ok(value) => body.push(value),
            Err(err) => return err.into_response(),
        }
    }
    let next = (rows.len() == page.limit)
        .then(|| rows.last().map(|r| r.id))
        .flatten();
    let prev = rows.first().map(|r| r.id);
    let mut response = axum::Json(body).into_response();
    // Upstream omits pagination headers on an empty page.
    if !rows.is_empty() {
        if let Some(link) = pagination::link_header(&uri, next, prev) {
            if let Ok(value) = HeaderValue::from_str(&link) {
                response.headers_mut().insert(header::LINK, value);
            }
        }
    }
    response
}

/// `GET /api/v1/admin/reports/:id`.
pub async fn show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let (actor, _) = match gate(&auth.0, &*state.context.store, READ_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match store.report_by_id(id) {
        Ok(Some(report)) => match report_json(&state, store, actor, &report) {
            Ok(value) => axum::Json(value).into_response(),
            Err(err) => err.into_response(),
        },
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Shared update body for PUT and PATCH (`report_params`: `category`,
/// `rule_ids`).
pub async fn update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
    LenientForm(pairs): LenientForm,
) -> Response {
    let (actor, _) = match gate(&auth.0, &*state.context.store, WRITE_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let id = match parse_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let category = forms::get(&pairs, "category").map(str::to_string);
    let rule_ids: Option<Vec<String>> =
        if forms::all(&pairs, "rule_ids").is_empty() && forms::get(&pairs, "rule_ids").is_none() {
            None
        } else {
            Some(
                forms::all(&pairs, "rule_ids")
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )
        };
    if let Err(err) =
        admin_uc::check_report_patch(category.as_deref(), rule_ids.as_deref())
    {
        return ApiError::from(err).into_response();
    }
    // D37: attached rule ids must reference existing rules (upstream
    // `validate_rule_ids`); unknown ids are refused `422`.
    if let Some(ids) = rule_ids.as_deref() {
        if !ids.is_empty() {
            let infra: &dyn AdminInfraStore = store;
            if let Err(err) = admin_uc::load_rules(infra, ids) {
                return ApiError::from(err).into_response();
            }
        }
    }
    let patch = AdminReportPatch {
        category,
        rule_ids,
        assigned_account_id: None,
        action_taken: None,
        action_taken_by_account_id: None,
        audit_action: "update".to_string(),
    };
    match admin_uc::update_report(store, actor, id, patch, state.context.now_ms()) {
        Ok(Some(report)) => match report_json(&state, store, actor, &report) {
            Ok(value) => axum::Json(value).into_response(),
            Err(err) => err.into_response(),
        },
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/admin/reports/:id/assign_to_self`.
pub async fn assign_to_self(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    transition(&state, &auth, &id, |actor| AdminReportPatch {
        assigned_account_id: Some(Some(actor)),
        action_taken: None,
        action_taken_by_account_id: None,
        category: None,
        rule_ids: None,
        audit_action: "assigned_to_self".to_string(),
    })
    .await
}

/// `POST /api/v1/admin/reports/:id/unassign`.
pub async fn unassign(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    transition(&state, &auth, &id, |_| AdminReportPatch {
        assigned_account_id: Some(None),
        action_taken: None,
        action_taken_by_account_id: None,
        category: None,
        rule_ids: None,
        audit_action: "unassigned".to_string(),
    })
    .await
}

/// `POST /api/v1/admin/reports/:id/reopen`.
pub async fn reopen(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    transition(&state, &auth, &id, |_| AdminReportPatch {
        assigned_account_id: None,
        action_taken: Some(false),
        action_taken_by_account_id: None,
        category: None,
        rule_ids: None,
        audit_action: "reopen".to_string(),
    })
    .await
}

/// `POST /api/v1/admin/reports/:id/resolve`.
pub async fn resolve(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    transition(&state, &auth, &id, |actor| AdminReportPatch {
        assigned_account_id: None,
        action_taken: Some(true),
        action_taken_by_account_id: Some(actor),
        category: None,
        rule_ids: None,
        audit_action: "resolve".to_string(),
    })
    .await
}

async fn transition(
    state: &ApiState,
    auth: &Auth,
    raw: &str,
    patch: impl FnOnce(Id) -> AdminReportPatch,
) -> Response {
    let (actor, _) = match gate(&auth.0, &*state.context.store, WRITE_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match admin_uc::update_report(store, actor, id, patch(actor), state.context.now_ms()) {
        Ok(Some(report)) => match report_json(state, store, actor, &report) {
            Ok(value) => axum::Json(value).into_response(),
            Err(err) => err.into_response(),
        },
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

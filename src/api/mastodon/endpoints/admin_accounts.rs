//! Admin account moderation (`GET /api/v1/admin/accounts*`,
//! `GET /api/v2/admin/accounts`, and the per-account member actions).
//!
//! Contracts follow Mastodon v4.7.3
//! (`app/controllers/api/v1/admin/accounts_controller.rb` and
//! `app/controllers/api/v2/admin/accounts_controller.rb`):
//! user token + moderator-or-admin role (role check before scope check),
//! `admin:read`/`admin:read:accounts` for reads and
//! `admin:write`/`admin:write:accounts` for writes, `Link` cursors, and
//! `REST::Admin::AccountSerializer` rows.
//!
//! The role gate lives in [`gate`] (one small function; if subagent A's
//! `Authenticated::require_admin` shape changes, only it needs an edit).

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::{Auth, Authenticated};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::pagination;
use crate::api::mastodon::ApiState;
use crate::application::admin as admin_uc;
use crate::application::ports::{AccountStore, AdminAccountFilter, AdminAccountPatch, Store};
use crate::domain::user::AdminAction;
use crate::domain::{Account, Id, LocalUser, UserRole};

const READ_SCOPES: &[&str] = &["admin:read", "admin:read:accounts"];
const WRITE_SCOPES: &[&str] = &["admin:write", "admin:write:accounts"];

/// Role-before-scope gate for every route in this module: app-only tokens
/// get `422`, role-less users `403 {"error":"Forbidden"}`, and in-scope
/// failures `403` with the scopes message. Returns the actor and its role.
///
/// D35: the gate is per-action ([`AdminAction`]), checked against the
/// caller's upstream permission bitmask before the scope check — the same
/// order as upstream's `PermissionsConcern` → controller `authorize`.
fn gate_action(
    auth: &Authenticated,
    store: &dyn Store,
    action: AdminAction,
    scopes: &[&str],
) -> ApiResult<(Id, UserRole)> {
    let store: &dyn AccountStore = store;
    let role = auth.require_admin_action(store, action, scopes)?;
    let viewer = auth.require_user()?;
    Ok((viewer, role))
}

/// Mastodon's ISO-8601 with milliseconds (`created_at` on the wire).
pub(crate) fn iso(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

fn opt_or_null(value: Option<String>) -> Value {
    value.map(Value::String).unwrap_or(Value::Null)
}

/// `REST::Admin::AccountSerializer` (verified field list from
/// `app/serializers/rest/admin/account_serializer.rb`).
///
/// `role` carries the stable numeric id (`0` = everyone/`User`, `1` =
/// `Moderator`, `2` = `Admin` — never `-1`) and the upstream permission
/// bitmask as a decimal string (`0` for non-staff, `1049888` for
/// moderators, the full set for admins).
///
/// Served since the fix round: `ip` (latest login IP or `null`), `ips`
/// (`[{ip, used_at}]`, newest first), `locale`, `invite_request`,
/// `created_by_application_id`, `invited_by_account_id` (all `null` when the
/// credential/attribution row is absent — e.g. remote accounts).
pub fn admin_account_json(
    server: &crate::api::mastodon::ServerInfo,
    store: &dyn Store,
    invites: &dyn crate::application::accounts::InviteStore,
    account: &Account,
    user: Option<&LocalUser>,
) -> Value {
    let role = user.map(|u| u.role).unwrap_or(UserRole::None);
    let (role_id, role_name) = match role {
        UserRole::Admin => (2u64, "Admin"),
        UserRole::Moderator => (1u64, "Moderator"),
        UserRole::None => (0u64, "User"),
    };
    // Login IPs, newest first (upstream `user.ips`, `IpSerializer`
    // `ip, used_at`); `ip` is the latest entry. Store errors degrade to the
    // absent shape rather than failing the row.
    let ips: Vec<Value> = store
        .login_ips_for(account.id, 16)
        .map(|rows| {
            rows.iter()
                .map(|(ip, used_at)| {
                    json!({ "ip": ip.to_string(), "used_at": iso(*used_at) })
                })
                .collect()
        })
        .unwrap_or_default();
    // Invite attribution: the code consumed at registration resolves to its
    // issuing account (upstream `user.invite.user.account_id`). Expiry and
    // use-counts don't matter for attribution — only the issuer does — so
    // this reads the row, not the usability gate.
    let invited_by_account_id = store
        .invite_code_for_account(account.id)
        .ok()
        .flatten()
        .and_then(|code| invites.invite_by_code(&code).ok().flatten())
        .map(|invite| invite.created_by_account_id.to_string());
    json!({
        "id": account.id.to_string(),
        "username": account.username,
        "domain": account.domain,
        "created_at": iso(account.created_at_ms),
        "email": opt_or_null(user.and_then(|u| u.email.clone())),
        "ip": ips.first().and_then(|entry| entry.get("ip").cloned()).unwrap_or(Value::Null),
        "confirmed": user.map(|u| json!(u.email_verified)).unwrap_or(Value::Null),
        "suspended": account.suspended,
        "silenced": account.silenced,
        "sensitized": account.sensitized,
        "disabled": user.map(|u| json!(u.disabled)).unwrap_or(Value::Null),
        "approved": user.map(|u| json!(u.approved)).unwrap_or(Value::Null),
        "locale": opt_or_null(user.and_then(|u| u.locale.clone())),
        "invite_request": opt_or_null(user.and_then(|u| u.invite_request.clone())),
        "created_by_application_id": opt_or_null(user.and_then(|u| u.created_by_application.map(|id| id.to_string()))),
        "invited_by_account_id": invited_by_account_id.map(Value::String).unwrap_or(Value::Null),
        "ips": ips,
        "account": dto::account_json(server, account),
        "role": {
            "id": role_id,
            "name": role_name,
            "permissions": role.permissions().to_string(),
            "color": "",
            "highlighted": role.is_staff(),
        },
    })
}

fn parse_id(raw: &str) -> ApiResult<Id> {
    raw.parse::<Id>().map_err(|_| ApiError::not_found())
}

/// Translate one v2 `role_ids` filter token to the role name the store
/// filter matches: the numeric serializer ids (`0`/`1`/`2`) map to their
/// names, and legacy name tokens pass through unchanged (compat with
/// clients that cached the pre-numeric shape).
fn translate_role_id(raw: &str) -> String {
    match raw.trim() {
        "0" => "user".to_string(),
        "1" => "moderator".to_string(),
        "2" => "admin".to_string(),
        other => other.to_string(),
    }
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

/// Presence check (non-empty value), matching Rails' `present?`: upstream
/// treats `?local=false` as present.
fn present(pairs: &[(String, String)], key: &str) -> bool {
    forms::get(pairs, key).is_some_and(|v| !v.is_empty())
}

/// Shared index body: filter, cursor, page, `Link`, serialize.
fn index_response(
    state: &ApiState,
    uri: &Uri,
    page: pagination::Page,
    filter: AdminAccountFilter,
) -> Response {
    let store: &dyn Store = &*state.context.store;
    // Upstream's `to_a_paginated_by_id`: a present `min_id` pages ascending.
    let ascending = page.min_id.is_some();
    let rows = match admin_uc::list_accounts(
        store,
        filter,
        page.max_id,
        page.since_id,
        page.min_id,
        ascending,
        page.limit,
    ) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let body: Vec<Value> = rows
        .iter()
        .map(|(account, user)| admin_account_json(&state.server, store, &*state.invites, account, user.as_ref()))
        .collect();
    let next = (rows.len() == page.limit)
        .then(|| rows.last().map(|(a, _)| a.id))
        .flatten();
    let prev = rows.first().map(|(a, _)| a.id);
    let mut response = axum::Json(body).into_response();
    // Upstream omits pagination headers on an empty page.
    if !rows.is_empty() {
        if let Some(link) = pagination::link_header(uri, next, prev) {
            if let Ok(value) = HeaderValue::from_str(&link) {
                response.headers_mut().insert(header::LINK, value);
            }
        }
    }
    response
}

/// `GET /api/v1/admin/accounts` — upstream v1 filter flags.
pub async fn index_v1(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = gate_action(&auth.0, &*state.context.store, AdminAction::ViewAccounts, READ_SCOPES) {
        return err.into_response();
    }
    let page = match parse_page(&uri) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    // Upstream translation: default `origin=local, status=active`; `remote`
    // flips the origin; each present status flag wins in order.
    let mut status = Some(admin_uc::parse_status(Some("active")));
    for flag in ["active", "pending", "disabled", "silenced", "suspended"] {
        if present(&pairs, flag) {
            status = Some(admin_uc::parse_status(Some(flag)));
        }
    }
    let status = match status {
        Some(Some(s)) => Some(s),
        _ => None,
    };
    // Upstream default `origin=local`; a present `remote` flag flips it.
    let local_only = Some(!present(&pairs, "remote"));
    index_response(
        &state,
        &uri,
        page,
        AdminAccountFilter {
            local_only,
            by_domain: forms::get(&pairs, "by_domain").map(str::to_string),
            status,
            username_sub: forms::get(&pairs, "username").map(str::to_string),
            display_name_sub: forms::get(&pairs, "display_name").map(str::to_string),
            email_sub: forms::get(&pairs, "email").map(str::to_string),
            ip_sub: forms::get(&pairs, "ip").map(str::to_string),
            staff_only: present(&pairs, "staff"),
            role_names: Vec::new(),
            invited_by: None,
        },
    )
}

/// `GET /api/v2/admin/accounts` — upstream v2 filter names.
pub async fn index_v2(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = gate_action(&auth.0, &*state.context.store, AdminAction::ViewAccounts, READ_SCOPES) {
        return err.into_response();
    }
    let page = match parse_page(&uri) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let local_only = match forms::get(&pairs, "origin") {
        Some("local") => Some(true),
        Some("remote") => Some(false),
        _ => None,
    };
    // `invited_by`: upstream `AccountFilter#invited_by` joins invites by
    // user id — absent/empty means no constraint, while a present garbage
    // value matches nothing (empty page, never an error, never unrelated
    // rows).
    let invited_by = match forms::get(&pairs, "invited_by").filter(|v| !v.is_empty()) {
        None => None,
        Some(raw) => match admin_uc::parse_invited_by(raw) {
            Some(id) => Some(id),
            None => return axum::Json(serde_json::json!([])).into_response(),
        },
    };
    let filter = AdminAccountFilter {
        local_only,
        by_domain: forms::get(&pairs, "by_domain").map(str::to_string),
        status: admin_uc::parse_status(forms::get(&pairs, "status")),
        username_sub: forms::get(&pairs, "username").map(str::to_string),
        display_name_sub: forms::get(&pairs, "display_name").map(str::to_string),
        email_sub: forms::get(&pairs, "email").map(str::to_string),
        ip_sub: forms::get(&pairs, "ip").map(str::to_string),
        staff_only: forms::get(&pairs, "permissions") == Some("staff"),
        role_names: forms::all(&pairs, "role_ids")
            .into_iter()
            .map(|raw| translate_role_id(&raw))
            .collect(),
        invited_by,
    };
    index_response(&state, &uri, page, filter)
}

async fn load_target(
    store: &dyn Store,
    raw: &str,
) -> Result<(Account, Option<LocalUser>), ApiError> {
    let id = parse_id(raw)?;
    let account = store
        .account_by_id(id)
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::not_found)?;
    let user = store.user_by_account_id(id).map_err(ApiError::from)?;
    Ok((account, user))
}

fn account_response(
    state: &ApiState,
    store: &dyn Store,
    account: &Account,
    user: Option<&LocalUser>,
) -> Response {
    axum::Json(admin_account_json(&state.server, store, &*state.invites, account, user))
        .into_response()
}

/// `GET /api/v1/admin/accounts/:id`.
pub async fn show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = gate_action(&auth.0, &*state.context.store, AdminAction::ViewAccounts, READ_SCOPES) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    match load_target(store, &id).await {
        Ok((account, user)) => account_response(&state, store, &account, user.as_ref()),
        Err(err) => err.into_response(),
    }
}

/// `DELETE /api/v1/admin/accounts/:id` — immediate suspend + disable (no
/// async deletion worker yet; statuses stay — a planned gap). Answers
/// `200 {}` like upstream's `render_empty`. D24: the target's tokens are
/// revoked and its open streams close.
pub async fn destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let (actor, _) = match gate_action(&auth.0, &*state.context.store, AdminAction::ModerateAccount, WRITE_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match parse_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    match store.admin_delete_account(actor, target, "destroy", state.context.now_ms()) {
        Ok(true) => {
            kill_tokens(&state, target);
            axum::Json(json!({})).into_response()
        }
        Ok(false) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// D24 follow-up: revoke the account's tokens and close its open streams.
/// Runs after the suspension/disablement commit; a failure is logged, never
/// reported as an admin error — the moderation itself already committed.
fn kill_tokens(state: &ApiState, target: Id) {
    let store: &dyn Store = &*state.context.store;
    match admin_uc::kill_account_tokens(store, &*state.context.streaming, target) {
        Ok(0) => {}
        Ok(revoked) => {
            tracing::info!(account = %target, revoked, "killed tokens on suspension/disablement");
        }
        Err(err) => {
            tracing::warn!(account = %target, error = ?err, "token kill failed after suspension/disablement");
        }
    }
}

async fn patch_flag(
    state: ApiState,
    auth: Auth,
    raw: &str,
    audit_action: &'static str,
    patch: AdminAccountPatch,
    local_only: bool,
) -> Response {
    let (actor, _) = match gate_action(&auth.0, &*state.context.store, AdminAction::ModerateAccount, WRITE_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let (account, user) = match load_target(store, raw).await {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    if local_only {
        if let Err(err) = admin_uc::require_local_target(&account, user.as_ref()) {
            return ApiError::from(err).into_response();
        }
    }
    let mut patch = patch;
    patch.audit_action = audit_action.to_string();
    let kills = patch.suspended == Some(true) || patch.disabled == Some(true);
    match admin_uc::moderate_account(store, actor, account.id, patch, state.context.now_ms()) {
        Ok((account, user)) => {
            if kills {
                kill_tokens(&state, account.id);
            }
            account_response(&state, store, &account, user.as_ref())
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/admin/accounts/:id/enable` — local accounts only.
pub async fn enable(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    patch_flag(
        state,
        auth,
        &id,
        "enable",
        AdminAccountPatch {
            disabled: Some(false),
            ..Default::default()
        },
        true,
    )
    .await
}

/// `POST /api/v1/admin/accounts/:id/unsensitive`.
pub async fn unsensitive(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    patch_flag(
        state,
        auth,
        &id,
        "unsensitive",
        AdminAccountPatch {
            sensitized: Some(false),
            ..Default::default()
        },
        false,
    )
    .await
}

/// `POST /api/v1/admin/accounts/:id/unsilence`.
pub async fn unsilence(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    patch_flag(
        state,
        auth,
        &id,
        "unsilence",
        AdminAccountPatch {
            silenced: Some(false),
            ..Default::default()
        },
        false,
    )
    .await
}

/// `POST /api/v1/admin/accounts/:id/unsuspend` — applied immediately (no
/// async unsuspension worker yet) with the same audit row.
pub async fn unsuspend(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    patch_flag(
        state,
        auth,
        &id,
        "unsuspend",
        AdminAccountPatch {
            suspended: Some(false),
            ..Default::default()
        },
        false,
    )
    .await
}

/// `POST /api/v1/admin/accounts/:id/approve` — local accounts only.
///
/// Upstream `User#approve!`: re-enable when disabled, set `approved`, and
/// run the new-user setup when the e-mail is confirmed. The re-enable and
/// the approval flag commit here with an `approve` audit row; the
/// new-user setup (bootstrap timeline, welcome mail, `account.approved`
/// webhook) stays dormant until the Phase 2 email-confirmation gap closes,
/// because it is gated on confirmation upstream.
///
/// Like upstream's `UserPolicy#approve?`, an already-approved account is
/// refused `403 {"error":"Forbidden"}`.
pub async fn approve(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let (actor, _) = match gate_action(&auth.0, &*state.context.store, AdminAction::ModerateAccount, WRITE_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let (account, user) = match load_target(store, &id).await {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    if let Err(err) = admin_uc::require_local_target(&account, user.as_ref()) {
        return ApiError::from(err).into_response();
    }
    if user.is_some_and(|u| u.approved) {
        return ApiError::role_forbidden().into_response();
    }
    match admin_uc::moderate_account(
        store,
        actor,
        account.id,
        AdminAccountPatch {
            disabled: Some(false),
            approved: Some(true),
            audit_action: "approve".to_string(),
            ..Default::default()
        },
        state.context.now_ms(),
    ) {
        Ok((account, user)) => account_response(&state, store, &account, user.as_ref()),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/admin/accounts/:id/reject` — local accounts only.
///
/// Upstream runs `DeleteAccountService` (the account is gone); here the
/// account is suspended + disabled immediately (same as DELETE) and its
/// statuses are removed by the D38 async deletion worker with federated
/// `Delete` distribution. Answers `200 {}` like upstream's `render_empty`.
/// D24: the target's tokens are revoked and its open streams close.
///
/// Like upstream's `UserPolicy#reject?`, an already-approved account is
/// refused `403 {"error":"Forbidden"}`.
pub async fn reject(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let (actor, _) = match gate_action(&auth.0, &*state.context.store, AdminAction::ModerateAccount, WRITE_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let (account, user) = match load_target(store, &id).await {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    if let Err(err) = admin_uc::require_local_target(&account, user.as_ref()) {
        return ApiError::from(err).into_response();
    }
    if user.is_some_and(|u| u.approved) {
        return ApiError::role_forbidden().into_response();
    }
    match store.admin_delete_account(actor, account.id, "reject", state.context.now_ms()) {
        Ok(true) => {
            kill_tokens(&state, account.id);
            axum::Json(json!({})).into_response()
        }
        Ok(false) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/admin/accounts/:account_id/action` — `Admin::AccountAction`
/// types `none`/`disable`/`sensitive`/`silence`/`suspend` (verified against
/// `app/models/admin/account_action.rb`). Answers `200 {}` like upstream's
/// `render_empty`.
///
/// Every action records a warning row (strike: author, target, linked
/// report, action, text, status ids — D37, upstream `process_strike!`); a
/// `none` action without `report_id` stays refused `422` (there is nothing
/// to warn about and nothing to resolve). Warning e-mail delivery has no
/// mailer and is dropped (documented); the `send_email_notification` flag
/// is accepted but changes nothing. Warning presets have no model: a
/// `warning_preset_id` is refused `404` like a missing preset row.
pub async fn action(
    State(state): State<ApiState>,
    auth: Auth,
    Path(account_id): Path<String>,
    LenientForm(pairs): LenientForm,
) -> Response {
    let (actor, _) = match gate_action(&auth.0, &*state.context.store, AdminAction::ModerateAccount, WRITE_SCOPES) {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let (account, user) = match load_target(store, &account_id).await {
        Ok(pair) => pair,
        Err(err) => return err.into_response(),
    };
    let Some(action_type) = forms::get(&pairs, "type") else {
        return ApiError::unprocessable("Validation failed: Type can't be blank").into_response();
    };
    let action_type = action_type.to_string();
    let local = account.local && user.is_some();
    if let Err(err) = admin_uc::check_action_type(&action_type, local) {
        return ApiError::from(err).into_response();
    }
    let report_id = match forms::get(&pairs, "report_id") {
        None => None,
        Some(raw) => match parse_id(raw) {
            Ok(id) => Some(id),
            Err(err) => return err.into_response(),
        },
    };
    // D37 warning text (the moderator's message on the strike) and the
    // linked report. A warning preset id names a row of a model that does
    // not exist here, so any value is a missing record (404).
    let text = forms::get(&pairs, "text").map(str::to_string);
    if forms::get(&pairs, "warning_preset_id").is_some_and(|v| !v.is_empty()) {
        return ApiError::not_found().into_response();
    }
    match admin_uc::account_action(
        store,
        actor,
        account.id,
        &action_type,
        report_id,
        text,
        state.context.now_ms(),
    ) {
        Ok(()) => {
            // D24: only `disable` and `suspend` remove access; the other
            // action types leave tokens alone.
            if action_type == "disable" || action_type == "suspend" {
                kill_tokens(&state, account.id);
            }
            axum::Json(json!({})).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

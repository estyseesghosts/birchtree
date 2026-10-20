//! Admin infrastructure routes (Phase 7.3, subagent C).
//!
//! Domain allows/blocks, email-domain blocks, IP blocks (with request-path
//! enforcement through the trusted-proxy client IP), canonical email blocks
//! (including the read-scoped `test` action), admin tag flags, and the
//! read-scoped measures/dimensions/retention analytics.
//!
//! Auth: user token + role gate (checked **before** the scope check).
//! Reads take `admin:read` (or the listed sub-scope); writes take
//! `admin:write` (or the listed sub-scope). `measures`, `dimensions`,
//! `retention` and canonical-email `test` take the read scope upstream, kept
//! here.
//!
//! Role split: most routes admit **admins only** ([`require_admin`]);
//! `measures`, `dimensions` and `retention` additionally admit **moderators**
//! ([`require_metrics`]) — the P3 operator decision (moderators read
//! analytics; upstream admits them through the dashboard policy, see the
//! helper). Tags stay admin-only (the finer D35 roadmap split is intact:
//! moderators are still refused everywhere else in this module).
//!
//! Enforcement gaps (recorded in `docs/endpoint-manifest.md`, never stubbed):
//! domain blocks wait for federation (Phase 6); email-domain and canonical
//! email blocks wait for email confirmation (Phase 2 gap); IP `no_access`
//! enforcement and `sign_up_block` registration refusal are provided as
//! [`ip_block_middleware`] / [`is_sign_up_blocked_for`] and wired by the
//! integrator (this module cannot touch `router.rs`).
//!
//! Every write appends one [`crate::domain::AdminAuditRecord`] (proposed
//! default: audit on).

use std::collections::HashMap;
use std::net::IpAddr;

use chrono::Datelike;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::{AdminArea, Auth, Authenticated};
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::pagination;
use crate::api::mastodon::ApiState;
use crate::application::admin_infra as use_cases;
use crate::application::ports::{
    AdminDomainBlockPatch, AdminIpBlockPatch, AdminTagPatch, StatusMetricRow, Store, TimelineFilter,
    TimelineQuery,
};
use crate::domain::{
    AdminAccountOverview, AdminCanonicalEmailBlock, AdminDomainAllow, AdminDomainBlock,
    AdminDomainSeverity, AdminEmailDomainBlock, AdminIpBlock, AdminIpSeverity, AdminMediaOwner,
    AdminReportTarget, AdminTag, FollowMetricRow, Id, Status, UserRole,
};

/// Scope alternatives for admin reads of a sub-scoped resource.
macro_rules! read_scopes {
    ($sub:literal) => {
        &["admin:read", $sub][..]
    };
}
/// Scope alternatives for admin writes of a sub-scoped resource.
macro_rules! write_scopes {
    ($sub:literal) => {
        &["admin:write", $sub][..]
    };
}

/// The admin-only role gate for this module: user token, then **admin role
/// only**, then scopes. Wraps [`Authenticated::require_admin`] pinned to
/// [`AdminArea::AdminOnly`]; rename here if the permission split changes.
/// (`measures`, `dimensions` and `retention` use [`require_metrics`]
/// instead so moderators pass.)
fn require_admin(auth: &Authenticated, store: &dyn Store, scopes: &[&str]) -> ApiResult<Id> {
    auth.require_admin(store, AdminArea::AdminOnly, scopes)?;
    auth.require_user()
}

/// The analytics role gate (`measures`, `dimensions`, `retention` only):
/// user token, then **admin or moderator** role, then scopes.
///
/// Upstream admits moderators here. `Admin::PermissionsConcern`
/// (`app/controllers/concerns/admin/permissions_concern.rb` at `8ff11c2b`)
/// only requires a moderator-or-admin role (`Admin::BasePolicy#access?`,
/// `app/policies/admin/base_policy.rb:1-6`: administrator, or any
/// moderation/administration/devops flag), and the per-action
/// `authorize :dashboard, :index?` (`measures_controller.rb:13`,
/// `dimensions_controller.rb:15`, `retention_controller.rb:15`) checks
/// `DashboardPolicy#index?` (`app/policies/dashboard_policy.rb:5-7`:
/// `role.can?(:view_dashboard)`), which the default moderator holds
/// (moderation category, `app/models/user_role.rb:64-77`). Birchtree roles
/// carry no permission bitmask, so admin-or-moderator is the faithful
/// coarse mapping. Every other route in this module keeps [`require_admin`].
fn require_metrics(auth: &Authenticated, store: &dyn Store, scopes: &[&str]) -> ApiResult<Id> {
    let user_id = auth.require_user()?;
    let role = store
        .user_by_account_id(user_id)
        .map_err(ApiError::from)?
        .map(|user| user.role)
        .unwrap_or(UserRole::None);
    match role {
        UserRole::Admin | UserRole::Moderator => {}
        UserRole::None => return Err(ApiError::role_forbidden()),
    }
    auth.require_scopes(scopes)?;
    Ok(user_id)
}

/// Route fragment for the integrator (`router.rs` mounts it; this module must
/// not edit `router.rs` itself):
///
/// ```ignore
/// .merge(crate::api::mastodon::endpoints::admin_infra::routes())
/// ```
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/admin/domain_allows", get(domain_allows_index).post(domain_allows_create))
        .route("/api/v1/admin/domain_allows/:id", get(domain_allows_show).delete(domain_allows_destroy))
        .route("/api/v1/admin/domain_blocks", get(domain_blocks_index).post(domain_blocks_create))
        .route(
            "/api/v1/admin/domain_blocks/:id",
            get(domain_blocks_show).put(domain_blocks_update).patch(domain_blocks_update).delete(domain_blocks_destroy),
        )
        .route("/api/v1/admin/email_domain_blocks", get(email_domain_blocks_index).post(email_domain_blocks_create))
        .route("/api/v1/admin/email_domain_blocks/:id", get(email_domain_blocks_show).delete(email_domain_blocks_destroy))
        .route("/api/v1/admin/ip_blocks", get(ip_blocks_index).post(ip_blocks_create))
        .route(
            "/api/v1/admin/ip_blocks/:id",
            get(ip_blocks_show).put(ip_blocks_update).patch(ip_blocks_update).delete(ip_blocks_destroy),
        )
        .route("/api/v1/admin/canonical_email_blocks", get(canonical_email_blocks_index).post(canonical_email_blocks_create))
        .route("/api/v1/admin/canonical_email_blocks/test", post(canonical_email_blocks_test))
        .route("/api/v1/admin/canonical_email_blocks/:id", get(canonical_email_blocks_show).delete(canonical_email_blocks_destroy))
        .route("/api/v1/admin/tags", get(tags_index))
        .route("/api/v1/admin/tags/:id", get(tags_show).put(tags_update).patch(tags_update))
        .route("/api/v1/admin/measures", post(measures))
        .route("/api/v1/admin/dimensions", post(dimensions))
        .route("/api/v1/admin/retention", post(retention))
}

// ---------------------------------------------------------------------------
// DTO packers (upstream `REST::Admin::*` serializers).
// ---------------------------------------------------------------------------

fn rfc3339_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

fn date_only(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.date_naive().to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

fn domain_allow_json(row: &AdminDomainAllow) -> Value {
    json!({
        "id": row.id.to_string(),
        "domain": row.domain,
        "created_at": rfc3339_ms(row.created_at_ms),
    })
}

fn sha256_hex_domain(domain: &str) -> String {
    crate::application::crypto::sha256_hex(domain.as_bytes())
}

fn domain_block_json(row: &AdminDomainBlock) -> Value {
    json!({
        "id": row.id.to_string(),
        "domain": row.domain,
        "digest": sha256_hex_domain(&row.domain),
        "created_at": rfc3339_ms(row.created_at_ms),
        "severity": row.severity.as_str(),
        "reject_media": row.reject_media,
        "reject_reports": row.reject_reports,
        "private_comment": row.private_comment.clone().map(Value::String).unwrap_or(Value::Null),
        "public_comment": row.public_comment.clone().map(Value::String).unwrap_or(Value::Null),
        "obfuscate": row.obfuscate,
    })
}

fn email_domain_block_json(row: &AdminEmailDomainBlock) -> Value {
    json!({
        "id": row.id.to_string(),
        "domain": row.domain,
        "created_at": rfc3339_ms(row.created_at_ms),
        "history": [],
        "allow_with_approval": row.allow_with_approval,
    })
}

fn ip_block_json(row: &AdminIpBlock) -> Value {
    json!({
        "id": row.id.to_string(),
        "ip": row.ip,
        "severity": row.severity.as_str(),
        "comment": row.comment,
        "created_at": rfc3339_ms(row.created_at_ms),
        "expires_at": row.expires_at_ms.map(rfc3339_ms).map(Value::String).unwrap_or(Value::Null),
    })
}

fn canonical_email_block_json(row: &AdminCanonicalEmailBlock) -> Value {
    json!({
        "id": row.id.to_string(),
        "canonical_email_hash": row.canonical_email_hash,
    })
}

fn admin_tag_json(server_public_url: &str, row: &AdminTag) -> Value {
    let base = server_public_url.trim_end_matches('/');
    json!({
        "id": row.id.to_string(),
        "name": row.name,
        "url": format!("{base}/tags/{}", row.name),
        "history": [],
        "following": false,
        "featuring": false,
        "display_name": row.display_name,
        "trendable": row.trendable.map(Value::from).unwrap_or(Value::Null),
        "usable": row.usable.map(Value::from).unwrap_or(Value::Null),
        "requires_review": row.reviewed_at_ms.is_none(),
        "listable": row.listable.map(Value::from).unwrap_or(Value::Null),
    })
}

// ---------------------------------------------------------------------------
// Shared request plumbing.
// ---------------------------------------------------------------------------

fn parse_page(uri: &Uri, default: usize, max: usize) -> ApiResult<pagination::Page> {
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        default,
        max,
    )
}

/// Human-readable size like Rails' `number_to_human_size` (used for
/// `human_value` on byte measures).
fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["Bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}


/// Paginate store rows newest-first and render them with `render`.
fn paged<T>(
    uri: &Uri,
    default: usize,
    max: usize,
    fetch: impl FnOnce(Option<Id>, Option<Id>, Option<Id>, usize) -> crate::domain::Result<Vec<T>>,
    render: impl Fn(&T) -> Value,
    id_of: impl Fn(&T) -> Id,
) -> Response {
    let page = match parse_page(uri, default, max) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    if page.limit == 0 {
        return Json(Vec::<Value>::new()).into_response();
    }
    let rows = match fetch(page.max_id, page.since_id, page.min_id, page.limit) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let full = rows.len() == page.limit;
    let next = if full { rows.last().map(&id_of) } else { None };
    let prev = if rows.is_empty() { None } else { rows.first().map(&id_of) };
    let body: Vec<Value> = rows.iter().map(&render).collect();
    let mut response = Json(body).into_response();
    if let Some(link) = pagination::link_header_desc(uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

fn require_id(raw: &str) -> ApiResult<Id> {
    raw.parse::<Id>().map_err(|_| ApiError::not_found())
}

fn bool_cast(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "1" | "t" | "yes" | "on")
}

/// One audit record per admin write (proposed default: on). Failures to
/// append the audit row fail the request: an unaudited admin write must not
/// look successful.
fn audit(state: &ApiState, actor: Id, action: &str, target_type: &str, target_id: &str) -> ApiResult<()> {
    let store: &dyn Store = &*state.context.store;
    store
        .admin_audit(actor, action, target_type, target_id, state.context.now_ms())
        .map_err(ApiError::from)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Domain allows.
// ---------------------------------------------------------------------------

pub async fn domain_allows_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:domain_allows")) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(&uri, 100, 500, |max, since, min, limit| store.admin_domain_allows_page(max, since, min, limit), domain_allow_json, |row| row.id)
}

pub async fn domain_allows_show(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:domain_allows")) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_domain_allow_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(domain_allow_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn domain_allows_create(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:domain_allows")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let domain = forms::get(&body, "domain").unwrap_or("");
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_domain_allow(domain, state.context.now_ms()) {
        Ok(row) => {
            // One audit record per admin write, dupes included.
            if let Err(err) = audit(&state, actor, "create", "DomainAllow", &row.id.to_string()) {
                return err.into_response();
            }
            Json(domain_allow_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn domain_allows_destroy(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:domain_allows")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_delete_domain_allow(id).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "destroy", "DomainAllow", &row.id.to_string()) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Domain blocks.
// ---------------------------------------------------------------------------

fn domain_block_patch(body: &[(String, String)]) -> AdminDomainBlockPatch {
    AdminDomainBlockPatch {
        severity: forms::get(body, "severity").and_then(AdminDomainSeverity::parse),
        reject_media: forms::get(body, "reject_media").map(bool_cast),
        reject_reports: forms::get(body, "reject_reports").map(bool_cast),
        private_comment: forms::get(body, "private_comment").map(|s| {
            if s.is_empty() { None } else { Some(s.to_string()) }
        }),
        public_comment: forms::get(body, "public_comment").map(|s| {
            if s.is_empty() { None } else { Some(s.to_string()) }
        }),
        obfuscate: forms::get(body, "obfuscate").map(bool_cast),
    }
}

pub async fn domain_blocks_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:domain_blocks")) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(&uri, 100, 500, |max, since, min, limit| store.admin_domain_blocks_page(max, since, min, limit), domain_block_json, |row| row.id)
}

pub async fn domain_blocks_show(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:domain_blocks")) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_domain_block_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(domain_block_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn domain_blocks_create(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:domain_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let domain = forms::get(&body, "domain").unwrap_or("");
    let patch = domain_block_patch(&body);
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_domain_block(domain, patch, state.context.now_ms()) {
        Ok(row) => {
            if let Err(err) = audit(&state, actor, "create", "DomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            // Federation enforcement (Phase 6) picks the row up from the
            // store; until then the row is stored but not enforced.
            Json(domain_block_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn domain_blocks_update(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:domain_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let patch = domain_block_patch(&body);
    let store: &dyn Store = &*state.context.store;
    match store.admin_update_domain_block(id, patch, state.context.now_ms()).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "update", "DomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(domain_block_json(&row)).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn domain_blocks_destroy(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:domain_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_delete_domain_block(id).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "destroy", "DomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Email-domain blocks.
// ---------------------------------------------------------------------------

pub async fn email_domain_blocks_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:email_domain_blocks")) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(
        &uri,
        100,
        200,
        |max, since, min, limit| store.admin_email_domain_blocks_page(max, since, min, limit),
        email_domain_block_json,
        |row| row.id,
    )
}

pub async fn email_domain_blocks_show(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:email_domain_blocks")) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_email_domain_block_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(email_domain_block_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn email_domain_blocks_create(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:email_domain_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let domain = forms::get(&body, "domain").unwrap_or("");
    let allow_with_approval = forms::get(&body, "allow_with_approval").is_some_and(bool_cast);
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_email_domain_block(domain, allow_with_approval, state.context.now_ms()) {
        Ok(row) => {
            if let Err(err) = audit(&state, actor, "create", "EmailDomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            // Stored only: registration-time enforcement needs email
            // confirmation (Phase 2 gap).
            Json(email_domain_block_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn email_domain_blocks_destroy(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:email_domain_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_delete_email_domain_block(id).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "destroy", "EmailDomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// IP blocks (+ request-path enforcement).
// ---------------------------------------------------------------------------

/// The client IP for this request under the trusted-proxy config (Phase 1
/// rule): when the peer is a configured trusted proxy, the leftmost
/// `X-Forwarded-For` entry wins; otherwise the peer address is the client IP.
pub fn request_client_ip(headers: &HeaderMap, peer: Option<IpAddr>, trusted_proxies: &[IpAddr]) -> Option<IpAddr> {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .map(|item| item.to_string());
    use_cases::client_ip(forwarded.as_deref(), peer, trusted_proxies)
}

/// Axum middleware enforcing `no_access` IP blocks: a request whose client IP
/// (see [`request_client_ip`]) falls in an unexpired `no_access` block is
/// refused with `403 {"error":"Forbidden"}` before it reaches the handler.
///
/// Mount inside the rate limiter (like `json_body::guard`) so the cheapest
/// refusal runs first:
///
/// ```ignore
/// .layer(middleware::from_fn_with_state(state.clone(), admin_infra::ip_block_middleware))
/// ```
pub async fn ip_block_middleware(State(state): State<ApiState>, request: axum::extract::Request, next: Next) -> Response {
    // `main.rs` serves with `into_make_service_with_connect_info`, so the
    // peer arrives as `ConnectInfo<SocketAddr>`; accept a raw `SocketAddr`
    // too (tests and alternate harnesses).
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0)
        .or_else(|| request.extensions().get::<std::net::SocketAddr>().copied())
        .map(|addr| addr.ip());
    let client = request_client_ip(request.headers(), peer, state.trusted_proxies.as_slice());
    if let Some(ip) = client {
        let store: &dyn Store = &*state.context.store;
        match store.admin_ip_blocks_all(1_000) {
            Ok(blocks) => {
                if use_cases::is_no_access_blocked(&blocks, ip, state.context.now_ms()) {
                    return ApiError::role_forbidden().into_response();
                }
            }
            Err(err) => return ApiError::from(err).into_response(),
        }
    }
    next.run(request).await
}

/// True when registration from `client_ip` must be refused: the IP sits in an
/// unexpired `sign_up_block` block (upstream `RegistrationHelper#ip_blocked?`).
/// The accounts registration path calls this; email-confirmation-gated
/// email blocks stay store-only until Phase 2.
pub fn is_sign_up_blocked_for(state: &ApiState, client_ip: IpAddr) -> bool {
    let store: &dyn Store = &*state.context.store;
    store
        .admin_ip_blocks_all(1_000)
        .map(|blocks| use_cases::is_sign_up_blocked(&blocks, client_ip, state.context.now_ms()))
        .unwrap_or(false)
}

fn ip_block_patch(body: &[(String, String)], now_ms: u64) -> AdminIpBlockPatch {
    AdminIpBlockPatch {
        severity: forms::get(body, "severity").and_then(|raw| AdminIpSeverity::parse(raw.trim())),
        comment: forms::get(body, "comment").map(|s| {
            if s.is_empty() { None } else { Some(s.to_string()) }
        }),
        expires_at_ms: forms::get(body, "expires_in").map(|raw| {
            raw.trim()
                .parse::<i64>()
                .ok()
                .filter(|secs| *secs > 0)
                .map(|secs| now_ms.saturating_add(secs as u64 * 1_000))
        }),
    }
}

pub async fn ip_blocks_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:ip_blocks")) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(&uri, 100, 200, |max, since, min, limit| store.admin_ip_blocks_page(max, since, min, limit), ip_block_json, |row| row.id)
}

pub async fn ip_blocks_show(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:ip_blocks")) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_ip_block_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(ip_block_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn ip_blocks_create(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:ip_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let ip = forms::get(&body, "ip").unwrap_or("");
    let severity_raw = forms::get(&body, "severity").unwrap_or("");
    if severity_raw.trim().is_empty() {
        return ApiError::unprocessable("Validation failed: Severity can't be blank").into_response();
    }
    let severity = match use_cases::parse_ip_severity(severity_raw) {
        Ok(severity) => severity,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let comment = forms::get(&body, "comment").unwrap_or("");
    let now = state.context.now_ms();
    let expires_at_ms = forms::get(&body, "expires_in").and_then(|raw| {
        raw.trim()
            .parse::<i64>()
            .ok()
            .filter(|secs| *secs > 0)
            .map(|secs| now.saturating_add(secs as u64 * 1_000))
    });
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_ip_block(ip, severity, comment, expires_at_ms, now) {
        Ok(row) => {
            if let Err(err) = audit(&state, actor, "create", "IpBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(ip_block_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn ip_blocks_update(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:ip_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    if let Some(raw) = forms::get(&body, "severity") {
        if use_cases::parse_ip_severity(raw).is_err() {
            return ApiError::from(crate::domain::Error::InvalidInput(
                "Validation failed: Severity is not included in the list".into(),
            ))
            .into_response();
        }
    }
    let now = state.context.now_ms();
    let patch = ip_block_patch(&body, now);
    let store: &dyn Store = &*state.context.store;
    match store.admin_update_ip_block(id, patch, now).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "update", "IpBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(ip_block_json(&row)).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn ip_blocks_destroy(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:ip_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_delete_ip_block(id).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "destroy", "IpBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Canonical email blocks.
// ---------------------------------------------------------------------------

pub async fn canonical_email_blocks_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:canonical_email_blocks")) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(
        &uri,
        100,
        200,
        |max, since, min, limit| store.admin_canonical_email_blocks_page(max, since, min, limit),
        canonical_email_block_json,
        |row| row.id,
    )
}

/// `POST /api/v1/admin/canonical_email_blocks/test`: which blocks match an
/// email. Takes the **read** scope upstream (`test` is in the read guard) —
/// kept here.
pub async fn canonical_email_blocks_test(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:canonical_email_blocks")) {
        return err.into_response();
    }
    let Some(email) = forms::get(&body, "email") else {
        return ApiError::bad_request("param is missing or the value is empty: email").into_response();
    };
    if use_cases::validate_canonical_email(email).is_err() {
        return ApiError::bad_request("param is missing or the value is empty: email").into_response();
    }
    let hash = use_cases::canonical_email_hash(email);
    let store: &dyn Store = &*state.context.store;
    match store.admin_canonical_blocks_for_hash(&hash).map_err(ApiError::from) {
        Ok(rows) => {
            let body: Vec<Value> = rows.iter().map(canonical_email_block_json).collect();
            Json(body).into_response()
        }
        Err(err) => err.into_response(),
    }
}

pub async fn canonical_email_blocks_create(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:canonical_email_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let hash = if let Some(email) = forms::get(&body, "email") {
        if use_cases::validate_canonical_email(email).is_err() {
            return ApiError::unprocessable("Validation failed: Email is invalid").into_response();
        }
        use_cases::canonical_email_hash(email)
    } else if let Some(hash) = forms::get(&body, "canonical_email_hash") {
        let hash = hash.trim().to_string();
        let is_hex = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        if !is_hex {
            return ApiError::unprocessable(
                "Validation failed: Canonical email hash is invalid",
            )
            .into_response();
        }
        hash.to_ascii_lowercase()
    } else {
        return ApiError::unprocessable("Validation failed: Email can't be blank").into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_canonical_email_block(&hash, state.context.now_ms()) {
        Ok(row) => {
            if let Err(err) = audit(&state, actor, "create", "CanonicalEmailBlock", &row.id.to_string()) {
                return err.into_response();
            }
            // Stored only: registration-time enforcement needs email
            // confirmation (Phase 2 gap).
            Json(canonical_email_block_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn canonical_email_blocks_show(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, read_scopes!("admin:read:canonical_email_blocks")) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_canonical_email_block_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(canonical_email_block_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn canonical_email_blocks_destroy(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, write_scopes!("admin:write:canonical_email_blocks")) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_delete_canonical_email_block(id).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "destroy", "CanonicalEmailBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Tags.
// ---------------------------------------------------------------------------

/// `GET /api/v1/admin/tags`: every real hashtag gets a registry row
/// (materialized from the hashtag index on read — never fabricated), then the
/// registry pages newest first.
pub async fn tags_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let names = match store.admin_hashtag_names(1_000) {
        Ok(names) => names,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let now = state.context.now_ms();
    for name in &names {
        if let Err(err) = store.admin_ensure_tag(name, now) {
            return ApiError::from(err).into_response();
        }
    }
    let server = state.server.public_url.clone();
    paged(
        &uri,
        100,
        200,
        |max, since, min, limit| store.admin_tags_page(max, since, min, limit),
        |row| admin_tag_json(&server, row),
        |row| row.id,
    )
}

pub async fn tags_show(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_tag_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(admin_tag_json(&state.server.public_url, &row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

/// `PUT|PATCH /api/v1/admin/tags/:id`: `display_name`, `listable`,
/// `trendable`, `usable` (upstream's permitted params). Stamps `reviewed_at`.
pub async fn tags_update(State(state): State<ApiState>, auth: Auth, Path(id): Path<String>, LenientForm(body): LenientForm) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, &["admin:write"]) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let patch = AdminTagPatch {
        display_name: forms::get(&body, "display_name").map(|item| item.to_string()),
        trendable: forms::get(&body, "trendable").map(bool_cast),
        usable: forms::get(&body, "usable").map(bool_cast),
        listable: forms::get(&body, "listable").map(bool_cast),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_update_tag(id, patch, state.context.now_ms()).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "update", "Tag", &row.id.to_string()) {
                return err.into_response();
            }
            Json(admin_tag_json(&state.server.public_url, &row)).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Measures, dimensions, retention (all POST, all read-scoped upstream).
// ---------------------------------------------------------------------------

const METRIC_SCAN_CAP: usize = 200_000;
const DAY_MS: u64 = 86_400_000;

/// D39 refused keys (round 3, narrowed round 4): served-or-refused, never
/// zero-filled. Each known-but-uncomputable key stays absent from the response
/// array (upstream's `filter_map` drops unknown keys the same way); the reason
/// lives here and in the dashboard note, not in the wire shape.
///
/// `interactions` was refused until the unified event journal landed; it is
/// served since round 4 (writers: non-self replies, favourites, reblogs,
/// votes, new follows/requests — the five upstream tracker call sites), so
/// its reason constant is gone.
pub const REFUSED_SOFTWARE_VERSIONS: &str =
    "no component-version census is collected: upstream `software_versions` reports mastodon/ruby/postgresql/redis/[elasticsearch|opensearch]/libvips/ffmpeg per row (`app/lib/admin/metrics/dimension/software_versions_dimension.rb:13-19`), each probed live (SELECT VERSION(), redis INFO, Chewy client info, Vips.version_string, ffprobe output). This server has no postgresql/redis/search processes to probe and collects no ruby/libvips/ffmpeg version rows, so the key stays absent rather than fabricated";
/// `tag_*` measures and dimensions require `<key>[id]` (upstream
/// `params.require(key).permit(:id)` → 400 when missing). Without the id the
/// key is omitted (lenient where upstream is strict); the reason is recorded
/// here (D39).
pub const REFUSED_TAG_NO_ID: &str =
    "tag measures/dimensions need `<key>[id]` naming a trend-flag or admin-tag row id";
/// An unknown tag id names no trend-flag or admin-tag row: the key is
/// omitted, never zero-filled.
pub const REFUSED_TAG_UNKNOWN_ID: &str = "unknown tag id: no trend-flag or admin-tag row";

/// Bounded tag-status scan backing the `tag_*` measures and dimensions
/// (round 3, D39): the same hashtag-index source as D40 `tag_history`
/// (`trends::tag_history` renders the trailing 7 days for display; metrics
/// need the full request window, so they scan the index directly, newest
/// entries first up to the `redb` hashtag scan cap).
const TAG_METRIC_SCAN: usize = 2_000;

struct AllowAllStatuses;

impl TimelineFilter for AllowAllStatuses {
    fn include(&self, _status: &Status) -> bool {
        true
    }
}

/// Resolve a `tag_*` `<key>[id]>` to its normalized tag name: trend-flag rows
/// first (the admin trends queue mints the numeric `:id`), then admin tag
/// rows. `None` names nothing (the key is omitted).
fn tag_name_for_id(store: &dyn Store, id: Id) -> Option<String> {
    if let Ok(flags) = store.all_trend_tag_flags()
        && let Some(flag) = flags.into_iter().find(|flag| flag.id == id)
    {
        return Some(flag.name);
    }
    if let Ok(Some(row)) = store.admin_tag_by_id(id) {
        return Some(row.name);
    }
    None
}

/// `(created_at_ms, account_id, language)` per live indexed status carrying
/// `tag`, newest first, bounded by [`TAG_METRIC_SCAN`].
fn tag_metric_rows(store: &dyn Store, tag: &str) -> Vec<(u64, Id, Option<String>)> {
    let page = store.hashtag_timeline(
        tag,
        TimelineQuery {
            limit: TAG_METRIC_SCAN,
            ..Default::default()
        },
        &AllowAllStatuses,
    );
    match page {
        Ok(page) => page
            .statuses
            .into_iter()
            .filter(|status| !status.is_deleted())
            .map(|status| (status.created_at_ms, status.account_id, status.language.clone()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Count distinct accounts per day bucket.
fn daily_distinct(pairs: &[(Id, u64)], buckets: &[u64]) -> Vec<u64> {
    use std::collections::HashSet;
    let mut sets: Vec<HashSet<Id>> = buckets.iter().map(|_| HashSet::new()).collect();
    for (who, time) in pairs {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            sets[idx].insert(*who);
        }
    }
    sets.iter().map(|set| set.len() as u64).collect()
}

/// Count distinct domains per day bucket.
fn daily_distinct_domains(pairs: &[(String, u64)], buckets: &[u64]) -> Vec<u64> {
    use std::collections::HashSet;
    let mut sets: Vec<HashSet<&str>> = buckets.iter().map(|_| HashSet::new()).collect();
    for (domain, time) in pairs {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            sets[idx].insert(domain.as_str());
        }
    }
    sets.iter().map(|set| set.len() as u64).collect()
}

/// Parse an optional `start_at`/`end_at` form value (RFC 3339 or date).
/// Absent values default to the trailing 30 days ending now.
fn metric_window(body: &[(String, String)], now_ms: u64) -> (u64, u64) {
    let parse = |key: &str| -> Option<u64> {
        let raw = forms::get(body, key)?.trim().to_string();
        if raw.is_empty() {
            return None;
        }
        // Epoch seconds or millis first (cheap), then RFC 3339, then date.
        if let Ok(num) = raw.parse::<i64>() {
            let ms = if num.abs() < 10_000_000_000 { num * 1_000 } else { num };
            return u64::try_from(ms).ok();
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&raw) {
            return u64::try_from(dt.timestamp_millis()).ok();
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(&raw, "%Y-%m-%d") {
            return u64::try_from(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis()).ok();
        }
        None
    };
    let end = parse("end_at").unwrap_or(now_ms);
    let start = parse("start_at").unwrap_or_else(|| end.saturating_sub(30 * DAY_MS));
    // Upstream clamps the window to two years back.
    let start = start.max(end.saturating_sub(2 * 365 * DAY_MS));
    (start.min(end), end)
}

/// Day-bucket boundaries covering `[start, end]` (each a midnight UTC ms).
fn day_buckets(start_ms: u64, end_ms: u64) -> Vec<u64> {
    let day = |ms: u64| ms - ms % DAY_MS;
    let mut out = Vec::new();
    let mut cursor = day(start_ms);
    let last = day(end_ms);
    while cursor <= last {
        out.push(cursor);
        cursor = cursor.saturating_add(DAY_MS);
        if out.len() > 732 {
            break;
        }
    }
    out
}

/// Count `times` per day bucket.
fn daily_counts(times: &[u64], buckets: &[u64]) -> Vec<u64> {
    let mut counts = vec![0u64; buckets.len()];
    for &time in times {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            counts[idx] += 1;
        }
    }
    counts
}

/// Count distinct `who` per day bucket (D39 `active_users`: unique logins
/// per day, like upstream's `:unique` activity tracker).
fn daily_unique(pairs: &[(Id, u64)], buckets: &[u64]) -> Vec<u64> {
    use std::collections::HashSet;
    let mut sets: Vec<HashSet<Id>> = buckets.iter().map(|_| HashSet::new()).collect();
    for (who, time) in pairs {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            sets[idx].insert(*who);
        }
    }
    sets.iter().map(|set| set.len() as u64).collect()
}

fn daily_data(buckets: &[u64], counts: &[u64]) -> Vec<Value> {
    buckets
        .iter()
        .zip(counts.iter())
        .map(|(day, count)| json!({ "date": date_only(*day), "value": count.to_string() }))
        .collect()
}

fn sum_in_window(times: &[u64], start: u64, end: u64) -> u64 {
    times.iter().filter(|&&t| t >= start && t <= end).count() as u64
}
/// `POST /api/v1/admin/measures` with read scope upstream (kept). Computes
/// only keys backed by stored data; unknown or uncomputable keys are omitted
/// (upstream's `filter_map` also drops unknown keys — never fabricated).
/// Uncomputable known keys are recorded as planned gaps in the manifest.
/// Moderators pass ([`require_metrics`]); every other admin-infra route
/// stays admin-only.
pub async fn measures(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    if let Err(err) = require_metrics(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let (start, end) = metric_window(&body, now);
    let buckets = day_buckets(start, end);
    let window_len = end.saturating_sub(start);
    let prev_start = start.saturating_sub(window_len);
    let prev_end = start.saturating_sub(1);

    let keys: Vec<String> = forms::all(&body, "keys[]")
        .into_iter()
        .map(|item| item.to_string())
        .chain(forms::get(&body, "keys").map(|item| item.to_string()))
        .collect();
    let local_domain = state.server.domain.clone();
    let domain_param = forms::get(&body, "domain").map(|item| item.to_string());
    let include_sub = forms::get(&body, "include_subdomains").is_some_and(bool_cast);

    // One bounded scan per source table, shared by every requested key.
    let accounts = match store.admin_accounts_overview(METRIC_SCAN_CAP) {
        Ok(accounts) => accounts,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let status_rows = match store.admin_status_metrics(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let report_targets = match store.admin_report_targets(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let media_owners = match store.admin_media_owners(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let follows: Vec<FollowMetricRow> = match store.admin_follow_metrics(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let sign_ins = match store.admin_sign_ins(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let resolved_times = match store.admin_report_resolved_times(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };

    let domains: HashMap<Id, (String, bool)> = accounts
        .iter()
        .map(|a| (a.id, (a.domain.clone(), a.local)))
        .collect();
    let wanted = domain_param.as_deref().unwrap_or(&local_domain);
    let in_scope = |domain: &str, local: bool| -> bool {
        if domain == wanted {
            return true;
        }
        if include_sub {
            if domain.ends_with(&format!(".{wanted}")) {
                return true;
            }
            // A bare local-domain request with subdomains also counts local
            // accounts (upstream's `by_domain_and_subdomains`).
            if local && wanted == local_domain {
                return true;
            }
        }
        false
    };
    let account_in_scope = |id: &Id| -> bool {
        domains.get(id).is_some_and(|(domain, local)| in_scope(domain, *local))
    };

    let mut out = Vec::new();
    for key in &keys {
        match key.as_str() {
            "new_users" => {
                let times: Vec<u64> = accounts.iter().map(|a| a.created_at_ms).collect();
                let total = sum_in_window(&times, start, end);
                let previous = sum_in_window(&times, prev_start, prev_end);
                let counts = daily_counts(&times, &buckets);
                out.push(json!({
                    "key": "new_users",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_accounts" => {
                let scoped: Vec<&AdminAccountOverview> = accounts
                    .iter()
                    .filter(|a| in_scope(&a.domain, a.local))
                    .collect();
                let total = scoped.len() as u64;
                let times: Vec<u64> = scoped.iter().map(|a| a.created_at_ms).collect();
                let counts = daily_counts(&times, &buckets);
                out.push(json!({
                    "key": "instance_accounts",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_statuses" => {
                let scoped: Vec<&StatusMetricRow> = status_rows
                    .iter()
                    .filter(|row| account_in_scope(&row.account_id))
                    .collect();
                let total = scoped.len() as u64;
                let times: Vec<u64> = scoped.iter().map(|row| row.created_at_ms).collect();
                let windowed: Vec<u64> = times.into_iter().filter(|&t| t >= start && t <= end).collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_statuses",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_reports" => {
                let scoped: Vec<&AdminReportTarget> = report_targets
                    .iter()
                    .filter(|report| account_in_scope(&report.target_account_id))
                    .collect();
                let total = scoped.len() as u64;
                let times: Vec<u64> = scoped.iter().map(|report| report.created_at_ms).collect();
                let windowed: Vec<u64> = times.into_iter().filter(|&t| t >= start && t <= end).collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_reports",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "opened_reports" => {
                let times: Vec<u64> = report_targets.iter().map(|r| r.created_at_ms).collect();
                let total = sum_in_window(&times, start, end);
                let previous = sum_in_window(&times, prev_start, prev_end);
                let counts = daily_counts(&times, &buckets);
                out.push(json!({
                    "key": "opened_reports",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_media_attachments" => {
                let scoped: Vec<&AdminMediaOwner> = media_owners
                    .iter()
                    .filter(|media| account_in_scope(&media.owner_id))
                    .collect();
                let total_bytes: u64 = scoped.iter().map(|media| media.file_size).sum();
                let windowed: Vec<u64> = scoped
                    .iter()
                    .filter(|media| media.created_at_ms >= start && media.created_at_ms <= end)
                    .map(|media| media.created_at_ms)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_media_attachments",
                    "unit": "bytes",
                    "total": total_bytes.to_string(),
                    "human_value": human_size(total_bytes),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "resolved_reports" => {
                // Upstream `ResolvedReportsMeasure`: resolutions by
                // `action_taken_at`, with a previous-window total.
                let total = sum_in_window(&resolved_times, start, end);
                let previous = sum_in_window(&resolved_times, prev_start, prev_end);
                let counts = daily_counts(&resolved_times, &buckets);
                out.push(json!({
                    "key": "resolved_reports",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "active_users" => {
                // D39 sign-in telemetry: daily *unique* password logins
                // (upstream's `:unique` activity tracker). Totals are sums
                // of the daily uniques, like upstream.
                let in_window: Vec<(Id, u64)> = sign_ins
                    .iter()
                    .copied()
                    .filter(|(_, t)| *t >= start && *t <= end)
                    .collect();
                let in_prev: Vec<(Id, u64)> = sign_ins
                    .iter()
                    .copied()
                    .filter(|(_, t)| *t >= prev_start && *t <= prev_end)
                    .collect();
                let counts = daily_unique(&in_window, &buckets);
                let total: u64 = counts.iter().sum();
                let previous: u64 = daily_unique(&in_prev, &buckets).iter().sum();
                out.push(json!({
                    "key": "active_users",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_follows" => {
                // Upstream `InstanceFollowsMeasure`: follows by *target*
                // domain. `total_in_time_range?` is false, so the total is
                // the all-time count and there is no `previous_total`.
                let scoped: Vec<&FollowMetricRow> = follows
                    .iter()
                    .filter(|row| {
                        domains.get(&row.target_id).is_some_and(|(domain, local)| {
                            in_scope(domain, *local)
                        })
                    })
                    .collect();
                let total = scoped.len() as u64;
                let windowed: Vec<u64> = scoped
                    .iter()
                    .map(|row| row.created_at_ms)
                    .filter(|&t| t >= start && t <= end)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_follows",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_followers" => {
                // Upstream `InstanceFollowersMeasure`: follows by
                // *follower* domain; otherwise shaped like
                // `instance_follows`.
                let scoped: Vec<&FollowMetricRow> = follows
                    .iter()
                    .filter(|row| {
                        domains.get(&row.follower_id).is_some_and(|(domain, local)| {
                            in_scope(domain, *local)
                        })
                    })
                    .collect();
                let total = scoped.len() as u64;
                let windowed: Vec<u64> = scoped
                    .iter()
                    .map(|row| row.created_at_ms)
                    .filter(|&t| t >= start && t <= end)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_followers",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "interactions" => {
                // D39 served (round 4): the unified interaction journal —
                // non-self replies, favourites, reblogs, votes and new
                // follows/requests (upstream's five `activity:interactions`
                // tracker call sites). Shape matches upstream's
                // `MeasureSerializer` (`key/unit/total/previous_total/data`,
                // daily values as strings): totals are sums of the daily
                // buckets, like `active_users` above. One bounded range scan
                // per bucket plus one for the previous window; the end bound
                // is inclusive on the wire, exclusive in the store call.
                let end_excl = end.saturating_add(1);
                let mut counts = Vec::with_capacity(buckets.len());
                for &day in &buckets {
                    let low = day.max(start);
                    let high = day.saturating_add(DAY_MS).min(end_excl);
                    let bucket_count = if low < high {
                        match store.count_interactions(None, low, high) {
                            Ok(bucket_count) => bucket_count,
                            Err(err) => return ApiError::from(err).into_response(),
                        }
                    } else {
                        0
                    };
                    counts.push(bucket_count);
                }
                let total: u64 = counts.iter().sum();
                let previous = match store.count_interactions(
                    None,
                    prev_start,
                    prev_end.saturating_add(1),
                ) {
                    Ok(previous) => previous,
                    Err(err) => return ApiError::from(err).into_response(),
                };
                out.push(json!({
                    "key": "interactions",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            _ => {
                // `tag_accounts`, `tag_uses` and `tag_servers` are served
                // above when `<key>[id]>` names a tag; without a resolvable id
                // they stay omitted ([`REFUSED_TAG_NO_ID`] /
                // [`REFUSED_TAG_UNKNOWN_ID`]). Unknown keys drop like
                // upstream's `filter_map`.
                if let Some(tag_key) = ["tag_accounts", "tag_uses", "tag_servers"]
                    .into_iter()
                    .find(|known| *known == key.as_str())
                {
                    let param_key = format!("{tag_key}[id]");
                    let tag = forms::get(&body, param_key.as_str())
                        .and_then(|raw| raw.trim().parse::<Id>().ok())
                        .and_then(|id| tag_name_for_id(store, id));
                    let Some(tag) = tag else {
                        continue;
                    };
                    // Real local counts from the hashtag index (never
                    // fabricated, never zero-filled): uses, distinct
                    // accounts and distinct author domains per day.
                    let rows = tag_metric_rows(store, &tag);
                    let in_win: Vec<(u64, Id)> = rows
                        .iter()
                        .filter(|(t, _, _)| *t >= start && *t <= end)
                        .map(|(t, who, _)| (*t, *who))
                        .collect();
                    let in_prev: Vec<(u64, Id)> = rows
                        .iter()
                        .filter(|(t, _, _)| *t >= prev_start && *t <= prev_end)
                        .map(|(t, who, _)| (*t, *who))
                        .collect();
                    match tag_key {
                        "tag_accounts" => {
                            let pairs: Vec<(Id, u64)> =
                                in_win.iter().map(|(t, who)| (*who, *t)).collect();
                            let prev_pairs: Vec<(Id, u64)> =
                                in_prev.iter().map(|(t, who)| (*who, *t)).collect();
                            let counts = daily_distinct(&pairs, &buckets);
                            let total: u64 = counts.iter().sum();
                            let previous: u64 =
                                daily_distinct(&prev_pairs, &buckets).iter().sum();
                            out.push(json!({
                                "key": "tag_accounts",
                                "unit": Value::Null,
                                "total": total.to_string(),
                                "previous_total": previous.to_string(),
                                "data": daily_data(&buckets, &counts),
                            }));
                        }
                        "tag_uses" => {
                            let times: Vec<u64> = in_win.iter().map(|(t, _)| *t).collect();
                            let prev_times: Vec<u64> =
                                in_prev.iter().map(|(t, _)| *t).collect();
                            let total = times.len() as u64;
                            let previous = prev_times.len() as u64;
                            let counts = daily_counts(&times, &buckets);
                            out.push(json!({
                                "key": "tag_uses",
                                "unit": Value::Null,
                                "total": total.to_string(),
                                "previous_total": previous.to_string(),
                                "data": daily_data(&buckets, &counts),
                            }));
                        }
                        _ => {
                            // `tag_servers`: distinct author domains.
                            let mut ids: Vec<Id> = rows.iter().map(|(_, who, _)| *who).collect();
                            ids.sort_unstable();
                            ids.dedup();
                            let authors = store.accounts_by_ids(&ids).unwrap_or_default();
                            let domain_of = |who: &Id| {
                                authors
                                    .get(who)
                                    .map(|account| account.domain.clone())
                                    .unwrap_or_else(|| local_domain.clone())
                            };
                            let dom_win: Vec<(String, u64)> = in_win
                                .iter()
                                .map(|(t, who)| (domain_of(who), *t))
                                .collect();
                            let dom_prev: Vec<(String, u64)> = in_prev
                                .iter()
                                .map(|(t, who)| (domain_of(who), *t))
                                .collect();
                            let counts = daily_distinct_domains(&dom_win, &buckets);
                            let total = {
                                use std::collections::HashSet;
                                dom_win.iter().map(|(d, _)| d.as_str()).collect::<HashSet<_>>().len() as u64
                            };
                            let previous = {
                                use std::collections::HashSet;
                                dom_prev.iter().map(|(d, _)| d.as_str()).collect::<HashSet<_>>().len() as u64
                            };
                            out.push(json!({
                                "key": "tag_servers",
                                "unit": Value::Null,
                                "total": total.to_string(),
                                "previous_total": previous.to_string(),
                                "data": daily_data(&buckets, &counts),
                            }));
                        }
                    }
                }
                // Omitted like unknown keys upstream (`filter_map` drops
                // them).
            }
        }
    }
    Json(out).into_response()
}

/// `POST /api/v1/admin/dimensions` with read scope upstream (kept). Same
/// omit-unknown-keys rule as measures. Moderators pass ([`require_metrics`]).
pub async fn dimensions(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    if let Err(err) = require_metrics(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let (start, end) = metric_window(&body, now);
    let limit = forms::get(&body, "limit")
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(10)
        .min(500)
        .max(1);
    let keys: Vec<String> = forms::all(&body, "keys[]")
        .into_iter()
        .map(|item| item.to_string())
        .chain(forms::get(&body, "keys").map(|item| item.to_string()))
        .collect();

    let status_rows = match store.admin_status_metrics(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let media_owners = match store.admin_media_owners(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let accounts = match store.admin_accounts_overview(METRIC_SCAN_CAP) {
        Ok(accounts) => accounts,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let follows: Vec<FollowMetricRow> = match store.admin_follow_metrics(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let domains: HashMap<Id, String> = accounts.iter().map(|a| (a.id, a.domain.clone())).collect();
    let in_window = |t: u64| t >= start && t <= end;

    let mut out = Vec::new();
    for key in &keys {
        match key.as_str() {
            "languages" => {
                let mut counts: HashMap<String, u64> = HashMap::new();
                for row in &status_rows {
                    if !in_window(row.created_at_ms) {
                        continue;
                    }
                    let locale = row.language.clone().unwrap_or_else(|| "und".to_string());
                    *counts.entry(locale).or_insert(0) += 1;
                }
                out.push(json!({
                    "key": "languages",
                    "data": top_counts(counts, limit),
                }));
            }
            "sources" => {
                let mut counts: HashMap<String, u64> = HashMap::new();
                for row in &status_rows {
                    if !in_window(row.created_at_ms) {
                        continue;
                    }
                    let name = row.application_name.clone().unwrap_or_else(|| "web".to_string());
                    *counts.entry(name).or_insert(0) += 1;
                }
                out.push(json!({
                    "key": "sources",
                    "data": top_counts(counts, limit),
                }));
            }
            "servers" => {
                // Status counts by author domain in the window (upstream's
                // `ServersDimension` shape: domain → status count).
                let mut counts: HashMap<String, u64> = HashMap::new();
                for row in &status_rows {
                    if !in_window(row.created_at_ms) {
                        continue;
                    }
                    let domain = domains
                        .get(&row.account_id)
                        .cloned()
                        .unwrap_or_else(|| state.server.domain.clone());
                    *counts.entry(domain).or_insert(0) += 1;
                }
                out.push(json!({
                    "key": "servers",
                    "data": top_counts(counts, limit),
                }));
            }
            "space_usage" => {
                let media_bytes: u64 = media_owners.iter().map(|media| media.file_size).sum();
                let mut data = vec![
                    json!({
                        "key": "media",
                        "human_key": "Media",
                        "value": media_bytes.to_string(),
                        "unit": "bytes",
                        "human_value": human_size(media_bytes),
                    }),
                ];
                // Round 3 (D39): the live database size, measured from the
                // filesystem — never estimated. postgres/redis/search entries
                // are never reported: this server has neither (no search
                // index on disk — the embedded index lives in the same
                // database file — and no postgres/redis to measure).
                if let Ok(Some(db_bytes)) = store.admin_db_size_bytes() {
                    data.push(json!({
                        "key": "database",
                        "human_key": "Database",
                        "value": db_bytes.to_string(),
                        "unit": "bytes",
                        "human_value": human_size(db_bytes),
                    }));
                }
                data.truncate(limit);
                out.push(json!({ "key": "space_usage", "data": data }));
            }
            "instance_accounts" => {
                // Upstream `InstanceAccountsDimension`: accounts of one
                // domain (exact match) by follower count, most-followed
                // first. The domain defaults to the local one (upstream
                // `params.require`s it — lenient here, documented).
                let wanted = forms::get(&body, "domain").unwrap_or(&state.server.domain);
                let mut counts: HashMap<String, u64> = HashMap::new();
                let mut names: HashMap<Id, String> = HashMap::new();
                for account in &accounts {
                    if account.domain == wanted {
                        counts.entry(account.username.clone()).or_insert(0);
                        names.insert(account.id, account.username.clone());
                    }
                }
                for row in &follows {
                    if let Some(name) = names.get(&row.target_id) {
                        *counts.entry(name.clone()).or_insert(0) += 1;
                    }
                }
                out.push(json!({
                    "key": "instance_accounts",
                    "data": top_counts(counts, limit),
                }));
            }
            "instance_languages" => {
                // Upstream `InstanceLanguagesDimension`: one domain's
                // non-reblog statuses by language. `human_key` carries the
                // bare language code (upstream localizes it; no locale
                // catalog here — documented).
                let wanted = forms::get(&body, "domain").unwrap_or(&state.server.domain);
                let mut counts: HashMap<String, u64> = HashMap::new();
                for row in &status_rows {
                    if row.is_reblog {
                        continue;
                    }
                    if domains.get(&row.account_id).is_some_and(|d| d == wanted) {
                        let locale = row.language.clone().unwrap_or_else(|| "und".to_string());
                        *counts.entry(locale).or_insert(0) += 1;
                    }
                }
                out.push(json!({
                    "key": "instance_languages",
                    "data": top_counts(counts, limit),
                }));
            }
            _ => {
                // `tag_servers` / `tag_languages` are served below when
                // `<key>[id]>` names a tag; without a resolvable id they stay
                // omitted ([`REFUSED_TAG_NO_ID`] / [`REFUSED_TAG_UNKNOWN_ID`]).
                // `software_versions` stays refused (D39,
                // [`REFUSED_SOFTWARE_VERSIONS`]). Unknown keys drop like
                // upstream's `filter_map`.
                if let Some(tag_key) = ["tag_servers", "tag_languages"]
                    .into_iter()
                    .find(|known| *known == key.as_str())
                {
                    let param_key = format!("{tag_key}[id]");
                    let tag = forms::get(&body, param_key.as_str())
                        .and_then(|raw| raw.trim().parse::<Id>().ok())
                        .and_then(|id| tag_name_for_id(store, id));
                    let Some(tag) = tag else {
                        continue;
                    };
                    let rows = tag_metric_rows(store, &tag);
                    match tag_key {
                        "tag_servers" => {
                            let mut ids: Vec<Id> =
                                rows.iter().map(|(_, who, _)| *who).collect();
                            ids.sort_unstable();
                            ids.dedup();
                            let authors = store.accounts_by_ids(&ids).unwrap_or_default();
                            let mut counts: HashMap<String, u64> = HashMap::new();
                            for (at, who, _) in &rows {
                                if !in_window(*at) {
                                    continue;
                                }
                                let domain = authors
                                    .get(who)
                                    .map(|account| account.domain.clone())
                                    .unwrap_or_else(|| state.server.domain.clone());
                                *counts.entry(domain).or_insert(0) += 1;
                            }
                            out.push(json!({
                                "key": "tag_servers",
                                "data": top_counts(counts, limit),
                            }));
                        }
                        _ => {
                            let mut counts: HashMap<String, u64> = HashMap::new();
                            for (at, _, language) in &rows {
                                if !in_window(*at) {
                                    continue;
                                }
                                let locale =
                                    language.clone().unwrap_or_else(|| "und".to_string());
                                *counts.entry(locale).or_insert(0) += 1;
                            }
                            out.push(json!({
                                "key": "tag_languages",
                                "data": top_counts(counts, limit),
                            }));
                        }
                    }
                }
            }
        }
    }
    Json(out).into_response()
}

fn top_counts(counts: HashMap<String, u64>, limit: usize) -> Vec<Value> {
    let mut entries: Vec<(String, u64)> = counts.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries
        .into_iter()
        .take(limit)
        .map(|(key, value)| {
            json!({ "key": key.clone(), "human_key": key, "value": value.to_string() })
        })
        .collect()
}

/// `POST /api/v1/admin/retention` with read scope upstream (kept).
///
/// Cohorts of accounts by creation day (or month), with the share still
/// active in each later period. Upstream measures sign-ins
/// (`users.current_sign_in_at`); this server unions two real activity
/// signals: the password sign-in log (round 2 telemetry — API calls on an
/// existing token are not sign-ins) and authored statuses (posting implies an
/// active session; bearer tokens here never expire, so sign-ins alone would
/// miss API-only users). Members with neither signal are never active —
///
/// `start_at`/`end_at` are required (upstream
/// `params.require` → 400 when missing); `frequency` is `day` (default) or
/// `month`. The window is clamped like upstream (31 days / 12 months back).
/// Moderators pass ([`require_metrics`]).
pub async fn retention(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    if let Err(err) = require_metrics(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let start_raw = forms::get(&body, "start_at").unwrap_or("").trim().to_string();
    let end_raw = forms::get(&body, "end_at").unwrap_or("").trim().to_string();
    if start_raw.is_empty() || end_raw.is_empty() {
        let missing = if start_raw.is_empty() { "start_at" } else { "end_at" };
        return ApiError::bad_request(format!("param is missing or the value is empty: {missing}"))
            .into_response();
    }
    let parse_dt = |raw: &str| -> Option<u64> {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
            return u64::try_from(dt.timestamp_millis()).ok();
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            return u64::try_from(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis()).ok();
        }
        raw.parse::<i64>().ok().and_then(|num| {
            let ms = if num.abs() < 10_000_000_000 { num * 1_000 } else { num };
            u64::try_from(ms).ok()
        })
    };
    let (Some(mut start_ms), Some(end_ms)) = (parse_dt(&start_raw), parse_dt(&end_raw)) else {
        return ApiError::bad_request("Invalid start_at or end_at").into_response();
    };
    if start_ms > end_ms {
        return ApiError::bad_request("Invalid start_at or end_at").into_response();
    }
    let frequency = if forms::get(&body, "frequency").unwrap_or("day") == "month" {
        "month"
    } else {
        "day"
    };
    // Upstream clamp: day → trailing 31 days, month → trailing 12 months.
    start_ms = start_ms.max(end_ms.saturating_sub(if frequency == "month" {
        366 * DAY_MS
    } else {
        31 * DAY_MS
    }));

    let store: &dyn Store = &*state.context.store;
    let accounts = match store.admin_accounts_overview(METRIC_SCAN_CAP) {
        Ok(accounts) => accounts,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let status_rows = match store.admin_status_metrics(METRIC_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };

    // Cohort key: creation day-start, or creation month-start.
    let cohort_of = |ms: u64| -> u64 {
        if frequency == "month" {
            month_start_ms(ms)
        } else {
            ms - ms % DAY_MS
        }
    };
    // Retention periods from each cohort start to `end_ms`, one step each.
    let step_ms = |from_ms: u64| -> u64 {
        if frequency == "month" {
            month_start_ms(from_ms.saturating_add(32 * DAY_MS)).saturating_sub(from_ms)
        } else {
            DAY_MS
        }
    };

    let mut cohorts: HashMap<u64, Vec<Id>> = HashMap::new();
    for account in &accounts {
        if account.created_at_ms < start_ms || account.created_at_ms > end_ms {
            continue;
        }
        cohorts.entry(cohort_of(account.created_at_ms)).or_default().push(account.id);
    }
    let mut cohort_keys: Vec<u64> = cohorts.keys().copied().collect();
    cohort_keys.sort_unstable();

    // Active periods per account: authored statuses plus password
    // sign-ins (round 3 sign-in cohorts; either signal counts).
    let mut active_periods: HashMap<Id, Vec<u64>> = HashMap::new();
    for row in &status_rows {
        active_periods.entry(row.account_id).or_default().push(row.created_at_ms);
    }
    if let Ok(sign_ins) = store.admin_sign_ins(METRIC_SCAN_CAP) {
        for (who, at) in sign_ins {
            active_periods.entry(who).or_default().push(at);
        }
    }
    for periods in active_periods.values_mut() {
        periods.sort_unstable();
    }

    let mut out = Vec::new();
    for cohort_start in cohort_keys {
        let members = &cohorts[&cohort_start];
        let created_count = members.len() as u64;
        let mut periods = Vec::new();
        let mut cursor = cohort_start;
        while cursor <= end_ms {
            periods.push(cursor);
            cursor = cursor.saturating_add(step_ms(cursor).max(1));
            if periods.len() > 366 {
                break;
            }
        }
        let mut data = Vec::new();
        for period in periods {
            let period_end = period.saturating_add(step_ms(period)).saturating_sub(1);
            let active_count = members
                .iter()
                .filter(|id| {
                    active_periods.get(*id).is_some_and(|times| {
                        times.iter().any(|&t| t >= period && t <= period_end)
                    })
                })
                .count() as u64;
            let rate = if created_count > 0 {
                active_count as f64 / created_count as f64
            } else {
                0.0
            };
            data.push(json!({
                "date": date_only(period),
                "rate": rate,
                "value": active_count.to_string(),
            }));
        }
        out.push(json!({
            "period": date_only(cohort_start),
            "frequency": frequency,
            "data": data,
        }));
    }
    Json(out).into_response()
}

/// Midnight UTC of the month containing `ms`.
fn month_start_ms(ms: u64) -> u64 {
    let days = (ms / DAY_MS) as i32 + 719_163;
    let date = chrono::NaiveDate::from_num_days_from_ce_opt(days).unwrap();
    let first = chrono::NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap();
    (first.num_days_from_ce() as u64).saturating_sub(719_163) * DAY_MS
}

// ---------------------------------------------------------------------------
// IP blocks (+ request-path enforcement).
// ---------------------------------------------------------------------------

use std::net::IpAddr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::ApiError;
use crate::application::admin_infra as use_cases;
use crate::application::ports::{AdminIpBlockPatch, Store};
use crate::domain::{AdminIpBlock, AdminIpSeverity};

use super::{audit, paged, require_admin, require_id, rfc3339_ms};

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

/// The client IP for this request under the trusted-proxy config: the single
/// chain rule in [`crate::api::mastodon::rate_limits::resolve_client_ip`]
/// (all `X-Forwarded-For` lines in order, then the peer; right-to-left walk
/// skipping trusted proxies, headers ignored for untrusted peers).
pub fn request_client_ip(
    headers: &HeaderMap,
    peer: Option<IpAddr>,
    trusted_proxies: &[IpAddr],
) -> Option<IpAddr> {
    let mut forwarded: Vec<&str> = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        if let Ok(text) = value.to_str() {
            forwarded.push(text);
        }
    }
    crate::api::mastodon::rate_limits::resolve_client_ip(peer, &forwarded, trusted_proxies)
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
pub async fn ip_block_middleware(
    State(state): State<ApiState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
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
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
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
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:ip_blocks"),
    ) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(
        &uri,
        100,
        200,
        |max, since, min, limit| store.admin_ip_blocks_page(max, since, min, limit),
        ip_block_json,
        |row| row.id,
    )
}

pub async fn ip_blocks_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:ip_blocks"),
    ) {
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

pub async fn ip_blocks_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:ip_blocks"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let ip = forms::get(&body, "ip").unwrap_or("");
    let severity_raw = forms::get(&body, "severity").unwrap_or("");
    if severity_raw.trim().is_empty() {
        return ApiError::unprocessable("Validation failed: Severity can't be blank")
            .into_response();
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

pub async fn ip_blocks_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:ip_blocks"),
    ) {
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
    match store
        .admin_update_ip_block(id, patch, now)
        .map_err(ApiError::from)
    {
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

pub async fn ip_blocks_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:ip_blocks"),
    ) {
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

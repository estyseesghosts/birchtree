//! `/api/web/*` — keystodon's first-party session+CSRF surface (Phase 7).
//!
//! Upstream guards these routes with the Devise session cookie and Rails'
//! CSRF protection (`Api::Web::BaseController`), not OAuth bearer tokens:
//!
//! - `PUT/PATCH /api/web/settings` — web UI settings sync (`require_user!`).
//! - `GET /api/web/embeds/:id` — OEmbed for a status: local statuses render
//!   the pinned `OEmbedSerializer` JSON under the status `show?` policy;
//!   remote statuses require a signed-in session and an outbound oEmbed
//!   fetch (remote fetch needs an HTML discovery + sanitizer pass; until
//!   then a signed-in remote read answers 404 like a failed fetch —
//!   never fabricated).
//! - `POST /api/web/push_subscriptions`, `PUT/PATCH /:id`, `DELETE /:id` —
//!   session-bound web push subscriptions (`require_user!` except destroy,
//!   which finds the row by its unsubscribe token and always answers 200).
//!   Destroy by token is CSRF-exempt like upstream (`skip_forgery_protection
//!   only: :destroy`); the numeric-id fallback underneath is a birchtree
//!   extension and requires CSRF when a session user acts (D44).
//!
//! Session authentication reuses the Phase 2 login session
//! (`birchtree_session` cookie). CSRF uses the double-submit cookie set at
//! login (`birchtree_csrf`, see the PATCH to `api::oauth::authorization`):
//! state-changing requests must carry its value in the `X-CSRF-Token`
//! header, else `422 Can't verify CSRF token authenticity.` (upstream's
//! exact message). This file also serves keystodon's built bundle from the
//! operator-configured directory (missing bundle → 404, never fabricated).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::status_view::load_status_views;
use crate::api::mastodon::ApiState;
use crate::api::oauth::authorization::SESSION_COOKIE;
use crate::application::policy;
use crate::application::ports::{Store, WebPushSubscription};
use crate::application::web as web_cases;
use crate::domain::Id;

/// CSRF cookie + header names (double-submit pattern).
pub const CSRF_COOKIE: &str = "birchtree_csrf";
const CSRF_HEADER: &str = "x-csrf-token";

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            if k == name {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// A validated browser session: the account id behind `birchtree_session`.
///
/// Missing, unknown or expired sessions answer `401 Authentication required`
/// (the JSON equivalent of Devise's sign-in redirect).
pub fn session_user(state: &ApiState, headers: &HeaderMap) -> ApiResult<Id> {
    let plaintext = cookie_value(headers, SESSION_COOKIE).ok_or_else(|| {
        ApiError::new(StatusCode::UNAUTHORIZED, "Authentication required")
    })?;
    let store: &dyn Store = &*state.context.store;
    let session = store
        .session_by_plaintext(&plaintext)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Authentication required"))?;
    if !session.is_valid_at(state.context.now_ms()) {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "Authentication required"));
    }
    store
        .account_by_id(session.user_id)
        .map_err(ApiError::from)?
        .map(|account| account.id)
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Authentication required"))
}

/// Enforce the CSRF double-submit check for state-changing web requests.
///
/// The `X-CSRF-Token` header must equal the `birchtree_csrf` cookie set at
/// login. Mismatches answer `422 Can't verify CSRF token authenticity.`
/// (upstream `Api::Web::BaseController`'s exact message).
pub fn check_csrf(headers: &HeaderMap) -> ApiResult<()> {
    let cookie = cookie_value(headers, CSRF_COOKIE);
    let header = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    match (cookie, header) {
        (Some(cookie), Some(header))
            if !cookie.is_empty() && constant_time_eq(&cookie, &header) =>
        {
            Ok(())
        }
        _ => Err(ApiError::unprocessable(
            "Can't verify CSRF token authenticity.",
        )),
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Rebuild the `data` JSON object from `data` / `data[...]` body pairs.
///
/// A form client sends `data` pre-encoded as JSON (or Rails-style
/// `data[policy]` keys); a JSON client sends `{"data": {...}}`, which the
/// JSON-body layer flattens to the same `data[...]` shape. Nested values
/// stay strings (upstream casts alert flags at read time).
fn data_json(pairs: &[(String, String)]) -> Result<serde_json::Value, ApiError> {
    if let Some(raw) = forms::get(pairs, "data") {
        if raw.trim().is_empty() {
            return Ok(serde_json::json!({}));
        }
        return serde_json::from_str(raw)
            .map_err(|_| ApiError::unprocessable("Validation failed: Data is invalid"));
    }
    let mut top = serde_json::Map::new();
    for (key, value) in pairs {
        let Some(rest) = key.strip_prefix("data[") else {
            continue;
        };
        let Some(inner) = rest.strip_suffix(']') else {
            continue;
        };
        if inner.is_empty() || inner.contains('[') || inner.contains(']') {
            // `data[][]` (bare arrays) have no settings meaning; a two-level
            // `data[alerts][mention]` path is handled below.
            if !inner.contains("][") {
                continue;
            }
        }
        if let Some((outer, leaf)) = inner.split_once("][") {
            let leaf = leaf.trim_end_matches(']');
            let entry = top
                .entry(outer.to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if let Some(object) = entry.as_object_mut() {
                object.insert(
                    leaf.to_string(),
                    serde_json::Value::String(value.clone()),
                );
            }
        } else {
            top.insert(inner.to_string(), serde_json::Value::String(value.clone()));
        }
    }
    Ok(serde_json::Value::Object(top))
}

/// `PUT /api/web/settings` (+ `PATCH` alias): web UI settings sync.
pub async fn settings_update(
    State(state): State<ApiState>,
    headers: HeaderMap,
    LenientForm(body): LenientForm,
) -> Response {
    let user = match session_user(&state, &headers) {
        Ok(user) => user,
        Err(err) => return err.into_response(),
    };
    if let Err(err) = check_csrf(&headers) {
        return err.into_response();
    }
    let data = match data_json(&body) {
        Ok(data) => data,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match web_cases::update_web_settings(store, user, &data.to_string()) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/web/push_subscriptions`.
pub async fn push_create(
    State(state): State<ApiState>,
    headers: HeaderMap,
    LenientForm(body): LenientForm,
) -> Response {
    let user = match session_user(&state, &headers) {
        Ok(user) => user,
        Err(err) => return err.into_response(),
    };
    if let Err(err) = check_csrf(&headers) {
        return err.into_response();
    }
    let subscription = match subscription_params(&body) {
        Ok(params) => params,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match web_cases::create_web_push_subscription(
        store,
        user,
        &subscription.endpoint,
        &subscription.key_p256dh,
        &subscription.key_auth,
        subscription.standard,
        &subscription.data.to_string(),
        state.context.now_ms(),
    ) {
        Ok(row) => web_push_response(&state, &row),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT /api/web/push_subscriptions/:id` (+ `PATCH` alias).
pub async fn push_update(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    let user = match session_user(&state, &headers) {
        Ok(user) => user,
        Err(err) => return err.into_response(),
    };
    if let Err(err) = check_csrf(&headers) {
        return err.into_response();
    }
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let data = match data_json(&body) {
        Ok(data) => data,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match web_cases::update_web_push_subscription(store, user, id, &data.to_string(), state.context.now_ms()) {
        Ok(row) => web_push_response(&state, &row),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/web/push_subscriptions/:id`.
///
/// Upstream (`PushSubscriptionsController#destroy`, v4.7.3) finds the row by
/// its unsubscribe token only and skips forgery protection for this action
/// (`skip_forgery_protection only: :destroy`): the token is a bearer
/// capability, so presenting it *is* the authorization and no session or
/// CSRF check applies. This handler keeps that path first and exempt.
///
/// The numeric-id fallback underneath is a birchtree extension (upstream has
/// no id lookup here): when the token misses and a session user acts on a
/// numeric id, the request is a state-changing session request like the
/// other web routes, so it must pass [`check_csrf`]. Without a session the
/// answer stays upstream's unconditional `200` (no user enumeration).
pub async fn push_destroy(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> Response {
    let user = session_user(&state, &headers).ok();
    let store: &dyn Store = &*state.context.store;
    match store.destroy_web_push_subscription_by_token(&raw) {
        Ok(Some(_)) => return axum::Json(serde_json::json!({})).into_response(),
        Ok(None) => {}
        Err(err) => return ApiError::from(err).into_response(),
    }
    let Some(user_id) = user else {
        return axum::Json(serde_json::json!({})).into_response();
    };
    let Ok(id) = raw.parse::<Id>() else {
        return axum::Json(serde_json::json!({})).into_response();
    };
    if let Err(err) = check_csrf(&headers) {
        return err.into_response();
    }
    match store.destroy_web_push_subscription(user_id, id) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

struct SubscriptionParams {
    endpoint: String,
    key_p256dh: String,
    key_auth: String,
    standard: bool,
    data: serde_json::Value,
}

/// `subscription[endpoint]`, `subscription[keys][p256dh|auth]`,
/// `subscription[standard]`, plus the `data` object.
fn subscription_params(pairs: &[(String, String)]) -> ApiResult<SubscriptionParams> {
    let get = |key: &str| forms::get(pairs, key).unwrap_or("").to_string();
    let endpoint = get("subscription[endpoint]");
    let key_p256dh = get("subscription[keys][p256dh]");
    let key_auth = get("subscription[keys][auth]");
    let standard = matches!(
        get("subscription[standard]").to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    );
    // `data` may arrive nested under the subscription key or top-level.
    let data_pairs: Vec<(String, String)> = pairs
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("subscription[data]")
                .map(|rest| (format!("data{rest}"), value.clone()))
        })
        .chain(
            pairs
                .iter()
                .filter(|(key, _)| key == "data" || key.starts_with("data["))
                .cloned(),
        )
        .collect();
    let data = data_json(&data_pairs)?;
    Ok(SubscriptionParams {
        endpoint,
        key_p256dh,
        key_auth,
        standard,
        data,
    })
}

/// `REST::WebPushSubscriptionSerializer`: `{id, endpoint, standard, alerts,
/// server_key, policy}`. Alert flags are cast to booleans at read time
/// (upstream's `ActiveModel::Type::Boolean` cast).
fn web_push_response(state: &ApiState, row: &WebPushSubscription) -> Response {
    let data: serde_json::Value = serde_json::from_str(&row.data).unwrap_or_default();
    let alerts = match data.get("alerts") {
        Some(serde_json::Value::Object(map)) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                let enabled = match value {
                    serde_json::Value::Bool(flag) => *flag,
                    serde_json::Value::String(text) => matches!(
                        text.to_ascii_lowercase().as_str(),
                        "true" | "1" | "t" | "yes" | "on"
                    ),
                    serde_json::Value::Number(number) => number.as_i64().unwrap_or(0) != 0,
                    _ => false,
                };
                out.insert(key.clone(), serde_json::Value::Bool(enabled));
            }
            serde_json::Value::Object(out)
        }
        _ => serde_json::json!({}),
    };
    let policy = data
        .get("policy")
        .and_then(|value| value.as_str())
        .unwrap_or("all");
    axum::Json(serde_json::json!({
        "id": row.id.to_string(),
        "endpoint": row.endpoint,
        "standard": row.standard,
        "alerts": alerts,
        "server_key": state.server.vapid_public_key,
        "policy": policy,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Embeds
// ---------------------------------------------------------------------------

/// `GET /api/web/embeds/:id`: OEmbed for a status.
///
/// Local statuses render the pinned `OEmbedSerializer` JSON when the viewer
/// (anonymous or signed-in) may see the status. Remote statuses require a
/// signed-in session and an outbound oEmbed fetch; a failed fetch answers
/// 404, as upstream does.
pub async fn embeds_show(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    uri: Uri,
) -> Response {
    let viewer = session_user(&state, &headers).ok();
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    let Some(status) = store.status_by_id(id).map_err(ApiError::from).ok().flatten() else {
        return ApiError::not_found().into_response();
    };
    if status.is_deleted() {
        return ApiError::not_found().into_response();
    }
    let Some(author) = store.account_by_id(status.account_id).ok().flatten() else {
        return ApiError::not_found().into_response();
    };
    if author.suspended {
        return ApiError::not_found().into_response();
    }
    if author.local {
        let pair = match store.pair_state(viewer.unwrap_or(status.account_id), status.account_id, state.context.now_ms()) {
            Ok(pair) => pair,
            Err(err) => return ApiError::from(err).into_response(),
        };
        if !policy::can_fetch(viewer, &status, pair) {
            return ApiError::not_found().into_response();
        }
        return oembed_response(&state, &status, &author, &uri);
    }
    // Remote: a signed-in session is required before any outbound fetch.
    if viewer.is_none() {
        return ApiError::new(StatusCode::UNAUTHORIZED, "Authentication required").into_response();
    }
    // No HTML discovery/sanitizer pass exists yet (see PATCHES): a remote
    // oEmbed fetch cannot be completed safely, so answer the fetch-failure
    // 404 rather than fabricating a document.
    ApiError::not_found().into_response()
}

/// Pinned `OEmbedSerializer` projection: `type`/`version` constants,
/// author/provider facts, `cache_age` 86400, the embed `html`, and
/// `width` (400 default, `?width=` override) / `height` (only when asked).
fn oembed_response(
    state: &ApiState,
    status: &crate::domain::Status,
    author: &crate::domain::Account,
    uri: &Uri,
) -> Response {
    let query = forms::parse(uri.query().unwrap_or("").as_bytes());
    let width: i64 = forms::get(&query, "width")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(400);
    let height: Option<i64> = forms::get(&query, "height").and_then(|raw| raw.parse().ok());
    let display_name = if author.display_name.trim().is_empty() {
        author.username.clone()
    } else {
        author.display_name.clone()
    };
    let status_url = format!(
        "{}/@{}/{}",
        state.server.public_url.trim_end_matches('/'),
        author.username,
        status.id
    );
    let html = format!(
        "<blockquote class=\"mastodon-embed\"><a href=\"{status_url}\">Post by @{}</a></blockquote>",
        author.username
    );
    let mut body = serde_json::Map::new();
    body.insert("type".into(), serde_json::json!("rich"));
    body.insert("version".into(), serde_json::json!("1.0"));
    body.insert("author_name".into(), serde_json::json!(display_name));
    body.insert("author_url".into(), serde_json::json!(author.actor_uri));
    body.insert("provider_name".into(), serde_json::json!(state.server.domain));
    body.insert(
        "provider_url".into(),
        serde_json::json!(state.server.public_url),
    );
    body.insert("cache_age".into(), serde_json::json!(86400));
    body.insert("html".into(), serde_json::json!(html));
    body.insert("width".into(), serde_json::json!(width));
    body.insert(
        "height".into(),
        height
            .map(|h| serde_json::json!(h))
            .unwrap_or(serde_json::Value::Null),
    );
    axum::Json(serde_json::Value::Object(body)).into_response()
}

// ---------------------------------------------------------------------------
// keystodon bundle
// ---------------------------------------------------------------------------

/// Mount point for the first-party bundle (mirrors the router PATCH).
pub const BUNDLE_MOUNT: &str = "/app";

/// Serve keystodon's built bundle from the operator-configured directory.
///
/// Unconfigured directory, missing directory, missing file, and path
/// traversal all answer 404 — a bundle that does not exist is never
/// fabricated. Directory traversal is refused by rejecting non-normal path
/// components.
pub async fn serve_bundle(State(state): State<ApiState>, uri: Uri) -> Response {
    let Some(dir) = state.server.web_client_dir.clone() else {
        return not_found_text();
    };
    let path_text = uri.path();
    let relative = match path_text.strip_prefix(BUNDLE_MOUNT) {
        Some(rest) => rest.trim_start_matches('/'),
        None => return not_found_text(),
    };
    let relative = if relative.is_empty() {
        "index.html".to_string()
    } else {
        relative.to_string()
    };
    let mut path = dir;
    for component in relative.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains('\\')
        {
            return not_found_text();
        }
        path.push(component);
    }
    match std::fs::read(&path) {
        Ok(bytes) => {
            let content_type = content_type_for(path.extension().and_then(|ext| ext.to_str()));
            match axum::http::header::HeaderValue::from_str(content_type) {
                Ok(value) => (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, value)],
                    bytes,
                )
                    .into_response(),
                Err(_) => not_found_text(),
            }
        }
        Err(_) => not_found_text(),
    }
}

fn not_found_text() -> Response {
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

fn content_type_for(extension: Option<&str>) -> &'static str {
    match extension.unwrap_or("").to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Status loads shared with the relation reads
// ---------------------------------------------------------------------------

/// Load viewer-aware status views for favourite/bookmark pages.
pub fn status_views(
    state: &ApiState,
    viewer: Option<Id>,
    statuses: Vec<crate::domain::Status>,
) -> Result<Vec<crate::api::mastodon::dto::StatusView>, ApiError> {
    let store: &dyn Store = &*state.context.store;
    load_status_views(
        &state.server,
        store,
        viewer,
        statuses,
        state.context.now_ms(),
    )
}

/// `REST::AccountSerializer[]` with real counters (relation reads).
pub fn accounts_json(
    state: &ApiState,
    store: &dyn Store,
    accounts: &[crate::domain::Account],
) -> Vec<serde_json::Value> {
    accounts
        .iter()
        .map(|account| {
            let stats = store.account_stats(account.id).ok();
            dto::account_json_with_stats(&state.server, account, stats.as_ref())
        })
        .collect()
}

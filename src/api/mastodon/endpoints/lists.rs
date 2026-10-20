//! List endpoints: CRUD + membership (`Api::V1::ListsController`,
//! `Api::V1::Lists::AccountsController`).

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::api::mastodon::ApiState;
use crate::application::lists;
use crate::application::ports::Store;
use crate::domain::{Error as DomainError, Id};

/// Read scopes for list reads.
const READ_SCOPES: &[&str] = &["read", "read:lists"];
/// Write scopes for list writes.
const WRITE_SCOPES: &[&str] = &["write", "write:lists"];
/// Mastodon `DEFAULT_ACCOUNTS_LIMIT` for the members endpoint.
const MEMBERS_DEFAULT_LIMIT: usize = 40;
const MEMBERS_MAX_LIMIT: usize = 80;
/// Bound for the `limit=0` unlimited path (upstream has no cap; a list holds
/// at most one row per account, so this is generous without unbounded reads).
const ALL_MEMBERS_CAP: usize = 10_000;

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Map list errors: Mastodon 422 validation bodies pass through, 404s use
/// the `Record not found` body.
fn list_error(err: DomainError) -> ApiError {
    match err {
        DomainError::NotFound(_) => ApiError::not_found(),
        other => ApiError::from(other),
    }
}

/// `GET /api/v1/lists` — all of the viewer's lists (no pagination in
/// v4.7.3).
pub async fn index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match lists::index(store, viewer) {
        Ok(rows) => {
            let body: Vec<_> = rows.iter().map(dto::list_json).collect();
            axum::Json(body).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/lists`.
pub async fn create(State(state): State<ApiState>, auth: Auth, LenientForm(body): LenientForm) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let title = forms::get(&body, "title").unwrap_or("");
    let replies_policy = forms::get(&body, "replies_policy").map(str::to_string);
    let exclusive = forms::get(&body, "exclusive").map(bool_cast);
    let store: &dyn Store = &*state.context.store;
    match lists::create(
        store,
        viewer,
        title,
        replies_policy,
        exclusive,
        state.context.now_ms(),
    ) {
        Ok(list) => axum::Json(dto::list_json(&list)).into_response(),
        Err(err) => list_error(err).into_response(),
    }
}

/// `GET /api/v1/lists/{id}`.
pub async fn show(State(state): State<ApiState>, auth: Auth, Path(raw): Path<String>) -> Response {
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
    match lists::show(store, viewer, id) {
        Ok(list) => axum::Json(dto::list_json(&list)).into_response(),
        Err(err) => list_error(err).into_response(),
    }
}

/// `PUT/PATCH /api/v1/lists/{id}`.
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
    let store: &dyn Store = &*state.context.store;
    match lists::update(
        store,
        viewer,
        id,
        forms::get(&body, "title").map(str::to_string),
        forms::get(&body, "replies_policy").map(str::to_string),
        forms::get(&body, "exclusive").map(bool_cast),
        state.context.now_ms(),
    ) {
        Ok(list) => axum::Json(dto::list_json(&list)).into_response(),
        Err(err) => list_error(err).into_response(),
    }
}

/// `DELETE /api/v1/lists/{id}` — `200 {}`.
pub async fn destroy(State(state): State<ApiState>, auth: Auth, Path(raw): Path<String>) -> Response {
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
    match lists::destroy(store, viewer, id) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => list_error(err).into_response(),
    }
}

/// `GET /api/v1/lists/{id}/accounts` — member accounts (`Link` cursors).
pub async fn accounts_index(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    uri: Uri,
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
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    // Mastodon: `limit=0` returns every member (`unlimited?`, no `Link`).
    // A present `max_id`/`since_id` with `limit=0` still returns everything
    // (upstream ignores cursors on the unlimited path).
    let unlimited = forms::get(&pairs, "limit").is_some_and(|raw| raw == "0");
    let limit: usize = match forms::get(&pairs, "limit") {
        None => MEMBERS_DEFAULT_LIMIT,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            if value < 0 {
                return ApiError::bad_request(
                    "Pagination values for `offset` and `limit` must be positive",
                )
                .into_response();
            }
            (value as usize).min(MEMBERS_MAX_LIMIT)
        }
    };
    let max_id = match forms::get(&pairs, "max_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => {
            return ApiError::bad_request("Invalid cursor value").into_response();
        }
        None => None,
    };
    let store: &dyn Store = &*state.context.store;
    // `limit=0` bypasses cursor paging entirely (upstream `unlimited?`).
    let members = if unlimited {
        match store.all_list_members(id, ALL_MEMBERS_CAP) {
            Ok(members) => members,
            Err(err) => return list_error(err).into_response(),
        }
    } else {
        match lists::members(store, viewer, id, max_id, limit) {
            Ok(members) => members,
            Err(err) => return list_error(err).into_response(),
        }
    };
    let accounts = store.accounts_by_ids(&members).unwrap_or_default();
    let stats = store.account_stats_by_ids(&members).unwrap_or_default();
    let mut body = Vec::new();
    for member in &members {
        if let Some(account) = accounts.get(member) {
            body.push(dto::account_json_with_stats(
                &state.server,
                account,
                stats.get(member),
            ));
        }
    }
    let mut response = axum::Json(body).into_response();
    // No `Link` on the unlimited path (upstream `unlimited?` skips paging).
    if !unlimited {
        if let Some(link) = pagination::link_header(
            &uri,
            members.last().copied(),
            members.first().copied(),
        ) {
            if let Ok(value) = axum::http::header::HeaderValue::from_str(&link) {
                response
                    .headers_mut()
                    .insert(axum::http::header::LINK, value);
            }
        }
    }
    response
}

/// `POST /api/v1/lists/{id}/accounts` — add members (`account_ids[]`).
pub async fn accounts_create(
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
    let account_ids: Vec<Id> = forms::all(&body, "account_ids")
        .iter()
        .filter_map(|raw| raw.parse::<Id>().ok())
        .collect();
    let store: &dyn Store = &*state.context.store;
    match lists::add_members(store, viewer, id, &account_ids) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => list_error(err).into_response(),
    }
}

/// `DELETE /api/v1/lists/{id}/accounts` — remove members (idempotent).
pub async fn accounts_destroy(
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
    let account_ids: Vec<Id> = forms::all(&body, "account_ids")
        .iter()
        .filter_map(|raw| raw.parse::<Id>().ok())
        .collect();
    let store: &dyn Store = &*state.context.store;
    match lists::remove_members(store, viewer, id, &account_ids) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => list_error(err).into_response(),
    }
}

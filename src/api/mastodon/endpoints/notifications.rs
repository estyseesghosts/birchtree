//! Notification endpoints (Phase 4): index with filters and `Link` cursors,
//! show, clear, dismiss, `unread_count`, and the notification-requests
//! surface (always empty until the notification-policy filter pipeline
//! lands — this server never fabricates rows).

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::forms;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::api::mastodon::status_view::load_status_views;
use crate::api::mastodon::ApiState;
use crate::application::notifications::{
    self, COUNT_DEFAULT_LIMIT, COUNT_MAX_LIMIT, DEFAULT_LIMIT, MAX_LIMIT, NotificationQuery,
};
use crate::application::ports::Store;
use crate::domain::{Id, NotificationType};

/// Read scopes for notification reads.
const READ_SCOPES: &[&str] = &["read", "read:notifications"];
/// Write scopes for clear/dismiss.
const WRITE_SCOPES: &[&str] = &["write", "write:notifications"];

fn account_id_of(actor: Id) -> Id {
    actor
}

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Parse a wire notification type (`types[]`/`exclude_types[]`).
fn parse_type(raw: &str) -> Option<NotificationType> {
    match raw {
        "mention" => Some(NotificationType::Mention),
        "follow" => Some(NotificationType::Follow),
        "reblog" => Some(NotificationType::Reblog),
        "favourite" => Some(NotificationType::Favourite),
        "poll" => Some(NotificationType::PollVote),
        "follow_request" => Some(NotificationType::FollowRequest),
        "quote" => Some(NotificationType::Quote),
        // Unknown values are silently ignored (Mastodon).
        _ => None,
    }
}

/// `GET /api/v1/notifications`.
pub async fn index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    // Mastodon `limit_param`: absent -> 20, else `min(abs(to_i), 40)` (so
    // `limit=0` and non-numeric values yield an empty page). Cursors keep
    // the shared numeric parsing.
    let limit: usize = match forms::get(&pairs, "limit") {
        None => DEFAULT_LIMIT,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            (value.unsigned_abs() as usize).min(MAX_LIMIT)
        }
    };
    let page = match pagination::parse(
        None,
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        limit,
        MAX_LIMIT,
    ) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    // `exclude_types[]` subtracts from the full set; `types[]` narrows to the
    // intersection (unknown values match nothing).
    let excluded: Vec<NotificationType> = forms::all(&pairs, "exclude_types")
        .iter()
        .filter_map(|raw| parse_type(raw))
        .collect();
    let explicit_types: Option<Vec<NotificationType>> = {
        let requested: Vec<NotificationType> = forms::all(&pairs, "types")
            .iter()
            .filter_map(|raw| parse_type(raw))
            .collect();
        if requested.is_empty() && forms::all(&pairs, "types").is_empty() {
            None
        } else if requested.is_empty() {
            // `types[]` present but nothing valid: the intersection is empty.
            Some(Vec::new())
        } else {
            Some(requested)
        }
    };

    let store: &dyn Store = &*state.context.store;
    let query = NotificationQuery {
        max_id: page.max_id,
        since_id: page.since_id,
        min_id: page.min_id,
        limit: page.limit,
    };
    let fetched = match notifications::index(store, recipient, query) {
        Ok(notifications) => notifications,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut notifications = fetched;
    if let Some(types) = &explicit_types {
        notifications.retain(|n| types.contains(&n.r#type));
    }
    notifications.retain(|n| !excluded.contains(&n.r#type));

    match render_notifications(&state, recipient, notifications) {
        Ok((body, first, last)) => {
            let mut response = axum::Json(body).into_response();
            if let Some(link) = pagination::link_header(&uri, last, first) {
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
        Err(err) => err.into_response(),
    }
}

/// Serialize a page of notifications (batched accounts + status views).
fn render_notifications(
    state: &ApiState,
    recipient: Id,
    notifications: Vec<crate::domain::Notification>,
) -> Result<(Vec<Value>, Option<Id>, Option<Id>), ApiError> {
    let store: &dyn Store = &*state.context.store;
    let first = notifications.first().map(|n| n.id);
    let last = notifications.last().map(|n| n.id);

    // Batch: actors, their stats, and the referenced statuses' views.
    let actor_ids: Vec<Id> = notifications
        .iter()
        .filter_map(|n| n.actor_id)
        .collect();
    let accounts = store.accounts_by_ids(&actor_ids)?;
    let stats = store.account_stats_by_ids(&actor_ids)?;

    let status_ids: Vec<Id> = notifications.iter().filter_map(|n| n.status_id).collect();
    let statuses = store.statuses_by_ids(&status_ids)?;
    let views = load_status_views(&state.server, store, Some(recipient), statuses, state.context.now_ms())?;
    let status_json: Vec<(Id, Value)> = views
        .into_iter()
        .map(|view| {
            let id = view.status.id;
            let json = dto::status_json(
                &state.server,
                Some(recipient),
                state.context.now_ms(),
                &view,
                dto::StatusSourceMode::Content,
            );
            (id, json)
        })
        .collect();

    let mut out = Vec::with_capacity(notifications.len());
    for notification in notifications {
        let actor = notification.actor_id;
        let account = match actor.and_then(|actor| accounts.get(&actor)) {
            Some(account) => dto::account_json_with_stats(
                &state.server,
                account,
                actor.and_then(|actor| stats.get(&account_id_of(actor))),
            ),
            // A notification whose actor row is gone cannot be rendered;
            // skip it (never fabricate an account).
            None => continue,
        };
        let status = notification
            .status_id
            .and_then(|id| status_json.iter().find(|(sid, _)| *sid == id).map(|(_, json)| json.clone()));
        out.push(dto::notification_json(&notification, account, status));
    }
    Ok((out, first, last))
}

/// `GET /api/v1/notifications/{id}`.
pub async fn show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match notifications::show(store, recipient, id) {
        Ok(notification) => {
            match render_notifications(&state, recipient, vec![notification]) {
                Ok((mut body, _, _)) => axum::Json(body.pop().unwrap_or(Value::Null)).into_response(),
                Err(err) => err.into_response(),
            }
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/notifications/clear` — delete every notification.
pub async fn clear(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match notifications::clear(store, recipient) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/notifications/{id}/dismiss` — delete one notification.
pub async fn dismiss(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match notifications::dismiss(store, recipient, id) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/notifications/unread_count` — `{"count": N}` against the
/// notifications marker (limit default 100, max 1000).
pub async fn unread_count(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let limit: u64 = match forms::get(&pairs, "limit")
        .map(|raw| raw.parse::<u64>().unwrap_or(COUNT_DEFAULT_LIMIT))
    {
        Some(value) => value.min(COUNT_MAX_LIMIT).max(1),
        None => COUNT_DEFAULT_LIMIT,
    };
    let _ = bool_cast; // `include_filtered` etc. have no effect without a filter pipeline
    let store: &dyn Store = &*state.context.store;
    match notifications::unread_count(store, recipient, limit) {
        Ok(count) => axum::Json(serde_json::json!({ "count": count })).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/notifications/requests` — always empty: this server does not
/// filter notifications yet, so no requests can exist (never fabricated).
pub async fn requests_index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    if let Err(err) = auth.0.require_user() {
        return err.into_response();
    }
    let _ = &state;
    axum::Json(Vec::<Value>::new()).into_response()
}

/// `GET /api/v1/notifications/requests/merged` — `{"merged": true}`: there is
/// no unmerge work because nothing is filtered.
pub async fn requests_merged(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    if let Err(err) = auth.0.require_user() {
        return err.into_response();
    }
    let _ = &state;
    axum::Json(serde_json::json!({ "merged": true })).into_response()
}

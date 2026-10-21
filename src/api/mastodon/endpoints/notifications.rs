//! Notification endpoints (Phase 4): index with filters and `Link` cursors,
//! show, clear, dismiss, `unread_count`, and the notification-requests
//! surface (always empty until the notification-policy filter pipeline
//! lands — this server never fabricates rows).

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::forms;
use crate::api::forms::LenientForm;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::api::mastodon::status_view::load_status_views;
use crate::api::mastodon::{ApiState, ServerInfo};
use crate::application::notifications::{
    self, COUNT_DEFAULT_LIMIT, COUNT_MAX_LIMIT, DEFAULT_LIMIT, MAX_LIMIT, NotificationQuery,
    PolicyChanges,
};
use crate::application::ports::Store;
use crate::domain::{Id, NotificationRequest, NotificationType};

/// Read scopes for notification reads.
const READ_SCOPES: &[&str] = &["read", "read:notifications"];
/// Write scopes for clear/dismiss.
const WRITE_SCOPES: &[&str] = &["write", "write:notifications"];

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Parse a wire notification type (`types[]`/`exclude_types[]`).
///
/// Covers the 7 types this server produces. The 10 remaining upstream types
/// (`status`, `update`, `severed_relationships`, `moderation_warning`,
/// `annual_report`, `admin.sign_up`, `admin.report`, `quoted_update`,
/// `added_to_collection`, `collection_update` — full list in the parent
/// snippet: `Notification::TYPES` at the pin needs new `NotificationType`
/// variants first, plus `as_str`/`filterable` entries and the status-bearing
/// Parse any upstream `types[]`/`exclude_types[]` entry
/// (`notification.rb:28-81` at the pin + `PROPERTIES` keys).
/// Unknown values are silently ignored (Mastodon). New types parse so
/// clients can name them; producers for unbuilt features get register rows,
/// not stubs.
fn parse_type(raw: &str) -> Option<NotificationType> {
    NotificationType::parse(raw)
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
    // Upstream `browserable_params`: `account_id` narrows to one actor;
    // `include_filtered` also serves filtered rows (the store page excludes
    // them — honored once the store gains the flag; accepted without effect
    // until then, see the use-case docs); `supported_types[]` only gates the
    // `fallback` rendering for non-baseline types, which this server never
    // emits, so it is accepted and carried on pagination links.
    let from_account = match forms::get(&pairs, "account_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => None,
        None => None,
    };
    let include_filtered = bool_cast(forms::get(&pairs, "include_filtered").unwrap_or("false"));
    // `supported_types[]` gates the `fallback` rendering for non-baseline
    // types (upstream `NotificationFallbackConcern`); absent = no fallbacks.
    let supported_types: Option<Vec<NotificationType>> = {
        let raw = forms::all(&pairs, "supported_types");
        if raw.is_empty() {
            None
        } else {
            Some(raw.iter().filter_map(|v| parse_type(v)).collect())
        }
    };

    let store: &dyn Store = &*state.context.store;
    let query = NotificationQuery {
        max_id: page.max_id,
        since_id: page.since_id,
        min_id: page.min_id,
        limit: page.limit,
        from_account,
        include_filtered,
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
    // `include_filtered=true` serves the filtered rows the store page
    // excludes (upstream `browserable`), merged newest-first by numeric id.
    if include_filtered {
        let extra = match store.filtered_notifications_page(
            recipient,
            page.limit,
            page.max_id,
            page.since_id,
            page.min_id,
        ) {
            Ok(extra) => extra,
            Err(err) => return ApiError::from(err).into_response(),
        };
        notifications = notifications::merge_filtered_index(notifications, extra, &query);
    }

    match render_notifications(&state.server, store, state.context.now_ms(), recipient, notifications, supported_types.as_deref()) {
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

/// Render a page of notifications for `recipient` (shared by the REST index
/// and the notification streams, so both show the same payloads).
/// `supported` is the v1 `supported_types[]` gate for `fallback` rendering
/// (`None` = absent = no fallbacks; streams always pass `None`, like upstream).
pub fn render_notifications(
    server: &ServerInfo,
    store: &dyn Store,
    now_ms: u64,
    recipient: Id,
    notifications: Vec<crate::domain::Notification>,
    supported: Option<&[NotificationType]>,
) -> Result<(Vec<Value>, Option<Id>, Option<Id>), ApiError> {
    let first = notifications.first().map(|n| n.id);
    let last = notifications.last().map(|n| n.id);

    // Batch: actors (+ the recipient itself for actor-less rows, which
    // upstream maps to the recipient's own account), their stats, and the
    // referenced statuses' views.
    let mut actor_ids: Vec<Id> = notifications
        .iter()
        .filter_map(|n| n.actor_id)
        .collect();
    if notifications.iter().any(|n| n.actor_id.is_none()) && !actor_ids.contains(&recipient) {
        actor_ids.push(recipient);
    }
    let accounts = store.accounts_by_ids(&actor_ids)?;
    let stats = store.account_stats_by_ids(&actor_ids)?;

    let status_ids: Vec<Id> = notifications.iter().filter_map(|n| n.status_id).collect();
    let statuses = store.statuses_by_ids(&status_ids)?;
    let mut views = load_status_views(
        server,
        store,
        Some(recipient),
        statuses,
        now_ms,
    )?;
    // `filtered` annotation (filter v2): one batched match per page, every
    // active filter — matching Mastodon's relationships presenter.
    let page: Vec<crate::domain::Status> = views.iter().map(|view| view.status.clone()).collect();
    let matches = crate::application::filters::filter_results_unscoped(
        store,
        recipient,
        &page,
        now_ms,
    )?;
    for view in &mut views {
        if let Some(entries) = matches.get(&view.status.id) {
            view.filtered = entries.clone();
        }
    }
    let status_json: Vec<(Id, Value)> = views
        .into_iter()
        .map(|view| {
            let id = view.status.id;
            let json = dto::status_json(
                server,
                Some(recipient),
                now_ms,
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
            Some(account) => account,
            // Actor-less rows (warnings, severances, annual reports, admin
            // events) render against the recipient's own account (upstream
            // `set_from_account`). A row whose actor row is gone is still
            // skipped (never fabricate an account).
            None => match actor {
                None => match accounts.get(&recipient) {
                    Some(account) => account,
                    None => continue,
                },
                Some(_) => continue,
            },
        };
        let actor_name = if account.display_name.trim().is_empty() {
            account.username.clone()
        } else {
            account.display_name.clone()
        };
        let account = dto::account_json_with_stats(server, account, stats.get(&actor.unwrap_or(recipient)));
        let status = notification
            .status_id
            .and_then(|id| status_json.iter().find(|(sid, _)| *sid == id).map(|(_, json)| json.clone()));
        let mut rendered = dto::notification_json(&notification, account, status);
        if notifications::needs_fallback(notification.r#type, supported, false) {
            // activity_present=false: our rows carry no severance-event /
            // report / collection linkage yet, so those three get no fallback
            // (never fabricated); moderation_warning + admin.sign_up do.
            // Revisit when activity linkage lands.
            rendered["fallback"] = notifications::fallback_for(notification.r#type, &actor_name);
        }
        out.push(rendered);
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
            match render_notifications(
                &state.server,
                store,
                state.context.now_ms(),
                recipient,
                vec![notification],
                // `show` carries no query string: no `supported_types[]`.
                None,
            ) {
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

/// `GET /api/v1/notifications/requests` — the recipient's live request rows,
/// newest first (round 3, area C; closes the always-`[]` D20 interim).
/// Suspended senders are excluded (upstream `without_suspended`); a request
/// whose actor or last status row is gone renders `null` for that half,
/// never fabricated.
pub async fn requests_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
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
    // Upstream `limit_param(DEFAULT_ACCOUNTS_LIMIT)`: absent -> 40.
    let limit: usize = match forms::get(&pairs, "limit") {
        None => 40,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            (value.unsigned_abs() as usize).min(80)
        }
    };
    let page = match pagination::parse(
        None,
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        limit,
        80,
    ) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let rows = match notifications::requests(store, recipient) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let rows: Vec<NotificationRequest> = rows
        .into_iter()
        .filter(|row| {
            if let Some(max_id) = page.max_id {
                if row.id.value() >= max_id.value() {
                    return false;
                }
            }
            if let Some(since_id) = page.since_id {
                if row.id.value() <= since_id.value() {
                    return false;
                }
            }
            if let Some(min_id) = page.min_id {
                if row.id.value() <= min_id.value() {
                    return false;
                }
            }
            true
        })
        .take(page.limit)
        .collect();
    match render_requests(&state.server, store, state.context.now_ms(), recipient, rows) {
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

/// Render request rows (`REST::NotificationRequestSerializer`): `{id,
/// created_at, updated_at, notifications_count, account, last_status}`.
/// `notifications_count` is a string, like upstream's `.to_s`.
fn render_requests(
    server: &ServerInfo,
    store: &dyn Store,
    now_ms: u64,
    recipient: Id,
    rows: Vec<NotificationRequest>,
) -> Result<(Vec<Value>, Option<Id>, Option<Id>), ApiError> {
    let first = rows.first().map(|row| row.id);
    let last = rows.last().map(|row| row.id);
    let actor_ids: Vec<Id> = rows.iter().map(|row| row.from_account_id).collect();
    let accounts = store.accounts_by_ids(&actor_ids)?;
    let stats = store.account_stats_by_ids(&actor_ids)?;
    let status_ids: Vec<Id> = rows.iter().filter_map(|row| row.last_status_id).collect();
    let statuses = store.statuses_by_ids(&status_ids)?;
    let views = load_status_views(server, store, Some(recipient), statuses, now_ms)?;
    let status_json: Vec<(Id, Value)> = views
        .into_iter()
        .map(|view| {
            let id = view.status.id;
            let json = dto::status_json(
                server,
                Some(recipient),
                now_ms,
                &view,
                dto::StatusSourceMode::Content,
            );
            (id, json)
        })
        .collect();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(actor) = accounts.get(&row.from_account_id) else {
            continue;
        };
        // Suspended senders are excluded, like upstream.
        if actor.suspended {
            continue;
        }
        let account = dto::account_json_with_stats(server, actor, stats.get(&row.from_account_id));
        let last_status = row
            .last_status_id
            .and_then(|id| status_json.iter().find(|(sid, _)| *sid == id))
            .map(|(_, json)| json.clone())
            .unwrap_or(Value::Null);
        let count = store
            .count_filtered_notifications(recipient, row.from_account_id)
            .unwrap_or(0);
        out.push(serde_json::json!({
            "id": row.id.to_string(),
            "created_at": iso_ms(row.created_at_ms),
            "updated_at": iso_ms(row.updated_at_ms),
            "notifications_count": count.to_string(),
            "account": account,
            "last_status": last_status,
        }));
    }
    Ok((out, first, last))
}

fn iso_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|time| {
            time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// `GET /api/v1/notifications/requests/merged` — `{"merged": bool}`:
/// accepts run synchronously, so this is exactly "no pending requests"
/// (upstream `requests#merged?` reads its async worker counter instead).
pub async fn requests_merged(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match notifications::merged(store, recipient) {
        Ok(merged) => axum::Json(serde_json::json!({ "merged": merged })).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/notifications/requests/:id`.
pub async fn requests_show(
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
    match notifications::request_show(store, recipient, id) {
        Ok(row) => {
            match render_requests(
                &state.server,
                store,
                state.context.now_ms(),
                recipient,
                vec![row],
            ) {
                Ok((mut body, _, _)) => {
                    match body.pop() {
                        Some(value) => axum::Json(value).into_response(),
                        // A suspended sender renders nothing (no fabrication).
                        None => ApiError::not_found().into_response(),
                    }
                }
                Err(err) => err.into_response(),
            }
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/notifications/requests/:id/accept` — unhide the actor's
/// rows and emit `notifications_merged` (only on a real merge). `200 {}`.
pub async fn requests_accept(
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
    match notifications::accept_request(store, &*state.context.streaming, recipient, id) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/notifications/requests/:id/dismiss` — delete the actor's
/// filtered rows and the request. Silent (no event). `200 {}`.
pub async fn requests_dismiss(
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
    match notifications::dismiss_request(store, recipient, id) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Bulk `id[]` ids from the query string and/or the form/JSON body
/// (upstream scopes the set to the recipient's rows; unknown or foreign ids
/// simply match nothing).
fn bulk_ids(uri: &Uri, body: &[(String, String)]) -> Vec<Id> {
    let mut ids = Vec::new();
    if let Some(query) = uri.query() {
        for (key, value) in forms::parse(query.as_bytes()) {
            if key == "id" || key == "id[]" {
                if let Ok(id) = value.parse::<Id>() {
                    ids.push(id);
                }
            }
        }
    }
    for raw in forms::all(body, "id") {
        if let Ok(id) = raw.parse::<Id>() {
            ids.push(id);
        }
    }
    ids.sort_by_key(|id| id.value());
    ids.dedup_by_key(|id| id.value());
    ids
}

/// `POST /api/v1/notifications/requests/accept` — bulk accept. `200 {}`.
pub async fn requests_accept_bulk(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let ids = bulk_ids(&uri, &body);
    let store: &dyn Store = &*state.context.store;
    match notifications::accept_requests_bulk(store, &*state.context.streaming, recipient, &ids) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/notifications/requests/dismiss` — bulk dismiss. `200 {}`.
pub async fn requests_dismiss_bulk(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let ids = bulk_ids(&uri, &body);
    let store: &dyn Store = &*state.context.store;
    match notifications::dismiss_requests_bulk(store, recipient, &ids) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/notifications/policy` (`REST::V1::NotificationPolicySerializer`).
pub async fn policy_show(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match notifications::policy(store, recipient) {
        Ok(policy) => match policy_json(store, recipient, &policy) {
            Ok(body) => axum::Json(body).into_response(),
            Err(err) => ApiError::from(err).into_response(),
        },
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT /api/v1/notifications/policy` — set `filter_*` flags (absent flags
/// keep their value; `for_limited_accounts` is not settable here, exactly
/// like upstream's V1 params).
pub async fn policy_update(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let recipient = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let flag = |name: &str| forms::get(&body, name).map(|raw| bool_cast(raw));
    let changes = PolicyChanges {
        filter_not_following: flag("filter_not_following"),
        filter_not_followers: flag("filter_not_followers"),
        filter_new_accounts: flag("filter_new_accounts"),
        filter_private_mentions: flag("filter_private_mentions"),
        filter_bots: flag("filter_bots"),
    };
    let store: &dyn Store = &*state.context.store;
    match notifications::update_policy(store, recipient, changes) {
        Ok(policy) => match policy_json(store, recipient, &policy) {
            Ok(rendered) => axum::Json(rendered).into_response(),
            Err(err) => ApiError::from(err).into_response(),
        },
        Err(err) => ApiError::from(err).into_response(),
    }
}

fn policy_json(
    store: &dyn Store,
    recipient: Id,
    policy: &crate::domain::NotificationPolicy,
) -> Result<Value, ApiError> {
    let requests = store.notification_requests(recipient)?;
    let mut pending_notifications = 0u64;
    for row in &requests {
        pending_notifications += store
            .count_filtered_notifications(recipient, row.from_account_id)
            .unwrap_or(0);
    }
    Ok(serde_json::json!({
        "filter_not_following": policy.filter_not_following(),
        "filter_not_followers": policy.filter_not_followers(),
        "filter_new_accounts": policy.filter_new_accounts(),
        "filter_private_mentions": policy.filter_private_mentions(),
        "filter_bots": policy.filter_bots(),
        "summary": {
            "pending_requests_count": requests.len() as u64,
            "pending_notifications_count": pending_notifications,
        },
    }))
}

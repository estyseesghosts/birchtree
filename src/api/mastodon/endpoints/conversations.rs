//! Conversation endpoints (Phase 7) and conversation mutes on statuses.
//!
//! Upstream reference: Mastodon v4.7.3 (`8ff11c2b`)
//! `app/controllers/api/v1/conversations_controller.rb` (LIMIT 20, `read` /
//! `read:statuses` on index, `write` / `write:conversations` elsewhere,
//! paginated by `last_status_id`, `render_empty` on destroy) and
//! `app/controllers/api/v1/statuses/mutes_controller.rb` (`write` /
//! `write:mutes`, 200 status DTO with the mute reflected).
//!
//! Handlers stay thin: authenticate, parse, call one application use case,
//! serialize. Per-viewer state and streaming fan-out live in
//! `application::conversations` (fan-out publishes after commit, never inside
//! a storage transaction).

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto::{self, StatusSourceMode};
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::api::mastodon::status_view::{load_status_view, load_status_views};use crate::api::mastodon::{ApiState, ServerInfo};
use crate::application::conversations::{
    self, ConversationQuery, ConversationView, DEFAULT_LIMIT, MAX_LIMIT,
};
use crate::application::ports::Store;
use crate::application::streaming::StreamEvent;
use crate::domain::{Error as DomainError, Id};

/// Read scopes for the conversation list.
const READ_SCOPES: &[&str] = &["read", "read:statuses"];
/// Write scopes for read/unread/delete.
const CONVERSATION_WRITE_SCOPES: &[&str] = &["write", "write:conversations"];
/// Write scopes for conversation mutes on statuses.
const MUTE_SCOPES: &[&str] = &["write", "write:mutes"];

/// `404 {"error":"Not Found"}` (status sub-routes and conversation lookups
/// answer the explicit body, matching the pinned controller).
fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({ "error": "Not Found" })),
    )
        .into_response()
}

/// Map a domain error from a conversation route: a missing/invisible thread
/// or status answers the explicit `Not Found` body.
fn conversation_error(err: DomainError) -> Response {
    match err {
        DomainError::NotFound(ref message)
            if message == "conversation" || message == "status" =>
        {
            not_found()
        }
        other => ApiError::from(other).into_response(),
    }
}

/// `GET /api/v1/conversations`: the viewer's threads, newest first, with
/// `Link` cursors over `last_status_id`.
pub async fn index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let page = match pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        DEFAULT_LIMIT,
        MAX_LIMIT,
    ) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let views = match conversations::list_conversations(
        store,
        viewer,
        ConversationQuery {
            max_id: page.max_id,
            since_id: page.since_id,
            min_id: page.min_id,
            limit: page.limit,
        },
        now,
    ) {
        Ok(views) => views,
        Err(err) => return conversation_error(err),
    };
    let mut body = Vec::with_capacity(views.len());
    for view in &views {
        match conversation_json(&state.server, store, viewer, view, now) {
            Ok(json) => body.push(json),
            Err(err) => return err.into_response(),
        }
    }
    let mut response = axum::Json(body).into_response();
    // `next` only on a full page (upstream `records_continue?`); `prev`
    // whenever the page is non-empty.
    let next = if views.len() == page.limit {
        views.last().map(|view| view.last_status.id)
    } else {
        None
    };
    let prev = views.first().map(|view| view.last_status.id);
    if next.is_some() || prev.is_some() {
        if let Some(link) = pagination::link_header(&uri, next, prev) {
            if let Ok(value) = axum::http::header::HeaderValue::from_str(&link) {
                response.headers_mut().insert(axum::http::header::LINK, value);
            }
        }
    }
    response
}

/// `DELETE /api/v1/conversations/:id`: remove the viewer's participation
/// state (read position and all); statuses are untouched. `200 {}`.
pub async fn destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(CONVERSATION_WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(root) = raw.parse::<Id>() else {
        return not_found();
    };
    let store: &dyn Store = &*state.context.store;
    match conversations::delete_conversation(store, viewer, root, state.context.now_ms()) {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(err) => conversation_error(err),
    }
}

/// `POST /api/v1/conversations/:id/read`: clear the unread flag (idempotent).
pub async fn read(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(CONVERSATION_WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(root) = raw.parse::<Id>() else {
        return not_found();
    };
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    match conversations::mark_read(store, viewer, root, now) {
        Ok(view) => {
            // Upstream's `after_commit` fans out on updates as well (not only
            // on creation).
            state.context.streaming.publish(StreamEvent::Conversation {
                viewer_id: viewer,
                status_id: view.last_status.id,
            });
            match conversation_json(&state.server, store, viewer, &view, now) {
                Ok(json) => axum::Json(json).into_response(),
                Err(err) => err.into_response(),
            }
        }
        Err(err) => conversation_error(err),
    }
}

/// `POST /api/v1/conversations/:id/unread`: force the unread flag (idempotent).
pub async fn unread(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(CONVERSATION_WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(root) = raw.parse::<Id>() else {
        return not_found();
    };
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    match conversations::mark_unread(store, viewer, root, now) {
        Ok(view) => {
            state.context.streaming.publish(StreamEvent::Conversation {
                viewer_id: viewer,
                status_id: view.last_status.id,
            });
            match conversation_json(&state.server, store, viewer, &view, now) {
                Ok(json) => axum::Json(json).into_response(),
                Err(err) => err.into_response(),
            }
        }
        Err(err) => conversation_error(err),
    }
}

/// `POST /api/v1/statuses/:id/mute`: mute the status's conversation
/// (idempotent); answers the 200 status DTO with `muted: true`.
pub async fn mute_status(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    _form: LenientForm,
) -> Response {
    mute_unmute(state, auth, &raw, true).await
}

/// `POST /api/v1/statuses/:id/unmute`: unmute (no-op when not muted);
/// answers the 200 status DTO with `muted: false`.
pub async fn unmute_status(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    _form: LenientForm,
) -> Response {
    mute_unmute(state, auth, &raw, false).await
}

async fn mute_unmute(state: ApiState, auth: Auth, raw: &str, muted: bool) -> Response {
    if let Err(err) = auth.0.require_scopes(MUTE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(status_id) = raw.parse::<Id>() else {
        return not_found();
    };
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let status = if muted {
        conversations::mute_status_conversation(store, viewer, status_id, now)
    } else {
        conversations::unmute_status_conversation(store, viewer, status_id, now)
    };
    let status = match status {
        Ok(status) => status,
        Err(err) => return conversation_error(err),
    };
    match load_status_view(&state.server, store, Some(viewer), status, now) {
        Ok(view) => {
            let mut rendered = dto::status_json(&state.server, Some(viewer), now, &view, StatusSourceMode::Content);
            // The DTO's `muted` comes from the conversation-mute rows (parent
            // patch to `status_view`); pin the value for this response either
            // way so mute/unmute read back immediately.
            rendered["muted"] = json!(muted);
            axum::Json(rendered).into_response()
        }
        Err(err) => err.into_response(),
    }
}

/// Publish one `conversation` streaming event per participant after a direct
/// status commits. The endpoint calls this after commit, never inside a
/// storage transaction.
pub fn publish_conversation_events(state: &ApiState, status: &crate::domain::Status) {
    for viewer in conversations::conversation_event_recipients(status) {
        state.context.streaming.publish(StreamEvent::Conversation {
            viewer_id: viewer,
            status_id: status.id,
        });
    }
}

// ---------------------------------------------------------------------------
// Wire rendering (`REST::ConversationSerializer`), shared by the REST index
// and the streaming fan-out so both emit the same payload.
// ---------------------------------------------------------------------------

/// Fill a freshly loaded status view's muted flag from the viewer's
/// conversation mutes.
///
/// The shared page loader cannot do this itself (it batches by status id,
/// while mutes key by thread root), so conversation-adjacent renders call
/// this explicitly. The global status DTO path is wired by the parent patch
/// (`status_view` fills `flags.muted` the same way).
pub fn apply_muted_flag(
    store: &dyn Store,
    viewer: Id,
    view: &mut dto::StatusView,
) -> Result<(), ApiError> {
    let muted = conversations::status_muted(store, viewer, &view.status)?;
    view.flags.muted = muted;
    Ok(())
}

/// Render one conversation for the wire (`{id, unread, accounts,
/// last_status}`).
///
/// `id` is the thread-root status id (stable and opaque to clients; upstream
/// serves its `account_conversations` row id). `last_status` is packed with
/// the shared viewer-aware loader so it matches the status DTO exactly.
pub fn conversation_json(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Id,
    view: &ConversationView,
    now_ms: u64,
) -> Result<serde_json::Value, ApiError> {
    let accounts = store.accounts_by_ids(&view.participant_ids)?;
    let stats = store.account_stats_by_ids(&view.participant_ids)?;
    let mut account_json: Vec<serde_json::Value> = Vec::new();
    for id in &view.participant_ids {
        if let Some(account) = accounts.get(id) {
            account_json.push(dto::account_json_with_stats(
                server,
                account,
                stats.get(id),
            ));
        }
    }
    // Upstream falls back to `[account]`; an empty participant list keeps the
    // viewer's own account so `accounts` is never empty.
    if account_json.is_empty() {
        let accounts = store.accounts_by_ids(&[viewer])?;
        if let Some(account) = accounts.get(&viewer) {
            let stats = store.account_stats_by_ids(&[viewer])?;
            account_json.push(dto::account_json_with_stats(
                server,
                account,
                stats.get(&viewer),
            ));
        }
    }
    let mut views = load_status_views(
        server,
        store,
        Some(viewer),
        vec![view.last_status.clone()],
        now_ms,
    )?;
    let mut status_view = views.pop().ok_or_else(ApiError::internal)?;
    apply_muted_flag(store, viewer, &mut status_view)?;
    let muted = status_view.flags.muted;
    let mut last_status = dto::status_json(
        server,
        Some(viewer),
        now_ms,
        &status_view,
        StatusSourceMode::Content,
    );
    // Pin the value for this response either way, so the mute/unmute routes
    // read back immediately with or without the global `dto` patch.
    last_status["muted"] = json!(muted);
    Ok(json!({
        "id": view.root_id.to_string(),
        "unread": view.unread,
        "accounts": account_json,
        "last_status": last_status,
    }))
}

/// Render a conversation for the `direct` streaming channel (`None` when the
/// status is gone, non-direct, or invisible to `viewer`).
pub fn render_conversation_event(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<Option<serde_json::Value>, String> {
    use crate::domain::Visibility;

    let status = store
        .status_by_id(status_id)
        .map_err(|err| format!("{err:?}"))?;
    let Some(status) = status.filter(|status| !status.is_deleted()) else {
        return Ok(None);
    };
    if status.visibility != Visibility::Direct {
        return Ok(None);
    }
    if !conversations::fetchable(store, viewer, &status, now_ms)
        .map_err(|err| format!("{err:?}"))?
    {
        return Ok(None);
    }
    let view = conversations::get_conversation(store, viewer, status.id, now_ms)
        .map_err(|err| format!("{err:?}"))?;
    conversation_json(server, store, viewer, &view, now_ms)
        .map(Some)
        .map_err(|err| format!("{err:?}"))
}

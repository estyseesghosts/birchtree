//! Status endpoints: CRUD, source/context/history, multi-fetch, interactions
//! (favourite/bookmark/reblog/pin), favourited/reblogged-by lists, quotes and
//! the interaction policy.
//!
//! Two 404 bodies exist in the pinned source and are reproduced here:
//! status sub-routes (`show?` in the base controller) answer
//! `{"error":"Not Found"}`, while `update`/`destroy` (`ActiveRecord::RecordNotFound`)
//! answer `{"error":"Record not found"}`. Reply/quote creation misses carry
//! their exact custom messages.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto::{self, StatusSourceMode};
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::pagination;
use crate::api::mastodon::status_view::load_status_view;
use crate::api::mastodon::ApiState;
use crate::application::ports::{AccountStatusesQuery, Store};
use crate::application::streaming::StreamEvent;
use crate::application::statuses::{
    self, CreateStatusCommand, CreateStatusResult, PollCommand, UpdateStatusCommand,
};
use crate::domain::{Error as DomainError, Id, Status};
pub mod interactions;

pub use self::interactions::{bookmark, favourited_by, favourite, pin, reblog, reblogged_by, unbookmark, unfavourite, unpin, unreblog};

/// Read scopes for status reads.
const READ_SCOPES: &[&str] = &["read", "read:statuses"];
/// Write scopes for status writes.
const WRITE_SCOPES: &[&str] = &["write", "write:statuses"];

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Map a domain error from a status sub-route: generic lookups 404 with
/// Mastodon's `"Not Found"` body; explicit messages pass through.
fn status_error(err: DomainError) -> ApiError {
    match err {
        DomainError::NotFound(ref msg)
            if msg == "status"
                || msg == "account"
                || msg == "poll"
                || msg == "Record not found" =>
        {
            ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found")
        }
        other => ApiError::from(other),
    }
}

/// `422 {"error":"Mastodon::ValidationError"}` (Mastodon raises the class
/// without a message, e.g. for too many `id[]`).
fn mastodon_validation_error() -> Response {
    (
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(serde_json::json!({ "error": "Mastodon::ValidationError" })),
    )
        .into_response()
}

/// The viewer id carried by an optional token.
fn viewer_of(auth: &OptionalAuth) -> Option<Id> {
    auth.0.as_ref().and_then(|a| a.token.user_id)
}

/// Scope check for optional-auth reads (anonymous passes).
fn optional_scopes(auth: &OptionalAuth, required: &[&str]) -> Result<(), ApiError> {
    match &auth.0 {
        Some(authenticated) => authenticated.require_scopes(required),
        None => Ok(()),
    }
}

/// Fill `view.filtered` with the batched filter matches for the page
/// (every active filter, like Mastodon's relationships presenter).
fn apply_filter_results(
    state: &ApiState,
    store: &dyn Store,
    viewer: Option<Id>,
    views: &mut [crate::api::mastodon::dto::StatusView],
) -> Result<(), ApiError> {
    let Some(viewer) = viewer else {
        return Ok(()); // Anonymous payloads carry no `filtered` entries.
    };
    let page: Vec<Status> = views.iter().map(|view| view.status.clone()).collect();
    let matches = crate::application::filters::filter_results_unscoped(
        store,
        viewer,
        &page,
        state.context.now_ms(),
    )?;
    for view in views.iter_mut() {
        if let Some(entries) = matches.get(&view.status.id) {
            view.filtered = entries.clone();
        }
    }
    Ok(())
}

/// Core extension CORE-T: `(type, description)` pairs for the nag signal.
/// A lookup failure reads as no attachments (no nag), never a 500: the
/// status already committed.
fn alt_pairs(store: &dyn Store, media_ids: &[Id]) -> Vec<(String, String)> {
    store
        .media_by_ids(media_ids)
        .map(|rows| {
            media_ids
                .iter()
                .filter_map(|id| {
                    rows.get(id)
                        .map(|m| (m.r#type.clone(), m.description.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Assemble and render one status as 200 JSON.
fn status_response(
    state: &ApiState,
    viewer: Option<Id>,
    status: Status,
    source: StatusSourceMode,
) -> Response {
    let store: &dyn Store = &*state.context.store;
    match load_status_view(&state.server, store, viewer, status, state.context.now_ms()) {
        Ok(mut view) => {
            if let Err(err) = apply_filter_results(state, store, viewer, std::slice::from_mut(&mut view))
            {
                return err.into_response();
            }
            axum::Json(dto::status_json(
                &state.server,
                viewer,
                state.context.now_ms(),
                &view,
                source,
            ))
            .into_response()
        }
        Err(err) => err.into_response(),
    }
}

/// Assemble and render a page of statuses as 200 JSON.
///
/// One batched [`StatusViewContext`] page loader for the whole page (Phase 4):
/// the transaction count is constant in the page size.
fn status_list_response(
    state: &ApiState,
    viewer: Option<Id>,
    statuses: Vec<Status>,
) -> Response {
    let store: &dyn Store = &*state.context.store;
    match crate::api::mastodon::status_view::load_status_views(
        &state.server,
        store,
        viewer,
        statuses,
        state.context.now_ms(),
    ) {
        Ok(mut views) => {
            if let Err(err) = apply_filter_results(state, store, viewer, &mut views) {
                return err.into_response();
            }
            let array: Vec<Value> = views
                .iter()
                .map(|view| {
                    dto::status_json(
                        &state.server,
                        viewer,
                        state.context.now_ms(),
                        view,
                        StatusSourceMode::Content,
                    )
                })
                .collect();
            axum::Json(array).into_response()
        }
        Err(err) => err.into_response(),
    }
}

/// `GET /api/v1/statuses` — multi-fetch by `id[]` (max 20, silently omitting
/// statuses the viewer cannot see, ordered id-descending).
pub async fn index(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    uri: Uri,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_SCOPES) {
        return err.into_response();
    }
    let viewer = viewer_of(&auth);
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let raw_ids = forms::all(&pairs, "id");
    if raw_ids.len() > 20 {
        return mastodon_validation_error();
    }
    let mut ids = Vec::with_capacity(raw_ids.len());
    for raw in &raw_ids {
        if let Ok(id) = raw.parse::<Id>() {
            ids.push(id);
        }
    }
    let store: &dyn Store = &*state.context.store;
    match statuses::statuses_by_ids(store, viewer, &ids, state.context.now_ms()) {
        Ok(statuses) => status_list_response(&state, viewer, statuses),
        Err(err) => status_error(err).into_response(),
    }
}

/// `GET /api/v1/statuses/{id}` — one status under the show gate.
pub async fn show(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_SCOPES) {
        return err.into_response();
    }
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let viewer = viewer_of(&auth);
    let store: &dyn Store = &*state.context.store;
    match statuses::show_status(store, viewer, id, state.context.now_ms()) {
        Ok(status) => status_response(&state, viewer, status, StatusSourceMode::Content),
        Err(err) => status_error(err).into_response(),
    }
}

/// `GET /api/v1/statuses/{id}/source` — `{id, text, spoiler_text}`; the token
/// is mandatory (401 when absent, unlike status show).
pub async fn source(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let viewer = auth.0.token.user_id;
    let store: &dyn Store = &*state.context.store;
    match statuses::status_source(store, viewer, id, state.context.now_ms()) {
        Ok(status) => axum::Json(dto::status_source_json(&status)).into_response(),
        Err(err) => status_error(err).into_response(),
    }
}

/// `GET /api/v1/statuses/{id}/context` — ancestors/descendants around a
/// status; limits depend on whether the caller presented a token.
pub async fn context(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_SCOPES) {
        return err.into_response();
    }
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let viewer = viewer_of(&auth);
    let store: &dyn Store = &*state.context.store;
    match statuses::status_context(
        store,
        viewer,
        id,
        state.context.now_ms(),
        auth.0.is_some(),
    ) {
        Ok(tree) => {
            let mut ancestors = Vec::with_capacity(tree.ancestors.len());
            let mut descendants = Vec::with_capacity(tree.descendants.len());
            for status in tree.ancestors {
                match load_status_view(&state.server, store, viewer, status, state.context.now_ms())
                {
                    Ok(view) => ancestors.push(dto::status_json(
                        &state.server,
                        viewer,
                        state.context.now_ms(),
                        &view,
                        StatusSourceMode::Content,
                    )),
                    Err(err) => return err.into_response(),
                }
            }
            for status in tree.descendants {
                match load_status_view(&state.server, store, viewer, status, state.context.now_ms())
                {
                    Ok(view) => descendants.push(dto::status_json(
                        &state.server,
                        viewer,
                        state.context.now_ms(),
                        &view,
                        StatusSourceMode::Content,
                    )),
                    Err(err) => return err.into_response(),
                }
            }
            // Remote threads complete on an authenticated read (Phase 6, contract B/C).
            // The refresh ID is announced so the client polls again.
            let refresh = if auth.0.is_some() {
                match store.status_by_id(id) {
                    Ok(Some(root)) => crate::federation::threads::read_time_completion(store, &root, state.context.now_ms())
                        .unwrap_or_else(|err| {
                            tracing::warn!(error = %err, "thread completion not scheduled");
                            None
                        }),
                    _ => None,
                }
            } else {
                None
            };
            let mut response = axum::Json(serde_json::json!({
                "ancestors": ancestors,
                "descendants": descendants,
            }))
            .into_response();
            if let Some(refresh_id) = refresh {
                if let Ok(value) = axum::http::HeaderValue::from_str(&format!("id=\"{refresh_id}\", retry=3")) {
                    response.headers_mut().insert("mastodon-async-refresh", value);
                }
            }
            response
        }
        Err(err) => status_error(err).into_response(),
    }
}

/// `GET /api/v1/statuses/{id}/history` — edit history as
/// `REST::StatusEditSerializer[]` (a synthesized snapshot when never edited).
pub async fn history(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let viewer = auth.0.token.user_id;
    let store: &dyn Store = &*state.context.store;
    let versions = match statuses::status_history(store, viewer, id, state.context.now_ms()) {
        Ok(versions) => versions,
        Err(err) => return status_error(err).into_response(),
    };
    let author_id = match store.status_by_id(id) {
        Ok(Some(status)) => status.account_id,
        _ => return ApiError::internal().into_response(),
    };
    let account = match store.account_by_id(author_id) {
        Ok(Some(account)) => account,
        _ => return ApiError::internal().into_response(),
    };
    let mut array = Vec::with_capacity(versions.len());
    for version in &versions {
        let media = version
            .media_ids
            .iter()
            .filter_map(|media_id| store.media_by_id(*media_id).ok().flatten())
            .collect::<Vec<_>>();
        array.push(dto::status_edit_json(
            &state.server,
            version,
            &account,
            &media,
            version.poll_options.clone(),
        ));
    }
    axum::Json(array).into_response()
}

/// `POST /api/v1/statuses` — create a status (200 with the new status).
///
/// Honors the `Idempotency-Key` header: a replay within one hour returns the
/// originally created status (still 200), matching Mastodon.
///
/// Thin adapter: auth + wire parse, then delegate to the use case (or
/// schedule when `scheduled_at` names a valid future time). All validation
/// lives in the use case; this function only preserves wire shapes.
pub async fn create(
    State(state): State<ApiState>,
    auth: Auth,
    headers: HeaderMap,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let parsed = match parse_create_command(&body, &headers, viewer, &auth) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let store: &dyn Store = &*state.context.store;
    let now_ms = state.context.now_ms();
    let Ok(scheduled_at) = crate::application::scheduled_statuses::parse_scheduled_at(
        parsed.scheduled_at.as_deref(),
        now_ms,
    ) else {
        return ApiError::unprocessable("Record invalid").into_response();
    };
    if let Some(scheduled_at_ms) = scheduled_at {
        return schedule_response(&state, store, parsed.cmd, parsed.place_id, scheduled_at_ms, now_ms);
    }
    match statuses::create_status(
        store,
        &state.server.public_url,
        state.context.now_ms(),
        parsed.cmd,
        state.server.max_status_chars,
        &state.server.license_policy,
        &state.server.place_policy,
        parsed.place_id,
        &state.server.alt_text_policy,
    ) {
        Ok(created) => created_response(&state, store, viewer, created),
        Err(err) => match err {
            // Reply/quote misses keep their exact custom messages.
            DomainError::NotFound(msg) if msg.contains("does not appear to exist") => {
                ApiError::new(axum::http::StatusCode::NOT_FOUND, msg).into_response()
            }
            other => status_error(other).into_response(),
        },
    }
}

/// Render a created status: stream the creation (unless an idempotent
/// replay) and pack the status JSON with the nag header when enabled.
fn created_response(
    state: &ApiState,
    store: &dyn Store,
    viewer: Id,
    created: CreateStatusResult,
) -> Response {
    // An idempotent replay committed nothing new, so nothing streams.
    if !created.replayed {
        state.context.streaming.publish(StreamEvent::StatusCreated {
            status_id: created.status.id,
        });
        crate::api::mastodon::endpoints::conversations::publish_conversation_events(
            &state,
            &created.status,
        );
    }
    // Core extension CORE-T: nag header on success when enabled.
    let attachments = alt_pairs(store, &created.status.media_ids);
    crate::api::mastodon::with_alt_text_nag(
        status_response(&state, Some(viewer), created.status, StatusSourceMode::Content),
        &state.server.alt_text_policy,
        &attachments,
    )
}

/// Parsed wire inputs for `POST /api/v1/statuses`: the use-case command plus
/// the two values the endpoint branches on before delegating.
struct ParsedCreate {
    cmd: CreateStatusCommand,
    place_id: Option<Id>,
    scheduled_at: Option<String>,
}

/// Parse the form body + `Idempotency-Key` header into a [`CreateStatusCommand`].
/// Every early return preserves its exact wire body: unparsable reply/quote
/// ids 404 with their custom messages, unparsable `place_id` 422s, poll
/// problems 422 via [`build_poll_command`].
fn parse_create_command(
    pairs: &[(String, String)],
    headers: &HeaderMap,
    viewer: Id,
    auth: &Auth,
) -> Result<ParsedCreate, Response> {
    let field = |key: &str| forms::get(pairs, key).map(str::to_string);

    // Reply/quote targets: a present-but-unparsable id behaves like an
    // unknown one (Mastodon `Status.find`).
    let in_reply_to_id = match field("in_reply_to_id") {
        None => None,
        Some(raw) => match raw.parse::<Id>() {
            Ok(id) => Some(id),
            Err(_) => {
                return Err(ApiError::new(
                    axum::http::StatusCode::NOT_FOUND,
                    "The post you are trying to reply to does not appear to exist.",
                )
                .into_response());
            }
        },
    };
    let quoted_status_id = match field("quoted_status_id") {
        None => None,
        Some(raw) => match raw.parse::<Id>() {
            Ok(id) => Some(id),
            Err(_) => {
                return Err(ApiError::new(
                    axum::http::StatusCode::NOT_FOUND,
                    "The post you are trying to quote does not appear to exist.",
                )
                .into_response());
            }
        },
    };

    // Nested `poll[...]` params.
    let poll = if pairs.iter().any(|(k, _)| k.starts_with("poll[")) {
        match build_poll_command(pairs) {
            Ok(poll) => Some(poll),
            Err(err) => return Err(err.into_response()),
        }
    } else {
        None
    };

    let media_ids = forms::all(pairs, "media_ids")
        .iter()
        .filter_map(|raw| raw.parse::<Id>().ok())
        .collect();

    // Core extension CORE-P: optional `place_id` (unknown ids take the
    // 422 path unless the policy is `off`, which ignores the param).
    // A present-but-unparsable id behaves like an unknown one.
    let place_id = match field("place_id") {
        None => None,
        Some(raw) => match raw.parse::<Id>() {
            Ok(id) => Some(id),
            Err(_) => {
                return Err(ApiError::unprocessable("Validation failed: Place not found")
                    .into_response());
            }
        },
    };

    let idempotency_key = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .filter(|key| !key.is_empty());

    Ok(ParsedCreate {
        cmd: CreateStatusCommand {
            account_id: viewer,
            text: field("status"),
            in_reply_to_id,
            quoted_status_id,
            quote_approval_policy: field("quote_approval_policy"),
            sensitive: forms::get(pairs, "sensitive").map(bool_cast),
            spoiler_text: field("spoiler_text"),
            visibility: field("visibility"),
            language: field("language"),
            media_ids,
            poll,
            application_name: Some(auth.0.app.name.clone()),
            application_website: auth.0.app.website.clone(),
            idempotency_key,
        },
        place_id,
        scheduled_at: field("scheduled_at"),
    })
}

/// The `scheduled_at` branch of `create`: schedule instead of posting (200 +
/// `ScheduledStatusSerializer`). Validation failures keep the exact 422
/// bodies (limits, media); `attachable_media` failure reads as empty (no nag
/// source needed on this path — unchanged).
fn schedule_response(
    state: &ApiState,
    store: &dyn Store,
    cmd: CreateStatusCommand,
    place_id: Option<Id>,
    scheduled_at_ms: u64,
    now_ms: u64,
) -> Response {
    match crate::application::scheduled_statuses::schedule_status(
        store,
        &state.server.public_url,
        now_ms,
        cmd,
        scheduled_at_ms,
        state.server.max_status_chars,
        &state.server.license_policy,
        &state.server.place_policy,
        place_id,
        &state.server.alt_text_policy,
    ) {
        Ok(row) => {
            let media = store
                .attachable_media(row.account_id, &row.media_ids, None)
                .unwrap_or_default();
            axum::Json(dto::scheduled_status_json(&state.server, &row, &media)).into_response()
        }
        // Validation failures keep the exact 422 bodies (limits, media).
        Err(DomainError::InvalidInput(message)) => {
            ApiError::unprocessable(message).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Build the nested poll command; validates the presence of `expires_in`.
fn build_poll_command(pairs: &[(String, String)]) -> ApiResult<PollCommand> {
    let multiple = forms::get(pairs, "poll[multiple]").is_some_and(bool_cast);
    let hide_totals = forms::get(pairs, "poll[hide_totals]").is_some_and(bool_cast);
    let options = forms::all(pairs, "poll[options]")
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let expires_in_s = match forms::get(pairs, "poll[expires_in]") {
        Some(raw) => match raw.parse::<u64>() {
            Ok(value) => value,
            Err(_) => {
                return Err(ApiError::unprocessable(
                    "Validation failed: Deadline can't be blank",
                ));
            }
        },
        None => {
            return Err(ApiError::unprocessable(
                "Validation failed: Deadline can't be blank",
            ));
        }
    };
    Ok(PollCommand {
        multiple,
        hide_totals,
        expires_in_s,
        options,
    })
}

/// `PUT/PATCH /api/v1/statuses/{id}` — edit (200, even with no changes).
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
    let Ok(status_id) = raw.parse::<Id>() else {
        // `find` on another (or unknown) id is an unrescued RecordNotFound.
        return ApiError::not_found().into_response();
    };
    let pairs = body;
    let field = |key: &str| forms::get(&pairs, key).map(str::to_string);
    let poll = if pairs.iter().any(|(k, _)| k.starts_with("poll[")) {
        match build_poll_command(&pairs) {
            Ok(poll) => Some(poll),
            Err(err) => return err.into_response(),
        }
    } else {
        None
    };
    let media_ids = forms::get(&pairs, "media_ids").map(|_| {
        forms::all(&pairs, "media_ids")
            .iter()
            .filter_map(|raw| raw.parse::<Id>().ok())
            .collect::<Vec<_>>()
    });

    // Core extension CORE-P: `place_id` present replaces (blank clears),
    // absent leaves unchanged. Present-but-unparsable is a 422 unless the
    // policy is `off` (ignored).
    let place_id = match field("place_id") {
        None => None,
        Some(raw) if raw.trim().is_empty() => Some(None),
        Some(raw) => match raw.trim().parse::<Id>() {
            Ok(id) => Some(Some(id)),
            Err(_) => {
                if state.server.place_policy == "off" {
                    None
                } else {
                    return ApiError::unprocessable("Validation failed: Place not found")
                        .into_response();
                }
            }
        },
    };

    let cmd = UpdateStatusCommand {
        account_id: viewer,
        status_id,
        text: field("status"),
        sensitive: forms::get(&pairs, "sensitive").map(bool_cast),
        spoiler_text: field("spoiler_text"),
        language: field("language"),
        media_ids,
        poll,
        quote_approval_policy: field("quote_approval_policy"),
    };
    let store: &dyn Store = &*state.context.store;
    match statuses::update_status(store, state.context.now_ms(), cmd, &state.server.license_policy, &state.server.place_policy, place_id, &state.server.alt_text_policy) {
        Ok(maybe_status) => {
            // No changes: reload and answer 200 with the unchanged status.
            let status = match maybe_status {
                Some(status) => {
                    state.context.streaming.publish(StreamEvent::StatusEdited {
                        status_id: status.id,
                    });
                    status
                }
                None => match store.status_by_id(status_id) {
                    Ok(Some(status)) => status,
                    _ => return ApiError::not_found().into_response(),
                },
            };
            // Core extension CORE-T: nag header on success when enabled.
            let attachments = alt_pairs(store, &status.media_ids);
            crate::api::mastodon::with_alt_text_nag(
                status_response(&state, Some(viewer), status, StatusSourceMode::Content),
                &state.server.alt_text_policy,
                &attachments,
            )
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/statuses/{id}` — tombstone; the 200 body is the
/// pre-deletion status in source mode (`text` instead of `content`).
pub async fn destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(status_id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match statuses::delete_status(store, viewer, status_id, state.context.now_ms()) {
        Ok(deleted) => {
            state.context.streaming.publish(StreamEvent::StatusDeleted {
                status: deleted.status.clone(),
                reblogs: deleted.reblogs,
            });
            status_response(&state, Some(viewer), deleted.status, StatusSourceMode::Source)
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}



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

/// `GET /api/v1/accounts/{id}/statuses` ΓÇö one account's statuses with
/// `pinned`/`exclude_replies`/`exclude_reblogs` filters and `Link` headers.
pub async fn account_statuses(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
    uri: Uri,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_SCOPES) {
        return err.into_response();
    }
    let viewer = viewer_of(&auth);
    let Ok(account_id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let page = match parse_page(&uri, 20, 40) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let query = AccountStatusesQuery {
        max_id: page.max_id,
        since_id: page.since_id,
        min_id: page.min_id,
        limit: page.limit,
        pinned_only: forms::get(&pairs, "pinned").is_some_and(bool_cast),
        exclude_replies: forms::get(&pairs, "exclude_replies").is_some_and(bool_cast),
        exclude_reblogs: forms::get(&pairs, "exclude_reblogs").is_some_and(bool_cast),
    };
    let store: &dyn Store = &*state.context.store;
    match statuses::account_statuses(store, viewer, account_id, query, state.context.now_ms()) {
        Ok(statuses) => {
            let next = (statuses.len() == page.limit)
                .then(|| statuses.last().map(|s| s.id))
                .flatten();
            let prev = statuses.first().map(|s| s.id);
            let mut response = status_list_response(&state, viewer, statuses);
            if let Some(link) = pagination::link_header(&uri, next, prev) {
                if let Ok(value) = header::HeaderValue::from_str(&link) {
                    response.headers_mut().insert(header::LINK, value);
                }
            }
            response
        }
        Err(err) => status_error(err).into_response(),
    }
}

/// `GET /api/v1/statuses/{id}/quotes` ΓÇö accepted quotes of a status as
/// statuses; the token is mandatory (401 when absent). Cursor = quoting
/// status id.
pub async fn quotes_index(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let Ok(status_id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let viewer = auth.0.token.user_id;
    let page = match parse_page(&uri, 20, 40) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match crate::application::quotes::quotes_of(
        store,
        viewer,
        status_id,
        page.max_id,
        page.limit,
        state.context.now_ms(),
    ) {
        Ok(entries) => {
            let next = (entries.len() == page.limit)
                .then(|| entries.last().map(|(quote, _)| quote.id))
                .flatten();
            let prev = entries.first().map(|(quote, _)| quote.id);
            let statuses: Vec<Status> = entries.into_iter().map(|(_, status)| status).collect();
            let mut response = status_list_response(&state, viewer, statuses);
            if let Some(link) = pagination::link_header(&uri, next, prev) {
                if let Ok(value) = header::HeaderValue::from_str(&link) {
                    response.headers_mut().insert(header::LINK, value);
                }
            }
            response
        }
        Err(err) => status_error(err).into_response(),
    }
}

/// `POST /api/v1/statuses/{id}/quotes/{quoting_status_id}/revoke` ΓÇö answers
/// 200 with the QUOTING status (its `quote.state` becomes `revoked`).
pub async fn quotes_revoke(
    State(state): State<ApiState>,
    auth: Auth,
    Path((raw, raw_quoting)): Path<(String, String)>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (Ok(status_id), Ok(quoting_id)) = (raw.parse::<Id>(), raw_quoting.parse::<Id>()) else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match crate::application::quotes::revoke_quote(
        store,
        viewer,
        status_id,
        quoting_id,
        state.context.now_ms(),
    ) {
        Ok(_) => match store.status_by_id(quoting_id) {
            Ok(Some(quoting)) => {
                status_response(&state, Some(viewer), quoting, StatusSourceMode::Content)
            }
            _ => ApiError::not_found().into_response(),
        },
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT/PATCH /api/v1/statuses/{id}/interaction_policy` ΓÇö set
/// `quote_approval_policy` on one's own status (200 with the status).
pub async fn interaction_policy(
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
    let Ok(status_id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let pairs = body;
    let Some(policy) = forms::get(&pairs, "quote_approval_policy").map(str::to_string) else {
        // No param: Mastodon still answers 200 with the unchanged status.
        return ApiError::unprocessable("param is missing or the value is empty: quote_approval_policy")
            .into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match crate::application::quotes::set_interaction_policy(
        store,
        viewer,
        status_id,
        &policy,
        state.context.now_ms(),
    ) {
        Ok(status) => status_response(&state, Some(viewer), status, StatusSourceMode::Content),
        Err(err) => ApiError::from(err).into_response(),
    }
}

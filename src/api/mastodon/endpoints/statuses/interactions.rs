//! Status interaction endpoints (S12 split of statuses.rs).
//!
//! Favourites, bookmarks, reblogs, pins and the favourited/reblogged-by
//! lists share one sub-route helper. Re-exported from super so all
//! pre-existing endpoints::statuses::X paths keep working.

use axum::extract::{Path, State};
use axum::http::{Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::{WRITE_SCOPES, optional_scopes, parse_page, status_error, status_response, viewer_of};
use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto::{self, StatusSourceMode};
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::application::ports::Store;
use crate::application::statuses;
use crate::application::streaming::StreamEvent;
use crate::domain::{AccountStats, Id, Status};

/// Favourite scopes.
const FAVOURITE_SCOPES: &[&str] = &["write", "write:favourites"];
/// Bookmark scopes.
const BOOKMARK_SCOPES: &[&str] = &["write", "write:bookmarks"];
/// Pin scopes.
const PIN_SCOPES: &[&str] = &["write", "write:accounts"];
/// Account-list read scopes (favourited_by / reblogged_by).
const READ_ACCOUNT_SCOPES: &[&str] = &["read", "read:accounts"];

/// `POST /api/v1/statuses/{id}/favourite` (200 with the status).
pub async fn favourite(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    status_interaction(
        &state,
        &auth,
        raw,
        FAVOURITE_SCOPES,
        |store, viewer, id, now| {
            crate::application::interactions::favourite(store, viewer, id, now)
        },
    )
    .await
}

/// `POST /api/v1/statuses/{id}/unfavourite` (200; no-op when not favourited).
pub async fn unfavourite(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    status_interaction(
        &state,
        &auth,
        raw,
        FAVOURITE_SCOPES,
        |store, viewer, id, now| {
            crate::application::interactions::unfavourite(store, viewer, id, now)
        },
    )
    .await
}

/// `POST /api/v1/statuses/{id}/bookmark`.
pub async fn bookmark(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    status_interaction(
        &state,
        &auth,
        raw,
        BOOKMARK_SCOPES,
        |store, viewer, id, now| crate::application::interactions::bookmark(store, viewer, id, now),
    )
    .await
}

/// `POST /api/v1/statuses/{id}/unbookmark`.
pub async fn unbookmark(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    status_interaction(
        &state,
        &auth,
        raw,
        BOOKMARK_SCOPES,
        |store, viewer, id, now| {
            crate::application::interactions::unbookmark(store, viewer, id, now)
        },
    )
    .await
}

/// `POST /api/v1/statuses/{id}/reblog` ΓÇö answers 200 with the NEW reblog.
pub async fn reblog(
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
    let visibility = forms::get(&pairs, "visibility").map(str::to_string);
    let store: &dyn Store = &*state.context.store;
    // A repeat reblog returns the existing wrapper and creates nothing, so only
    // a fresh wrapper streams (upstream's `ReblogService` returns early too).
    let target = match store.status_by_id(status_id) {
        Ok(Some(status)) => status.reblog_of_id.unwrap_or(status_id),
        _ => status_id,
    };
    let existed = store.own_reblog(viewer, target).ok().flatten().is_some();
    match crate::application::interactions::reblog(
        store,
        viewer,
        status_id,
        visibility.as_deref(),
        state.context.now_ms(),
    ) {
        Ok(status) => {
            if !existed {
                // The wrapper is what followers' home streams carry.
                state.context.streaming.publish(StreamEvent::StatusCreated {
                    status_id: status.id,
                });
            }
            status_response(&state, Some(viewer), status, StatusSourceMode::Content)
        }
        Err(err) => status_error(err).into_response(),
    }
}

/// `POST /api/v1/statuses/{id}/unreblog` ΓÇö answers 200 with the ORIGINAL.
///
/// The viewer's wrapper is read before the unreblog, so its `delete` can be
/// published after the commit.
pub async fn unreblog(
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
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let store: &dyn Store = &*state.context.store;
    // The unreblog takes the original's id; a wrapper id resolves to its target.
    let target = match store.status_by_id(status_id) {
        Ok(Some(status)) => status.reblog_of_id.unwrap_or(status_id),
        _ => status_id,
    };
    let wrapper = store.own_reblog(viewer, target).ok().flatten();
    match crate::application::interactions::unreblog(
        store,
        viewer,
        status_id,
        state.context.now_ms(),
    ) {
        Ok(status) => {
            if let Some(wrapper) = wrapper {
                state.context.streaming.publish(StreamEvent::StatusDeleted {
                    status: wrapper,
                    reblogs: Vec::new(),
                });
            }
            status_response(&state, Some(viewer), status, StatusSourceMode::Content)
        }
        Err(err) => status_error(err).into_response(),
    }
}

/// `POST /api/v1/statuses/{id}/pin`.
pub async fn pin(State(state): State<ApiState>, auth: Auth, Path(raw): Path<String>) -> Response {
    status_interaction(&state, &auth, raw, PIN_SCOPES, |store, viewer, id, now| {
        crate::application::interactions::pin(store, viewer, id, now)
    })
    .await
}

/// `POST /api/v1/statuses/{id}/unpin` (idempotent no-op).
pub async fn unpin(State(state): State<ApiState>, auth: Auth, Path(raw): Path<String>) -> Response {
    status_interaction(&state, &auth, raw, PIN_SCOPES, |store, viewer, id, now| {
        crate::application::interactions::unpin(store, viewer, id, now)
    })
    .await
}

/// Shared shape for the status sub-route interactions.
async fn status_interaction(
    state: &ApiState,
    auth: &Auth,
    raw: String,
    scopes: &'static [&'static str],
    action: fn(&dyn Store, Id, Id, u64) -> crate::domain::Result<Status>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(scopes) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(status_id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match action(store, viewer, status_id, state.context.now_ms()) {
        Ok(status) => status_response(state, Some(viewer), status, StatusSourceMode::Content),
        Err(err) => status_error(err).into_response(),
    }
}

/// `GET /api/v1/statuses/{id}/favourited_by` ΓÇö accounts, paginated by
/// favourite id (cursor hidden in `Link`).
pub async fn favourited_by(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
    uri: Uri,
) -> Response {
    interacted_by(&state, &auth, raw, uri, ByWhich::Favourited).await
}

/// `GET /api/v1/statuses/{id}/reblogged_by` ΓÇö accounts, paginated by reblog id.
pub async fn reblogged_by(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
    uri: Uri,
) -> Response {
    interacted_by(&state, &auth, raw, uri, ByWhich::Reblogged).await
}

#[derive(Clone, Copy)]
enum ByWhich {
    Favourited,
    Reblogged,
}

async fn interacted_by(
    state: &ApiState,
    auth: &OptionalAuth,
    raw: String,
    uri: Uri,
    which: ByWhich,
) -> Response {
    if let Err(err) = optional_scopes(auth, READ_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = viewer_of(auth);
    let Ok(status_id) = raw.parse::<Id>() else {
        return ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let page = match parse_page(&uri, 40, 80) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    // The show gate applies first (404 for invisible statuses).
    if let Err(err) = statuses::show_status(store, viewer, status_id, state.context.now_ms()) {
        return status_error(err).into_response();
    }
    let pairs = match which {
        ByWhich::Favourited => store.favourited_by(status_id, page.max_id, page.limit),
        // D52 distributable-visibility scope (gaps G-C): only public/unlisted
        // wrappers attribute reblogs; private/direct wrappers never leak
        // through this read.
        ByWhich::Reblogged => store.reblogged_by_distributable(status_id, page.max_id, page.limit),
    };
    let pairs = match pairs {
        Ok(pairs) => pairs,
        Err(err) => return ApiError::from(err).into_response(),
    };
    // Upstream `load_accounts`
    // (`favourited_by_accounts_controller.rb` /
    // `reblogged_by_accounts_controller.rb` at pin `8ff11c2b`):
    // `without_suspended` always applies, and an authenticated viewer never
    // sees accounts they block/mute or are blocked by
    // (`not_excluded_by_account`, i.e. upstream's blocks + blocked-bys +
    // mutes ΓÇö domain blocks are account-scoped here, not domain-scoped).
    // Anonymous viewers get every resolvable non-suspended account.
    let excluded: std::collections::HashSet<Id> = match viewer {
        Some(viewer) => {
            let ids: Vec<Id> = pairs.iter().map(|(account_id, _)| *account_id).collect();
            match store.batch_pair_states(viewer, &ids, state.context.now_ms()) {
                // `(incoming, outgoing)`: `incoming.blocking` = the account
                // blocks the viewer; `outgoing.blocking`/`muting` = the
                // viewer blocks/mutes the account.
                Ok(states) => states
                    .into_iter()
                    .filter_map(|(id, (incoming, outgoing))| {
                        (outgoing.blocking || outgoing.muting || incoming.blocking).then_some(id)
                    })
                    .collect(),
                Err(err) => return ApiError::from(err).into_response(),
            }
        }
        None => std::collections::HashSet::new(),
    };
    let mut accounts = Vec::with_capacity(pairs.len());
    let mut kept: Vec<(Id, Id)> = Vec::with_capacity(pairs.len());
    for (account_id, cursor) in &pairs {
        if excluded.contains(account_id) {
            continue;
        }
        match store.account_by_id(*account_id) {
            Ok(Some(account)) if !account.suspended => {
                kept.push((*account_id, *cursor));
                accounts.push(account);
            }
            Ok(_) => continue,
            Err(err) => return ApiError::from(err).into_response(),
        }
    }
    // Cursors are interaction ids (favourite id / reblog status id), derived
    // from the filtered page like upstream's `pagination_max_id`.
    let next = (pairs.len() == page.limit)
        .then(|| kept.last().map(|(_, cursor)| *cursor))
        .flatten();
    let prev = kept.first().map(|(_, cursor)| *cursor);
    let mut response = axum::Json(
        accounts
            .iter()
            .map(|account| {
                let stats: Option<AccountStats> = store.account_stats(account.id).ok();
                dto::account_json_with_stats(&state.server, account, stats.as_ref())
            })
            .collect::<Vec<Value>>(),
    )
    .into_response();
    if let Some(link) = pagination::link_header(&uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

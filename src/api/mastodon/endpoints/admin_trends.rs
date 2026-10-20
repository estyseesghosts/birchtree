//! Admin trend approvals (Phase 7.3): review queues over the same
//! candidates the public reads derive, plus approve/reject writes.
//!
//! Gate: admins only (`AdminArea::AdminOnly`), role before scope — a
//! role-less user gets `403 {"error":"Forbidden"}`, a scoped-out admin gets
//! `403 {"error":"This action is outside the authorized scopes"}`. Each
//! approve/reject commits the flag and one audit-trail record atomically.
//!
//! Index paging mirrors the public controllers (`offset` + `limit`),
//! except the publishers index, which pages by numeric cursor like
//! upstream's preview-card-provider list.

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};

use crate::api::forms;
use crate::api::mastodon::auth::{AdminArea, Auth};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::{pagination, status_view, ApiState};
use crate::application::ports::Store;
use crate::application::trends;
use crate::domain::Id;

const ADMIN_READ: &[&str] = &["admin:read"];
const ADMIN_WRITE: &[&str] = &["admin:write"];

fn offset_limit(uri: &Uri, default: usize) -> (usize, usize) {
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    trends::parse_offset_limit(
        forms::get(&pairs, "offset"),
        forms::get(&pairs, "limit"),
        default,
    )
}

/// `GET /api/v1/admin/trends/tags`: every tag candidate with its review
/// state (pending rows materialize on read so each has an `:id`).
pub async fn tags_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    let viewer = match auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_READ,
    ) {
        Ok(_) => match auth.0.token.user_id {
            Some(id) => id,
            None => return ApiError::requires_user().into_response(),
        },
        Err(err) => return err.into_response(),
    };
    let (offset, limit) = offset_limit(&uri, 10);
    let store: &dyn Store = &*state.context.store;
    let candidates = match trends::tag_candidates(store, state.context.now_ms()) {
        Ok(candidates) => candidates,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let page = trends::page(candidates, offset, limit);
    let full = page.len() == limit && limit > 0;
    let now = state.context.now_ms();
    let body: Vec<serde_json::Value> = page
        .iter()
        .map(|candidate| {
            let flag = candidate.flag.as_ref().cloned().unwrap_or(crate::domain::TrendTagFlag {
                id: Id::new(0),
                name: candidate.name.clone(),
                allowed: None,
                reviewed_at_ms: None,
                created_at_ms: 0,
            });
            let history = trends::tag_history(store, &flag.name, now).unwrap_or_default();
            let (following, featuring) =
                crate::application::web::tag_relationship(store, viewer, &flag.name)
                    .unwrap_or((false, false));
            dto::admin_trend_tag_json(&state.server, &flag, Some(viewer), &history, following, featuring)
        })
        .collect();
    crate::api::mastodon::endpoints::trends::paged_admin(&uri, body, offset, limit, full)
}

/// `POST /api/v1/admin/trends/tags/:id/approve`.
pub async fn tags_approve(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_tag(&state, &auth, &raw, true)
}

/// `POST /api/v1/admin/trends/tags/:id/reject`.
pub async fn tags_reject(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_tag(&state, &auth, &raw, false)
}

fn review_tag(state: &ApiState, auth: &Auth, raw: &str, allowed: bool) -> Response {
    let viewer = match auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_WRITE,
    ) {
        Ok(_) => match auth.0.token.user_id {
            Some(id) => id,
            None => return ApiError::requires_user().into_response(),
        },
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.review_trend_tag(id, allowed, viewer, state.context.now_ms()) {
        Ok(flag) => {
            let history = trends::tag_history(store, &flag.name, state.context.now_ms())
                .unwrap_or_default();
            let (following, featuring) =
                crate::application::web::tag_relationship(store, viewer, &flag.name)
                    .unwrap_or((false, false));
            axum::Json(dto::admin_trend_tag_json(
                &state.server,
                &flag,
                Some(viewer),
                &history,
                following,
                featuring,
            ))
            .into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/admin/trends/links`: every link candidate (derived from
/// recent public statuses) with its review state.
pub async fn links_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_READ,
    ) {
        return err.into_response();
    }
    let (offset, limit) = offset_limit(&uri, 10);
    let store: &dyn Store = &*state.context.store;
    let candidates = match trends::link_candidates(store, state.context.now_ms()) {
        Ok(candidates) => candidates,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let page = trends::page(candidates, offset, limit);
    let full = page.len() == limit && limit > 0;
    let body: Vec<serde_json::Value> =
        page.iter().map(dto::admin_trend_link_json).collect();
    crate::api::mastodon::endpoints::trends::paged_admin(&uri, body, offset, limit, full)
}

/// `POST /api/v1/admin/trends/links/:id/approve`.
pub async fn links_approve(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_link(&state, &auth, &raw, true)
}

/// `POST /api/v1/admin/trends/links/:id/reject`.
pub async fn links_reject(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_link(&state, &auth, &raw, false)
}

fn review_link(state: &ApiState, auth: &Auth, raw: &str, allowed: bool) -> Response {
    if let Err(err) = auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_WRITE,
    ) {
        return err.into_response();
    }
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    let viewer = auth.0.token.user_id.unwrap_or(Id::new(0));
    match store.review_trend_link(id, allowed, viewer, state.context.now_ms()) {
        Ok(link) => axum::Json(dto::admin_trend_link_json(&link)).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/admin/trends/links/publishers`: every seen publisher host,
/// cursor-paged like upstream (default 100, max 200).
pub async fn publishers_index(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_READ,
    ) {
        return err.into_response();
    }
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let page = match pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        100,
        200,
    ) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    // Materialize publisher rows for any link hosts first (same derivation
    // as the links index), then page the rows newest-first.
    if let Err(err) = trends::link_candidates(store, state.context.now_ms()) {
        return ApiError::from(err).into_response();
    }
    let publishers = match store.all_trend_publishers() {
        Ok(publishers) => publishers,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let filtered: Vec<_> = publishers
        .into_iter()
        .filter(|p| {
            if let Some(max_id) = page.max_id
                && p.id >= max_id
            {
                return false;
            }
            if let Some(since_id) = page.since_id
                && p.id <= since_id
            {
                return false;
            }
            if let Some(min_id) = page.min_id
                && p.id <= min_id
            {
                return false;
            }
            true
        })
        .take(page.limit)
        .collect();
    let full = filtered.len() == page.limit;
    let next = if full { filtered.last().map(|p| p.id) } else { None };
    let prev = filtered.first().map(|p| p.id);
    let body: Vec<serde_json::Value> = filtered
        .iter()
        .map(dto::admin_trend_publisher_json)
        .collect();
    let mut response = axum::Json(body).into_response();
    if let Some(link) = pagination::link_header_desc(&uri, next, prev)
        && let Ok(value) = link.parse::<axum::http::HeaderValue>()
    {
        response
            .headers_mut()
            .insert(axum::http::header::LINK, value);
    }
    response
}

/// `POST /api/v1/admin/trends/links/publishers/:id/approve`.
pub async fn publishers_approve(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_publisher(&state, &auth, &raw, true)
}

/// `POST /api/v1/admin/trends/links/publishers/:id/reject`.
pub async fn publishers_reject(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_publisher(&state, &auth, &raw, false)
}

fn review_publisher(state: &ApiState, auth: &Auth, raw: &str, allowed: bool) -> Response {
    if let Err(err) = auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_WRITE,
    ) {
        return err.into_response();
    }
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    let viewer = auth.0.token.user_id.unwrap_or(Id::new(0));
    match store.review_trend_publisher(id, allowed, viewer, state.context.now_ms()) {
        Ok(publisher) => {
            axum::Json(dto::admin_trend_publisher_json(&publisher)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/admin/trends/statuses`: every status candidate with its
/// review state.
pub async fn statuses_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    let viewer = match auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_READ,
    ) {
        Ok(_) => match auth.0.token.user_id {
            Some(id) => id,
            None => return ApiError::requires_user().into_response(),
        },
        Err(err) => return err.into_response(),
    };
    let (offset, limit) = offset_limit(&uri, 20);
    let store: &dyn Store = &*state.context.store;
    let candidates = match trends::status_candidates(store, state.context.now_ms()) {
        Ok(candidates) => candidates,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let page = trends::page(candidates, offset, limit);
    let full = page.len() == limit && limit > 0;
    let statuses: Vec<crate::domain::Status> =
        page.iter().map(|candidate| candidate.status.clone()).collect();
    let reviews: Vec<bool> = page
        .iter()
        .map(|candidate| candidate.allowed.is_none())
        .collect();
    let views = match status_view::load_status_views(
        &state.server,
        store,
        Some(viewer),
        statuses,
        state.context.now_ms(),
    ) {
        Ok(views) => views,
        Err(err) => return err.into_response(),
    };
    let body: Vec<serde_json::Value> = views
        .into_iter()
        .zip(reviews)
        .map(|(view, requires_review)| {
            let mut value = dto::status_json(
                &state.server,
                Some(viewer),
                state.context.now_ms(),
                &view,
                dto::StatusSourceMode::Content,
            );
            if let serde_json::Value::Object(ref mut map) = value {
                map.insert("requires_review".into(), serde_json::json!(requires_review));
            }
            value
        })
        .collect();
    crate::api::mastodon::endpoints::trends::paged_admin(&uri, body, offset, limit, full)
}

/// `POST /api/v1/admin/trends/statuses/:id/approve` (`:id` is the status id).
pub async fn statuses_approve(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_status(&state, &auth, &raw, true)
}

/// `POST /api/v1/admin/trends/statuses/:id/reject`.
pub async fn statuses_reject(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    review_status(&state, &auth, &raw, false)
}

fn review_status(state: &ApiState, auth: &Auth, raw: &str, allowed: bool) -> Response {
    let viewer = match auth.0.require_admin(
        &*state.context.store,
        AdminArea::AdminOnly,
        ADMIN_WRITE,
    ) {
        Ok(_) => match auth.0.token.user_id {
            Some(id) => id,
            None => return ApiError::requires_user().into_response(),
        },
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.review_trend_status(id, allowed, viewer, state.context.now_ms()) {
        Ok(flag) => {
            let Some(status) = (match store.status_by_id(flag.status_id) {
                Ok(status) => status,
                Err(err) => return ApiError::from(err).into_response(),
            }) else {
                return ApiError::not_found().into_response();
            };
            let view = match status_view::load_status_view(
                &state.server,
                store,
                Some(viewer),
                status,
                state.context.now_ms(),
            ) {
                Ok(view) => view,
                Err(err) => return err.into_response(),
            };
            let mut value = dto::status_json(
                &state.server,
                Some(viewer),
                state.context.now_ms(),
                &view,
                dto::StatusSourceMode::Content,
            );
            if let serde_json::Value::Object(ref mut map) = value {
                map.insert("requires_review".into(), serde_json::json!(false));
            }
            axum::Json(value).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

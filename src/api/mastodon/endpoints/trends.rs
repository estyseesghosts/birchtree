//! Public trend reads (Phase 7.3).
//!
//! Only admin-approved items are served; unreviewed and rejected rows never
//! appear here, and nothing is served when trends are disabled (upstream
//! `Setting.trends`; here `Config::trends_enabled`, on by default).
//! Authentication is optional (`authorize_if_got_token!`: a present token
//! must be valid, anonymous reads are fine) and no scope is required.
//! Paging is `offset` + `limit` (default 10), not cursors.

use axum::extract::State;
use axum::http::Uri;
use axum::response::{IntoResponse, Response};

use crate::api::forms;
use crate::api::mastodon::auth::OptionalAuth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::{status_view, ApiState};
use crate::application::ports::Store;
use crate::application::trends;
use crate::application::web as web_cases;

/// `Deprecation` for the deprecated `/api/v1/trends` alias of `/tags`:
/// upstream deprecated it 2022-03-30, rendered as `@<unix timestamp>` with
/// no `Sunset` header.
pub const TRENDS_DEPRECATION: &str = "@1648612800";

/// `GET /api/v1/trends/tags`.
pub async fn tags_index(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    tags_page(&state, &auth, &uri, false)
}

/// `GET /api/v1/trends`: deprecated alias of `/api/v1/trends/tags`, with
/// the upstream `Deprecation` header value.
pub async fn trends_alias(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    tags_page(&state, &auth, &uri, true)
}

fn tags_page(state: &ApiState, auth: &OptionalAuth, uri: &Uri, deprecated: bool) -> Response {
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let (offset, limit) = offset_limit(uri, 10);
    let now = state.context.now_ms();
    let store: &dyn Store = &*state.context.store;
    let names = if !state.server.trends_enabled {
        Vec::new()
    } else {
        match trends::approved_tags(store, now) {
            Ok(names) => names,
            Err(err) => return ApiError::from(err).into_response(),
        }
    };
    let page = trends::page(names, offset, limit);
    let full = page.len() == limit && limit > 0;
    let body: Vec<serde_json::Value> = page
        .iter()
        .map(|flag| {
            let history = trends::tag_history(store, &flag.name, now).unwrap_or_default();
            let (following, featuring) = viewer
                .map(|viewer| {
                    web_cases::tag_relationship(store, viewer, &flag.name).unwrap_or((false, false))
                })
                .unwrap_or((false, false));
            dto::trend_tag_json(&state.server, flag, viewer, &history, following, featuring)
        })
        .collect();
    paged_json(uri, body, offset, limit, full, deprecated)
}

/// `GET /api/v1/trends/links` (approved preview cards only).
pub async fn links_index(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let _ = viewer;
    let (offset, limit) = offset_limit(&uri, 10);
    let store: &dyn Store = &*state.context.store;
    let links = if !state.server.trends_enabled {
        Vec::new()
    } else {
        match trends::approved_links(store, state.context.now_ms()) {
            Ok(links) => links,
            Err(err) => return ApiError::from(err).into_response(),
        }
    };
    let page = trends::page(links, offset, limit);
    let full = page.len() == limit && limit > 0;
    let body: Vec<serde_json::Value> =
        page.iter().map(dto::trend_link_json).collect();
    paged_json(&uri, body, offset, limit, full, false)
}

/// `GET /api/v1/trends/statuses` (approved statuses only).
///
/// `?locale=` is accepted and ordering-only: statuses stored in that
/// language sort first, stably; every approved status is still listed
/// (real per-user locale ranking needs the `chosen_languages` setting,
/// gap D21).
pub async fn statuses_index(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    uri: Uri,
) -> Response {
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let (offset, limit) = trends::parse_offset_limit(
        forms::get(&pairs, "offset"),
        forms::get(&pairs, "limit"),
        20,
    );
    let locale = forms::get(&pairs, "locale").map(str::to_string);
    let store: &dyn Store = &*state.context.store;
    let statuses = if !state.server.trends_enabled {
        Vec::new()
    } else {
        match trends::approved_statuses(store, viewer, state.context.now_ms()) {
            Ok(statuses) => statuses,
            Err(err) => return ApiError::from(err).into_response(),
        }
    };
    let page = trends::page(trends::locale_first(statuses, locale.as_deref()), offset, limit);
    let full = page.len() == limit && limit > 0;
    let views = match status_view::load_status_views(
        &state.server,
        store,
        viewer,
        page,
        state.context.now_ms(),
    ) {
        Ok(views) => views,
        Err(err) => return err.into_response(),
    };
    let body: Vec<serde_json::Value> = views
        .iter()
        .map(|view| {
            dto::status_json(
                &state.server,
                viewer,
                state.context.now_ms(),
                view,
                dto::StatusSourceMode::Content,
            )
        })
        .collect();
    paged_json(&uri, body, offset, limit, full, false)
}

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

/// Render a page as JSON with upstream's offset `Link` header (`next` when
/// the page is full, `prev` when `offset > limit`) and the `Deprecation`
/// header for the deprecated alias.
fn paged_json(
    uri: &Uri,
    body: Vec<serde_json::Value>,
    offset: usize,
    limit: usize,
    full: bool,
    deprecated: bool,
) -> Response {
    let mut response = axum::Json(body).into_response();
    let mut links = Vec::new();
    if full {
        links.push(format!("<{}>; rel=\"next\"", with_offset(uri, offset + limit)));
    }
    if offset > limit && limit > 0 {
        links.push(format!(
            "<{}>; rel=\"prev\"",
            with_offset(uri, offset.saturating_sub(limit))
        ));
    }
    if !links.is_empty()
        && let Ok(value) = links.join(", ").parse::<axum::http::HeaderValue>()
    {
        response
            .headers_mut()
            .insert(axum::http::header::LINK, value);
    }
    if deprecated {
        response.headers_mut().insert(
            "Deprecation",
            axum::http::HeaderValue::from_static(TRENDS_DEPRECATION),
        );
    }
    response
}

/// `uri` with the `offset` query parameter replaced (other parameters are
/// preserved, matching `pagination_params` upstream).
fn with_offset(uri: &Uri, offset: usize) -> String {
    let path = uri.path();
    let mut kept: Vec<(String, String)> = uri
        .query()
        .map(|query| {
            query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .filter_map(|pair| {
                    let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                    if key == "offset" {
                        None
                    } else {
                        Some((key.to_string(), value.to_string()))
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    kept.push(("offset".to_string(), offset.to_string()));
    let query = kept
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    format!("{path}?{query}")
}

/// Offset-paged JSON for the admin review queues (same `Link` shape as the
/// public reads, without the `Deprecation` header).
pub fn paged_admin(
    uri: &Uri,
    body: Vec<serde_json::Value>,
    offset: usize,
    limit: usize,
    full: bool,
) -> Response {
    paged_json(uri, body, offset, limit, full, false)
}

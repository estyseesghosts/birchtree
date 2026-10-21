//! Stray Phase 7 rows: the link timeline and app credential check.
//!
//! - `GET /api/v1/timelines/link?url=…`: public statuses containing an
//!   approved trend-link URL (pinned `LinkController`; the `url` must match
//!   an `allowed` trend link row exactly, else 404).
//! - `GET /api/v1/apps/verify_credentials`: the caller's application
//!   without the client secret (pinned `CredentialsController`; any valid
//!   token, user or app-only, no scope check).

use axum::extract::State;
use axum::http::{Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::forms;
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto::{self, StatusSourceMode};
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::api::mastodon::status_view::load_status_views;
use crate::api::mastodon::ApiState;
use crate::application::filters::filter_results_unscoped;
use crate::application::ports::{Store, TimelineQuery};
use crate::application::timelines;
use crate::application::web as web_cases;
use crate::domain::Id;

const READ_STATUSES: &[&str] = &["read", "read:statuses"];

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

fn viewer_of(auth: &OptionalAuth) -> Option<Id> {
    auth.0.as_ref().and_then(|a| a.token.user_id)
}

/// `GET /api/v1/timelines/link?url=…`.
pub async fn link(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    if let Some(authenticated) = &auth.0 {
        if let Err(err) = authenticated.require_scopes(READ_STATUSES) {
            return err.into_response();
        }
    }
    let pairs = forms::parse(uri.query().unwrap_or("").as_bytes());
    let url = forms::get(&pairs, "url").unwrap_or("").to_string();
    let limit = match forms::get(&pairs, "limit") {
        None => timelines::DEFAULT_LIMIT,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            (value.unsigned_abs() as usize).min(timelines::MAX_LIMIT)
        }
    };
    let cursors = match pagination::parse(
        None,
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        limit,
        timelines::MAX_LIMIT,
    ) {
        Ok(cursors) => cursors,
        Err(err) => return err.into_response(),
    };
    let flag = |key: &str| forms::get(&pairs, key).is_some_and(bool_cast);
    let mut query = TimelineQuery {
        max_id: cursors.max_id,
        since_id: cursors.since_id,
        min_id: cursors.min_id,
        limit,
        only_media: flag("only_media"),
        local_only: flag("local"),
        remote_only: flag("remote"),
    };
    // Aperture (Stage 2): flag on forces photo-only timelines regardless of
    // the client's `only_media`. No cursor/`Link` change.
    query.only_media |= state.server.aperture_mode;
    let viewer = viewer_of(&auth);
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    // Topic feed gate (#21d; upstream `TopicController#require_auth?`).
    let modes: Vec<&str> = if query.local_only {
        vec![state.server.local_topic_feed_access.as_str()]
    } else if query.remote_only {
        vec![state.server.remote_topic_feed_access.as_str()]
    } else {
        vec![
            state.server.local_topic_feed_access.as_str(),
            state.server.remote_topic_feed_access.as_str(),
        ]
    };
    match super::timelines::feed_gate(&state, viewer, &modes, &uri) {
        Ok(Some(refusal)) => return refusal,
        Ok(None) => {}
        Err(err) => return err.into_response(),
    };
    match web_cases::link_timeline(store, viewer, &url, query, now) {
        Ok(page) => {
            let first = page.statuses.first().map(|status| status.id);
            let last = page.statuses.last().map(|status| status.id);
            match render(&state, viewer, page.statuses) {
                Ok(body) => {
                    let mut response = axum::Json(body).into_response();
                    let cache = if viewer.is_some() {
                        "private, no-store"
                    } else {
                        "max-age=15, public, stale-while-revalidate=30, stale-if-error=86400"
                    };
                    if let Ok(value) = header::HeaderValue::from_str(cache) {
                        response.headers_mut().insert(header::CACHE_CONTROL, value);
                    }
                    let kept: Vec<&str> = uri
                        .query()
                        .unwrap_or("")
                        .split('&')
                        .filter(|pair| {
                            let key = pair.split('=').next().unwrap_or("");
                            !pair.is_empty() && !matches!(key, "max_id" | "since_id" | "min_id")
                        })
                        .collect();
                    let base_text = if kept.is_empty() {
                        uri.path().to_string()
                    } else {
                        format!("{}?{}", uri.path(), kept.join("&"))
                    };
                    let base: Uri = base_text.parse().unwrap_or_else(|_| uri.clone());
                    if let Some(link) = pagination::link_header(&base, last, first) {
                        if let Ok(value) = header::HeaderValue::from_str(&link) {
                            response.headers_mut().insert(header::LINK, value);
                        }
                    }
                    response
                }
                Err(err) => err.into_response(),
            }
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

fn render(
    state: &ApiState,
    viewer: Option<Id>,
    statuses: Vec<crate::domain::Status>,
) -> Result<Vec<Value>, ApiError> {
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let mut views = load_status_views(&state.server, store, viewer, statuses, now)?;
    if let Some(viewer) = viewer {
        let page: Vec<crate::domain::Status> =
            views.iter().map(|view| view.status.clone()).collect();
        let matches = filter_results_unscoped(store, viewer, &page, now)?;
        for view in &mut views {
            if let Some(entries) = matches.get(&view.status.id) {
                view.filtered = entries.clone();
            }
        }
    }
    Ok(views
        .iter()
        .map(|view| {
            dto::status_json(
                &state.server,
                viewer,
                now,
                view,
                StatusSourceMode::Content,
            )
        })
        .collect())
}

/// `GET /api/v1/apps/verify_credentials`: the caller's application.
///
/// Any valid token works (user or app-only) and no scope is required —
/// the pinned controller only checks the token, and never returns the
/// client secret.
pub async fn verify_app_credentials(State(_state): State<ApiState>, auth: Auth) -> Response {
    axum::Json(dto::app_json(&auth.0.app, None)).into_response()
}

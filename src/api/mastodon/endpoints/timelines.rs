//! Timeline endpoints (Phase 4): home, public, tag and list.
//!
//! Policy lives in `application::timelines`; these handlers parse the
//! Mastodon query contract, render the page and set the cache and `Link`
//! headers.

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Uri};
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
use crate::application::policy::HomeMentionPolicy;
use crate::application::ports::{Store, TimelinePage, TimelineQuery};
use crate::application::timelines;
use crate::domain::{Error as DomainError, Id, Status};

/// Read scopes for status timelines (home, public, tag).
const READ_STATUSES: &[&str] = &["read", "read:statuses"];
/// Read scopes for the list timeline.
const READ_LISTS: &[&str] = &["read", "read:lists"];

/// Anonymous public and tag responses may be shared-cached.
const PUBLIC_CACHE: &str = "max-age=15, public, stale-while-revalidate=30, stale-if-error=86400";
/// Personalized responses (any authenticated viewer, home, list) never cache.
const PRIVATE_CACHE: &str = "private, no-store";

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Scope check for optional-auth reads (anonymous passes).
fn optional_scopes(auth: &OptionalAuth, required: &[&str]) -> Result<(), ApiError> {
    match &auth.0 {
        Some(authenticated) => authenticated.require_scopes(required),
        None => Ok(()),
    }
}

/// The viewer id carried by an optional token (app-only tokens have none).
fn viewer_of(auth: &OptionalAuth) -> Option<Id> {
    auth.0.as_ref().and_then(|a| a.token.user_id)
}

/// Parse the store query from the request URI.
///
/// `limit` follows Mastodon's `limit_param`: absent -> 20, else
/// `min(abs(to_i), 40)`, so `limit=0` and non-numeric values give an empty
/// page. Cursors are numeric and exclusive; anything else is a 400.
fn timeline_query(uri: &Uri) -> Result<TimelineQuery, ApiError> {
    let pairs = forms::parse(uri.query().unwrap_or("").as_bytes());
    let limit = match forms::get(&pairs, "limit") {
        None => timelines::DEFAULT_LIMIT,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            (value.unsigned_abs() as usize).min(timelines::MAX_LIMIT)
        }
    };
    let cursors = pagination::parse(
        None,
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        limit,
        timelines::MAX_LIMIT,
    )?;
    let flag = |key: &str| forms::get(&pairs, key).is_some_and(bool_cast);
    Ok(TimelineQuery {
        max_id: cursors.max_id,
        since_id: cursors.since_id,
        min_id: cursors.min_id,
        limit,
        only_media: flag("only_media"),
        local_only: flag("local"),
        remote_only: flag("remote"),
    })
}

/// The request URI with its cursors removed: the base for `Link` next/prev.
///
/// Documented deviation: Mastodon echoes only its permitted parameters; we
/// echo the rest of the query.
fn link_base(uri: &Uri) -> Uri {
    let kept: Vec<&str> = uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or("");
            !pair.is_empty() && !matches!(key, "max_id" | "since_id" | "min_id")
        })
        .collect();
    let text = if kept.is_empty() {
        uri.path().to_string()
    } else {
        format!("{}?{}", uri.path(), kept.join("&"))
    };
    text.parse::<Uri>().unwrap_or_else(|_| uri.clone())
}

/// Render one page: batched status views, the `filtered` annotation for a
/// known viewer, and the cache and `Link` headers.
fn timeline_response(
    state: &ApiState,
    viewer: Option<Id>,
    uri: &Uri,
    cache: &'static str,
    page: TimelinePage,
    home_fold: bool,
) -> Response {
    let first = page.statuses.first().map(|status| status.id);
    let last = page.statuses.last().map(|status| status.id);
    match render(state, viewer, page.statuses, home_fold) {
        Ok(body) => {
            let mut response = axum::Json(body).into_response();
            let headers = response.headers_mut();
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            // `Link` only when the page is non-empty (Mastodon's shape).
            if let Some(link) = pagination::link_header(&link_base(uri), last, first) {
                if let Ok(value) = HeaderValue::from_str(&link) {
                    headers.insert(header::LINK, value);
                }
            }
            response
        }
        Err(err) => err.into_response(),
    }
}

fn render(
    state: &ApiState,
    viewer: Option<Id>,
    statuses: Vec<Status>,
    home_fold: bool,
) -> Result<Vec<Value>, ApiError> {
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let mut views = load_status_views(&state.server, store, viewer, statuses, now)?;
    if let Some(viewer) = viewer {
        let page: Vec<Status> = views.iter().map(|view| view.status.clone()).collect();
        let matches = filter_results_unscoped(store, viewer, &page, now)?;
        // D22 `Collapse` mode (home only): a status that mentions a
        // blocked/muted account renders folded behind a
        // content-warning-style `filtered` entry instead of being removed.
        let collapse =
            home_fold && timelines::home_mention_policy_of(store, viewer) == HomeMentionPolicy::Collapse;
        for view in &mut views {
            if let Some(entries) = matches.get(&view.status.id) {
                view.filtered = entries.clone();
            }
            if collapse
                && timelines::home_mention_fold(store, viewer, &view.status, now)
                    .unwrap_or(false)
            {
                view.filtered.push(serde_json::json!({
                    "filter": {
                        "id": "home-mention-fold",
                        "title": "Hidden mentions",
                        "context": ["home"],
                        "filter_action": "warn",
                        "expires_at": null
                    },
                    "keyword_matches": [],
                    "status_matches": []
                }));
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

/// Cache policy for the anonymous-capable feeds: only anonymous (no user)
/// responses may be shared-cached.
fn public_cache(viewer: Option<Id>) -> &'static str {
    if viewer.is_some() {
        PRIVATE_CACHE
    } else {
        PUBLIC_CACHE
    }
}

/// `GET /api/v1/timelines/home`.
pub async fn home(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_STATUSES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let mut query = match timeline_query(&uri) {
        Ok(query) => query,
        Err(err) => return err.into_response(),
    };
    // Aperture (Stage 2): flag on forces photo-only timelines regardless of
    // the client's `only_media`. No cursor/`Link` change.
    query.only_media |= state.server.aperture_mode;
    let store: &dyn Store = &*state.context.store;
    match timelines::home(store, viewer, query, state.context.now_ms()) {
        Ok(page) => timeline_response(&state, Some(viewer), &uri, PRIVATE_CACHE, page, true),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Feed-access gate (fix round P2 #21d; upstream
/// `PublicController#require_auth?` / `TopicController#require_auth?` plus
/// `PublicFeed#incompatible_feed_settings?`'s empty-page behavior).
///
/// `modes` are the relevant `*_feed_access` settings (selected like
/// upstream: `local` flag wins, else `remote`, else both). All `public` →
/// serve. Otherwise anonymous is refused (`401`, upstream `require_user!`);
/// an authenticated viewer without a functional credential, or a non-staff
/// viewer under `disabled`, gets an empty page (`200 []`, upstream
/// incompatible-settings behavior). `Ok(None)` = serve.
pub(crate) fn feed_gate(
    state: &ApiState,
    viewer: Option<Id>,
    modes: &[&str],
    uri: &Uri,
) -> Result<Option<Response>, ApiError> {
    if modes.iter().all(|mode| *mode == "public") {
        return Ok(None);
    }
    let Some(viewer) = viewer else {
        return Err(ApiError::invalid_token());
    };
    let store: &dyn Store = &*state.context.store;
    let user = store.user_by_account_id(viewer).map_err(ApiError::from)?;
    let functional = user.as_ref().is_some_and(|u| u.approved && !u.disabled);
    let staff = user
        .as_ref()
        .is_some_and(|u| u.role != crate::domain::UserRole::None);
    if !functional || (modes.iter().any(|mode| *mode == "disabled") && !staff) {
        return Ok(Some(timeline_response(
            state,
            Some(viewer),
            uri,
            PRIVATE_CACHE,
            TimelinePage {
                statuses: Vec::new(),
                scanned: 0,
            },
            false,
        )));
    }
    Ok(None)
}

/// `GET /api/v1/timelines/public`.
pub async fn public(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_STATUSES) {
        return err.into_response();
    }
    let viewer = viewer_of(&auth);
    let mut query = match timeline_query(&uri) {
        Ok(query) => query,
        Err(err) => return err.into_response(),
    };
    // Aperture (Stage 2): flag on forces photo-only timelines regardless of
    // the client's `only_media`. No cursor/`Link` change.
    query.only_media |= state.server.aperture_mode;
    let modes: Vec<&str> = if query.local_only {
        vec![state.server.local_live_feed_access.as_str()]
    } else if query.remote_only {
        vec![state.server.remote_live_feed_access.as_str()]
    } else {
        vec![
            state.server.local_live_feed_access.as_str(),
            state.server.remote_live_feed_access.as_str(),
        ]
    };
    match feed_gate(&state, viewer, &modes, &uri) {
        Ok(Some(refusal)) => return refusal,
        Ok(None) => {}
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match timelines::public(store, viewer, query, state.context.now_ms()) {
        Ok(page) => timeline_response(&state, viewer, &uri, public_cache(viewer), page, false),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/timelines/tag/{tag}`.
pub async fn tag(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
    uri: Uri,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_STATUSES) {
        return err.into_response();
    }
    let viewer = viewer_of(&auth);
    let mut query = match timeline_query(&uri) {
        Ok(query) => query,
        Err(err) => return err.into_response(),
    };
    // Aperture (Stage 2): flag on forces photo-only timelines regardless of
    // the client's `only_media`. No cursor/`Link` change.
    query.only_media |= state.server.aperture_mode;
    // Upstream `TagFeed` modes: `any[]` widens, `all[]` narrows, `none[]`
    // excludes (each capped in `timelines::tag_filtered`).
    let pairs = forms::parse(uri.query().unwrap_or("").as_bytes());
    let any: Vec<String> = forms::all(&pairs, "any").into_iter().map(str::to_string).collect();
    let all: Vec<String> = forms::all(&pairs, "all").into_iter().map(str::to_string).collect();
    let none: Vec<String> = forms::all(&pairs, "none").into_iter().map(str::to_string).collect();
    let store: &dyn Store = &*state.context.store;
    // Topic feed gate (#21d): same shape as the live gate above, over the
    // topic settings (upstream `TopicController#require_auth?`).
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
    match feed_gate(&state, viewer, &modes, &uri) {
        Ok(Some(refusal)) => return refusal,
        Ok(None) => {}
        Err(err) => return err.into_response(),
    };
    match timelines::tag_filtered(store, viewer, &raw, &any, &all, &none, query, state.context.now_ms()) {
        Ok(page) => timeline_response(&state, viewer, &uri, public_cache(viewer), page, false),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/timelines/list/{id}`.
pub async fn list(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_LISTS) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let mut query = match timeline_query(&uri) {
        Ok(query) => query,
        Err(err) => return err.into_response(),
    };
    // Aperture (Stage 2): flag on forces photo-only timelines regardless of
    // the client's `only_media`. No cursor/`Link` change.
    query.only_media |= state.server.aperture_mode;
    let store: &dyn Store = &*state.context.store;
    match timelines::list(store, viewer, id, query, state.context.now_ms()) {
        Ok(page) => timeline_response(&state, Some(viewer), &uri, PRIVATE_CACHE, page, false),
        Err(DomainError::NotFound(_)) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

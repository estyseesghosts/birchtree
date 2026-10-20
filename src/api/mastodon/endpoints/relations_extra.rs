//! Relation collection reads + tag surface (Phase 7).
//!
//! - `GET /api/v1/blocks`, `GET /api/v1/mutes` (account reads over the
//!   block/mute rows; mutes render `REST::MutedAccountSerializer`).
//! - `GET /api/v1/favourites`, `GET /api/v1/bookmarks` (status reads over
//!   the favourite/bookmark rows with viewer-aware status packing).
//! - `GET /api/v1/accounts/familiar_followers` (mutual-follow samples per
//!   requested account, per `FamiliarFollowersPresenter`).
//! - Featured tags (`GET/POST /api/v1/featured_tags`, `DELETE :id`,
//!   `GET /api/v1/accounts/:id/featured_tags`), followed tags
//!   (`GET /api/v1/followed_tags`), and the tag endpoints
//!   (`GET /api/v1/tags/:id`, `POST .../follow|unfollow|feature|unfeature`).
//!
//! Everything here is a real read over stored rows (empty tables read
//! empty, never literals). Scopes and auth follow the pinned controllers.

use axum::extract::{Path, State};
use axum::http::{Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto::{self, StatusSourceMode};
use crate::api::mastodon::endpoints::web;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::pagination;
use crate::api::mastodon::ApiState;
use crate::application::filters::filter_results_unscoped;
use crate::application::ports::{FeaturedTag, Store, TagFollow};
use crate::application::web as web_cases;
use crate::domain::{Account, Id};

const BLOCK_SCOPES: &[&str] = &["follow", "read", "read:blocks"];
const MUTE_SCOPES: &[&str] = &["follow", "read", "read:mutes"];
const FAVOURITE_SCOPES: &[&str] = &["read", "read:favourites"];
const BOOKMARK_SCOPES: &[&str] = &["read", "read:bookmarks"];
const FOLLOW_SCOPES: &[&str] = &["read", "read:follows"];
const ACCOUNT_SCOPES: &[&str] = &["read", "read:accounts"];
const WRITE_ACCOUNT_SCOPES: &[&str] = &["write", "write:accounts"];
const TAG_FOLLOW_SCOPES: &[&str] = &["follow", "write", "write:follows"];

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

fn require_named(store: &dyn Store, raw: &str) -> ApiResult<Account> {
    let id = raw.parse::<Id>().map_err(|_| ApiError::not_found())?;
    store
        .account_by_id(id)
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::not_found)
}

/// Muted-account JSON: the account plus `mute_expires_at` (pinned
/// `REST::MutedAccountSerializer`; `null` when the mute never expires).
fn muted_account_json(
    state: &ApiState,
    store: &dyn Store,
    account: &Account,
    mute: &crate::domain::Mute,
) -> Value {
    let mut object = match web::accounts_json(state, store, std::slice::from_ref(account))
        .into_iter()
        .next()
    {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    object.insert(
        "mute_expires_at".into(),
        mute.expire_at_ms
            .map(|ms| {
                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
                    .unwrap_or_default()
            })
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    Value::Object(object)
}

/// `GET /api/v1/blocks`: the viewer's blocked accounts.
pub async fn blocks(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(BLOCK_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri, 40, 80) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let ids = match web_cases::blocks_of(store, viewer, page.max_id, page.limit) {
        Ok(ids) => ids,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let accounts: Vec<Account> = ids
        .iter()
        .filter_map(|id| store.account_by_id(*id).ok().flatten())
        .collect();
    let full = ids.len() == page.limit;
    let next = if full { accounts.last().map(|a| a.id) } else { None };
    let prev = accounts.first().map(|a| a.id);
    let mut response = axum::Json(web::accounts_json(&state, store, &accounts)).into_response();
    if let Some(link) = pagination::link_header_desc(&uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

/// `GET /api/v1/mutes`: the viewer's muted accounts with expiry.
pub async fn mutes(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(MUTE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri, 40, 80) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let rows = match web_cases::mutes_of(store, viewer, page.max_id, page.limit) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut body = Vec::with_capacity(rows.len());
    for mute in &rows {
        match store.account_by_id(mute.muted_id).ok().flatten() {
            Some(account) => body.push(muted_account_json(&state, store, &account, mute)),
            None => continue,
        }
    }
    let full = rows.len() == page.limit;
    let next = if full { rows.last().map(|m| m.muted_id) } else { None };
    let prev = rows.first().map(|m| m.muted_id);
    let mut response = axum::Json(body).into_response();
    if let Some(link) = pagination::link_header_desc(&uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

/// Shared status-page rendering for favourites/bookmarks (viewer-aware
/// packing with filter annotations, like the timelines).
fn status_page(
    state: &ApiState,
    viewer: Id,
    uri: &Uri,
    statuses: Vec<crate::domain::Status>,
    next: Option<Id>,
    prev: Option<Id>,
) -> Response {
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let views = match web::status_views(state, Some(viewer), statuses.clone()) {
        Ok(views) => views,
        Err(err) => return err.into_response(),
    };
    let mut views = views;
    let page: Vec<crate::domain::Status> =
        views.iter().map(|view| view.status.clone()).collect();
    match filter_results_unscoped(store, viewer, &page, now) {
        Ok(matches) => {
            for view in &mut views {
                if let Some(entries) = matches.get(&view.status.id) {
                    view.filtered = entries.clone();
                }
            }
        }
        Err(err) => return ApiError::from(err).into_response(),
    }
    let body: Vec<Value> = views
        .iter()
        .map(|view| {
            dto::status_json(
                &state.server,
                Some(viewer),
                now,
                view,
                StatusSourceMode::Content,
            )
        })
        .collect();
    let mut response = axum::Json(body).into_response();
    if let Some(link) = pagination::link_header(uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

/// `GET /api/v1/favourites`: the viewer's favourited statuses.
pub async fn favourites(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(FAVOURITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri, 20, 40) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let rows =
        match web_cases::favourited(store, viewer, page.max_id, page.since_id, page.min_id, page.limit) {
            Ok(rows) => rows,
            Err(err) => return ApiError::from(err).into_response(),
        };
    let statuses: Vec<crate::domain::Status> = rows
        .iter()
        .filter_map(|fav| store.status_by_id(fav.status_id).ok().flatten())
        .filter(|status| !status.is_deleted())
        .collect();
    let full = rows.len() == page.limit;
    let next = if full { rows.last().map(|r| r.id) } else { None };
    let prev = if rows.is_empty() {
        None
    } else {
        rows.first().map(|r| r.id)
    };
    status_page(&state, viewer, &uri, statuses, next, prev)
}

/// `GET /api/v1/bookmarks`: the viewer's bookmarked statuses.
pub async fn bookmarks(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(BOOKMARK_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri, 20, 40) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let rows =
        match web_cases::bookmarked(store, viewer, page.max_id, page.since_id, page.min_id, page.limit) {
            Ok(rows) => rows,
            Err(err) => return ApiError::from(err).into_response(),
        };
    let statuses: Vec<crate::domain::Status> = rows
        .iter()
        .filter_map(|mark| store.status_by_id(mark.status_id).ok().flatten())
        .filter(|status| !status.is_deleted())
        .collect();
    let full = rows.len() == page.limit;
    let next = if full { rows.last().map(|r| r.id) } else { None };
    let prev = if rows.is_empty() {
        None
    } else {
        rows.first().map(|r| r.id)
    };
    status_page(&state, viewer, &uri, statuses, next, prev)
}

/// `GET /api/v1/accounts/familiar_followers?id[]=…`.
pub async fn familiar_followers(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_scopes(FOLLOW_SCOPES) {
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
    let mut ids = Vec::new();
    for (key, value) in &pairs {
        if key == "id" || key == "id[]" {
            if let Ok(id) = value.parse::<Id>() {
                ids.push(id);
            }
        }
    }
    let store: &dyn Store = &*state.context.store;
    let entries = match web_cases::familiar_followers(store, viewer, &ids, state.context.now_ms()) {
        Ok(entries) => entries,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut body = Vec::with_capacity(entries.len());
    for entry in &entries {
        let accounts: Vec<Account> = entry
            .familiar_ids
            .iter()
            .filter_map(|id| store.account_by_id(*id).ok().flatten())
            .collect();
        body.push(serde_json::json!({
            "id": entry.target_id.to_string(),
            "accounts": web::accounts_json(&state, store, &accounts),
        }));
    }
    axum::Json(body).into_response()
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

/// Pinned `REST::TagSerializer` projection over a stored or ad-hoc tag
/// name. There is no tag table yet (recorded in the manifest), so the name
/// is the id; `history` is a contract-shaped empty array; `following` and
/// `featuring` appear for authenticated viewers with real values.
fn tag_json(
    state: &ApiState,
    store: &dyn Store,
    viewer: Option<Id>,
    name: &str,
) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("id".into(), Value::String(name.to_string()));
    object.insert("name".into(), Value::String(name.to_string()));
    object.insert(
        "url".into(),
        Value::String(state.server.asset_url(&format!("tags/{name}"))),
    );
    object.insert("history".into(), Value::Array(Vec::new()));
    if let Some(viewer) = viewer {
        let (following, featuring) = web_cases::tag_relationship(store, viewer, name)
            .unwrap_or((false, false));
        object.insert("following".into(), Value::Bool(following));
        object.insert("featuring".into(), Value::Bool(featuring));
    }
    Value::Object(object)
}

/// `GET /api/v1/followed_tags`: tags the viewer follows.
pub async fn followed_tags(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["follow", "read", "read:follows"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri, web_cases::FOLLOWED_TAGS_LIMIT, web_cases::FOLLOWED_TAGS_LIMIT) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let rows = match web_cases::followed_tags(store, viewer, page.max_id, page.since_id, page.limit) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let body: Vec<Value> = rows
        .iter()
        .map(|row: &TagFollow| tag_json(&state, store, Some(viewer), &row.tag))
        .collect();
    let full = rows.len() == page.limit;
    let next = if full { rows.last().map(|r| r.id) } else { None };
    let prev = if rows.is_empty() {
        None
    } else {
        rows.first().map(|r| r.id)
    };
    let mut response = axum::Json(body).into_response();
    if let Some(link) = pagination::link_header_desc(&uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

/// Pinned `REST::FeaturedTagSerializer`: `{id, name, url, statuses_count,
/// last_status_at}`. `statuses_count` renders as a string; `last_status_at`
/// is a `YYYY-MM-DD` date or `null`.
fn featured_tag_json(state: &ApiState, row: &FeaturedTag, count: u64, last: Option<u64>) -> Value {
    let account_url = format!(
        "{}/tagged/{}",
        state.server.public_url.trim_end_matches('/'),
        row.tag
    );
    serde_json::json!({
        "id": row.id.to_string(),
        "name": row.tag,
        "url": account_url,
        "statuses_count": count.to_string(),
        "last_status_at": last.map(|ms| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
                .map(|dt| dt.date_naive().to_string())
                .unwrap_or_else(|| "1970-01-01".to_string())
        }),
    })
}

/// `GET /api/v1/featured_tags`: the viewer's featured tags with live counts.
pub async fn featured_tags_index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match web_cases::featured_tags(store, viewer, state.context.now_ms()) {
        Ok(rows) => {
            let body: Vec<Value> = rows
                .iter()
                .map(|row| {
                    featured_tag_json(&state, &row.tag, row.statuses_count, row.last_status_at_ms)
                })
                .collect();
            axum::Json(body).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/featured_tags`: feature `name` on the viewer's profile.
pub async fn featured_tags_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let name = forms::get(&body, "name").unwrap_or("").to_string();
    let store: &dyn Store = &*state.context.store;
    match web_cases::feature_tag(store, viewer, &name, state.context.now_ms()) {
        Ok(row) => {
            match web_cases::featured_tags(store, viewer, state.context.now_ms()) {
                Ok(rows) => {
                    let usage = rows.iter().find(|r| r.tag.id == row.id);
                    let (count, last) = usage
                        .map(|r| (r.statuses_count, r.last_status_at_ms))
                        .unwrap_or((0, None));
                    axum::Json(featured_tag_json(&state, &row, count, last)).into_response()
                }
                Err(err) => ApiError::from(err).into_response(),
            }
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/featured_tags/:id`: `:id` is the numeric featured-tag row.
pub async fn featured_tags_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_ACCOUNT_SCOPES) {
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
    match web_cases::unfeature_tag(store, viewer, id) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/:id/featured_tags`: public; unavailable → `[]`.
pub async fn account_featured_tags(
    State(state): State<ApiState>,
    _auth: OptionalAuth,
    Path(target): Path<String>,
) -> Response {
    let store: &dyn Store = &*state.context.store;
    let target = match require_named(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    if target.suspended {
        return axum::Json(Vec::<Value>::new()).into_response();
    }
    match web_cases::featured_tags(store, target.id, state.context.now_ms()) {
        Ok(rows) => {
            let body: Vec<Value> = rows
                .iter()
                .map(|row| {
                    featured_tag_json(&state, &row.tag, row.statuses_count, row.last_status_at_ms)
                })
                .collect();
            axum::Json(body).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/tags/:id`: public tag display (unsaved names allowed).
pub async fn tag_show(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
) -> Response {
    if !web_cases::valid_tag_name(&raw) {
        return ApiError::not_found().into_response();
    }
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let store: &dyn Store = &*state.context.store;
    let name = web_cases::normalize_tag_name(&raw);
    axum::Json(tag_json(&state, store, viewer, &name)).into_response()
}

/// Shared tag-mutation response: the tag with the viewer's fresh flags.
fn tag_response(state: &ApiState, viewer: Id, name: &str) -> Response {
    let store: &dyn Store = &*state.context.store;
    axum::Json(tag_json(state, store, Some(viewer), name)).into_response()
}

/// `POST /api/v1/tags/:id/follow`.
pub async fn tag_follow(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(TAG_FOLLOW_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match web_cases::follow_tag(store, viewer, &raw, state.context.now_ms()) {
        Ok(row) => tag_response(&state, viewer, &row.tag),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/tags/:id/unfollow`.
pub async fn tag_unfollow(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(TAG_FOLLOW_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let name = web_cases::normalize_tag_name(&raw);
    match web_cases::unfollow_tag(store, viewer, &raw) {
        Ok(_) => tag_response(&state, viewer, &name),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/tags/:id/feature`.
pub async fn tag_feature(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match web_cases::feature_tag(store, viewer, &raw, state.context.now_ms()) {
        Ok(row) => tag_response(&state, viewer, &row.tag),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/tags/:id/unfeature`.
pub async fn tag_unfeature(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let name = web_cases::normalize_tag_name(&raw);
    if !web_cases::valid_tag_name(&raw) {
        return ApiError::not_found().into_response();
    }
    // Unfeature by name: resolve the viewer's row first.
    let rows = match web_cases::featured_tags(store, viewer, state.context.now_ms()) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    if let Some(row) = rows.iter().find(|r| r.tag.tag == name) {
        let row_id = row.tag.id;
        match web_cases::unfeature_tag(store, viewer, row_id) {
            Ok(_) => tag_response(&state, viewer, &name),
            Err(err) => return ApiError::from(err).into_response(),
        }
    } else {
        tag_response(&state, viewer, &name)
    }
}

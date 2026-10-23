//! Collections + missing-REST-surface handlers (gaps batch, `docs/gaps.md`, G-C).
//!
//! ALL G-C REST handlers live here — even ones that upstream files elsewhere —
//! so no other agent's files are touched. The parent merge mounts them via the
//! router snippet in the G-C final report (one snippet, every method+path,
//! mirrored by [`ROUTES`]).
//!
//! Shape note: these are plain `fn`s over narrow store handles, not axum
//! handlers. `Context.store` is `Arc<dyn Store>`, and the G-C port trait
//! joins `Store` only at the parent merge (it is defined in
//! `application::collections` until the parent moves it into `ports.rs`), so
//! axum adapters that pull both handles out of `State<ApiState>` cannot
//! compile until the merge lands. The snippet adapters are three-line
//! extractor wrappers around these fns; until the parent mounts them, the
//! `gaps_collections` suite calls these fns directly.

use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::forms;
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::pagination;
use crate::api::mastodon::status_view::load_status_views;
use crate::api::mastodon::ServerInfo;
use crate::application::collections::{self};
use crate::application::ports::{CollectionRecord, CollectionStore};
use crate::application::notifications::{
    self, COUNT_DEFAULT_LIMIT, COUNT_MAX_LIMIT, NotificationQuery,
};
use crate::application::ports::Store;
use crate::domain::{Id, Notification, NotificationType};

// ---------------------------------------------------------------------------
// Route table (mirrored by the parent-mounted router snippet; the suite
// asserts this table is non-empty, unique and `/api/`-rooted).
// ---------------------------------------------------------------------------

/// `(method, path)` for every G-C route, in mount order. Static paths sort
/// before dynamic ones (so `peers/search` never resolves as a peer id, and
/// `notifications/clear` never resolves as a group key).
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/collections"),
    ("POST", "/api/v1/collections"),
    ("GET", "/api/v1/collections/:id"),
    ("PUT", "/api/v1/collections/:id"),
    ("PATCH", "/api/v1/collections/:id"),
    ("DELETE", "/api/v1/collections/:id"),
    ("GET", "/api/v1/collections/:id/items"),
    ("POST", "/api/v1/collections/:id/items"),
    ("DELETE", "/api/v1/collections/:id/items"),
    ("POST", "/api/v1/collections/:id/revoke"),
    ("GET", "/api/v1/statuses/:id/in_collections"),
    ("GET", "/api/v1/accounts/:id/collections"),
    ("GET", "/api/v1/accounts/:id/lists"),
    ("POST", "/api/v1/domain_blocks/preview"),
    ("GET", "/api/v1/instance/peers/search"),
    ("GET", "/api/v1_alpha/collections"),
    ("GET", "/api/v2/notifications"),
    ("POST", "/api/v2/notifications/clear"),
    ("GET", "/api/v2/notifications/unread_count"),
    ("POST", "/api/v2/notifications/:group_key/dismiss"),
    ("GET", "/api/v2/notifications/:group_key/accounts"),
    ("GET", "/api/v2/notifications/policy"),
];

// ---------------------------------------------------------------------------
// Scopes + small helpers.
// ---------------------------------------------------------------------------

/// Read scopes for collection reads (broad `read`, plus the granular name
/// for forward-compatibility; `require_scopes` is any-of).
const READ_SCOPES: &[&str] = &["read", "read:collections"];
/// Write scopes for collection writes.
const WRITE_SCOPES: &[&str] = &["write", "write:collections"];
/// Read scopes for the account `lists` sub-resource (same as lists).
const LISTS_READ_SCOPES: &[&str] = &["read", "read:lists"];
/// Write scopes for `domain_blocks/preview` (same controller as create).
const BLOCK_WRITE_SCOPES: &[&str] = &["follow", "write", "write:blocks"];
/// Read scopes for the v2 notifications family (same as v1).
const NOTIF_READ_SCOPES: &[&str] = &["read", "read:notifications"];
/// Write scopes for v2 clear/dismiss.
const NOTIF_WRITE_SCOPES: &[&str] = &["write", "write:notifications"];

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

fn iso_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// Scope check + user resolution for required-auth handlers.
fn authed(auth: &Auth, scopes: &[&str]) -> Result<Id, Response> {
    if let Err(err) = auth.0.require_scopes(scopes) {
        return Err(err.into_response());
    }
    auth.0.require_user().map_err(|err| err.into_response())
}

/// Scope check for optional-auth handlers; a present token must carry the
/// scope, an app-only token reads as anonymous.
fn optional_viewer(auth: &OptionalAuth, scopes: &[&str]) -> Result<Option<Id>, Response> {
    match &auth.0 {
        None => Ok(None),
        Some(authenticated) => {
            if let Err(err) = authenticated.require_scopes(scopes) {
                return Err(err.into_response());
            }
            Ok(authenticated.token.user_id)
        }
    }
}

fn parse_id(raw: &str) -> Result<Id, Response> {
    raw.parse::<Id>()
        .map_err(|_| ApiError::not_found().into_response())
}

fn parse_status_id(raw: &str) -> Result<Id, Response> {
    // Statuses sub-routes use the explicit `Not Found` body.
    raw.parse::<Id>().map_err(|_| {
        ApiError::new(axum::http::StatusCode::NOT_FOUND, "Not Found").into_response()
    })
}

/// The collection DTO (birchtree shape; every key is stored, never
/// fabricated — see `CollectionRecord`).
pub fn collection_json(record: &CollectionRecord) -> Value {
    serde_json::json!({
        "id": record.id.to_string(),
        "account_id": record.account_id.to_string(),
        "title": record.title,
        "description": record.description,
        "public": record.public,
        "revoked": record.revoked,
        "items_count": record.items_count,
        "created_at": iso_ms(record.created_at_ms),
        "updated_at": iso_ms(record.updated_at_ms),
    })
}

fn status_ids(body: &[(String, String)]) -> Vec<Id> {
    forms::all(body, "status_ids")
        .iter()
        .filter_map(|raw| raw.parse::<Id>().ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Top-level `collections` CRUD.
// ---------------------------------------------------------------------------

/// `GET /api/v1/collections` — the viewer's collections.
pub fn collections_index(store: &dyn CollectionStore, auth: &Auth) -> Response {
    let viewer = match authed(auth, READ_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    match collections::index(store, viewer) {
        Ok(rows) => axum::Json(rows.iter().map(collection_json).collect::<Vec<_>>())
            .into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/collections` — create (`title`, `description`, `public`).
/// `public` defaults to false (private until published).
pub fn collections_create(
    store: &dyn CollectionStore,
    auth: &Auth,
    body: &[(String, String)],
    now_ms: u64,
) -> Response {
    let viewer = match authed(auth, WRITE_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let title = forms::get(body, "title").unwrap_or("");
    let description = forms::get(body, "description").unwrap_or("");
    let public = forms::get(body, "public").is_some_and(bool_cast);
    match collections::create(store, viewer, title, description, public, now_ms) {
        Ok(record) => axum::Json(collection_json(&record)).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/collections/:id` — owner sees everything; others see only
/// public, non-revoked collections.
pub fn collections_show(
    store: &dyn CollectionStore,
    auth: &OptionalAuth,
    raw: &str,
) -> Response {
    let viewer = match optional_viewer(auth, READ_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match collections::show(store, viewer, id) {
        Ok(record) => axum::Json(collection_json(&record)).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT/PATCH /api/v1/collections/:id` — patch an owned collection.
pub fn collections_update(
    store: &dyn CollectionStore,
    auth: &Auth,
    raw: &str,
    body: &[(String, String)],
    now_ms: u64,
) -> Response {
    let viewer = match authed(auth, WRITE_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let has = |key: &str| body.iter().any(|(k, _)| k == key);
    match collections::update(
        store,
        viewer,
        id,
        has("title").then(|| forms::get(body, "title").unwrap_or("").to_string()),
        has("description")
            .then(|| forms::get(body, "description").unwrap_or("").to_string()),
        has("public").then(|| forms::get(body, "public").is_some_and(bool_cast)),
        now_ms,
    ) {
        Ok(record) => axum::Json(collection_json(&record)).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/collections/:id` — `200 {}`.
pub fn collections_destroy(
    store: &dyn CollectionStore,
    auth: &Auth,
    raw: &str,
    now_ms: u64,
) -> Response {
    let viewer = match authed(auth, WRITE_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match collections::destroy(store, viewer, id, now_ms) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Collection items + revoke.
// ---------------------------------------------------------------------------

/// Page bound for the items read (status-shaped pages, always bounded — no
/// unlimited path, unlike list members).
const ITEMS_DEFAULT_LIMIT: usize = 20;
const ITEMS_MAX_LIMIT: usize = 40;

/// Ambient handles shared by the collection-items endpoints.
///
/// `store` and `collections` are usually the same `Arc<dyn Store>` handle
/// passed twice (the G-C port joins `Store` at the parent merge); `server`
/// is carried for rendering even though `items_add` ignores it, so both
/// axum adapters construct the context identically. Per-request wire values
/// (`auth`, `raw`, `query`/`body`) stay explicit parameters.
pub struct CollectionCtx<'a> {
    /// Broad store handle (accounts, statuses, notifications).
    pub store: &'a dyn Store,
    /// Narrow collections-table handle.
    pub collections: &'a dyn CollectionStore,
    /// Server info for status rendering.
    pub server: &'a ServerInfo,
    /// Clock reading, bound once per request in the adapter.
    pub now_ms: u64,
}

/// Cursor window for [`scan_notifications`]: the v2 grouping scan over one
/// recipient's rows. Bundles the five scan inputs that are not the store so
/// the private helper takes two parameters.
pub struct CollectionScan {
    /// Owner of the scanned notification rows.
    pub recipient: Id,
    /// Exclusive upper cursor over numeric notification ids.
    pub max_id: Option<Id>,
    /// Exclusive lower cursor (newer side).
    pub since_id: Option<Id>,
    /// Exclusive lower cursor (older side).
    pub min_id: Option<Id>,
    /// Row-scan bound (`V2_SCAN_ROWS` at both call sites).
    pub cap: usize,
}

/// `GET /api/v1/collections/:id/items` — member statuses, newest first.
///
/// Privacy: the collection itself must be visible (see `show`); when the
/// owner's `hide_collections` is set and the viewer is not the owner, the
/// members hide (`200 []`, counts stay on the collection row — D30/D52).
/// Each candidate status additionally passes the `show_status` policy gate
/// for the viewer, so private statuses in a public collection never leak.
pub fn items_index(
    ctx: &CollectionCtx<'_>,
    auth: &OptionalAuth,
    raw: &str,
    query: Option<&str>,
) -> Response {
    let viewer = match optional_viewer(auth, READ_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let pairs = query.map(|q| forms::parse(q.as_bytes())).unwrap_or_default();
    let page = match pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        None,
        None,
        ITEMS_DEFAULT_LIMIT,
        ITEMS_MAX_LIMIT,
    ) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let record = match collections::show(ctx.collections, viewer, id) {
        Ok(record) => record,
        Err(err) => return ApiError::from(err).into_response(),
    };
    // `hide_collections`: members hidden for flagged owners (counts stay).
    let hide = match ctx.store.account_by_id(record.account_id) {
        Ok(Some(owner)) => !collections::members_visible(owner.hide_collections, viewer, owner.id),
        // A missing owner row reads like "never set" (fail-open, matching
        // the approval fail-open in `auth.rs`).
        Ok(None) | Err(_) => false,
    };
    if hide {
        return axum::Json(Vec::<Value>::new()).into_response();
    }
    let ids = match collections::items(ctx.collections, viewer, id, page.max_id, page.limit) {
        Ok(ids) => ids,
        Err(err) => return ApiError::from(err).into_response(),
    };
    // Batch-resolve + per-viewer policy gate (one `show_status` per
    // candidate, bounded by the page limit).
    let statuses = match ctx.store.statuses_by_ids(&ids) {
        Ok(statuses) => statuses,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut visible = Vec::new();
    for status in statuses {
        if crate::application::statuses::show_status(ctx.store, viewer, status.id, ctx.now_ms).is_ok() {
            visible.push(status);
        }
    }
    let views = match load_status_views(ctx.server, ctx.store, viewer, visible, ctx.now_ms) {
        Ok(views) => views,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let body: Vec<Value> = views
        .iter()
        .map(|view| {
            dto::status_json(
                ctx.server,
                viewer,
                ctx.now_ms,
                view,
                dto::StatusSourceMode::Content,
            )
        })
        .collect();
    body_response(query, body, views.iter().map(|v| v.status.id).collect())
}

/// Render a status page with a `Link` header over the kept ids.
fn body_response(query: Option<&str>, body: Vec<Value>, kept: Vec<Id>) -> Response {
    let mut response = axum::Json(body).into_response();
    let uri: axum::http::Uri = query
        .map(|q| format!("/?{q}"))
        .unwrap_or_else(|| "/".to_string())
        .parse()
        .unwrap_or_else(|_| "/".parse().unwrap());
    if let Some(link) = pagination::link_header(&uri, kept.last().copied(), kept.first().copied())
    {
        if let Ok(value) = axum::http::header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(axum::http::header::LINK, value);
        }
    }
    response
}

/// `POST /api/v1/collections/:id/items` — add `status_ids[]` (`200 {added:[...]}`).
pub fn items_add(
    ctx: &CollectionCtx<'_>,
    auth: &Auth,
    raw: &str,
    body: &[(String, String)],
) -> Response {
    let viewer = match authed(auth, WRITE_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match collections::add_items(ctx.store, ctx.collections, viewer, id, &status_ids(body), ctx.now_ms) {
        Ok(added) => axum::Json(
            serde_json::json!({ "added": added.iter().map(|id| id.to_string()).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/collections/:id/items` — remove `status_ids[]`
/// (idempotent, `200 {removed:[...]}`).
pub fn items_remove(
    collections: &dyn CollectionStore,
    auth: &Auth,
    raw: &str,
    body: &[(String, String)],
    now_ms: u64,
) -> Response {
    let viewer = match authed(auth, WRITE_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match collections::remove_items(collections, viewer, id, &status_ids(body), now_ms) {
        Ok(removed) => axum::Json(
            serde_json::json!({ "removed": removed.iter().map(|id| id.to_string()).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/collections/:id/revoke` — the owner empties the collection
/// and marks it revoked (members fan out as `collection_update`).
pub fn collections_revoke(
    collections: &dyn CollectionStore,
    auth: &Auth,
    raw: &str,
    now_ms: u64,
) -> Response {
    let viewer = match authed(auth, WRITE_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match collections::revoke(collections, viewer, id, now_ms) {
        Ok(record) => axum::Json(collection_json(&record)).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// `in_collections`, account `collections`, account `lists`.
// ---------------------------------------------------------------------------

/// `GET /api/v1/statuses/:id/in_collections` — the viewer's collections
/// containing the status.
pub fn in_collections(
    collections: &dyn CollectionStore,
    auth: &Auth,
    raw: &str,
) -> Response {
    let viewer = match authed(auth, READ_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_status_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match collections::in_collections(collections, viewer, id) {
        Ok(rows) => axum::Json(rows.iter().map(collection_json).collect::<Vec<_>>())
            .into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/:id/collections` — the account's public,
/// non-revoked collections (counts stay even under `hide_collections`;
/// only the member lists hide, on the items read).
pub fn account_collections(
    store: &dyn Store,
    collections: &dyn CollectionStore,
    auth: &OptionalAuth,
    raw: &str,
) -> Response {
    if let Err(response) = optional_viewer(auth, READ_SCOPES) {
        return response;
    }
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match store.account_by_id(id) {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    }
    match collections.collections_by_account(id) {
        Ok(rows) => {
            let body: Vec<Value> = rows
                .iter()
                .filter(|row| row.public && !row.revoked)
                .map(collection_json)
                .collect();
            axum::Json(body).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/:id/lists` — the viewer's lists containing the
/// account (unknown accounts 404).
pub fn account_lists(store: &dyn Store, auth: &Auth, raw: &str) -> Response {
    let viewer = match authed(auth, LISTS_READ_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let exists = match store.account_by_id(id) {
        Ok(account) => account.is_some(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    if !exists {
        return ApiError::not_found().into_response();
    }
    let lists = match store.lists_by_account(viewer) {
        Ok(lists) => lists,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut out = Vec::new();
    for list in lists {
        let members = match store.all_list_members(list.id, 10_000) {
            Ok(members) => members,
            Err(err) => return ApiError::from(err).into_response(),
        };
        if members.contains(&id) {
            out.push(dto::list_json(&list));
        }
    }
    axum::Json(out).into_response()
}

// ---------------------------------------------------------------------------
// `domain_blocks/preview`, `peers/search`, minimal `v1_alpha`.
// ---------------------------------------------------------------------------

/// `POST /api/v1/domain_blocks/preview` — `{domain, accounts_count}` over
/// known remote accounts (suspended accounts excluded; missing rows
/// excluded, never fabricated).
pub fn domain_blocks_preview(
    store: &dyn Store,
    auth: &Auth,
    body: &[(String, String)],
) -> Response {
    let _viewer = match authed(auth, BLOCK_WRITE_SCOPES) {
        Ok(viewer) => viewer,
        Err(response) => return response,
    };
    let domain = forms::get(body, "domain").unwrap_or("").trim().to_lowercase();
    if domain.is_empty() {
        return ApiError::unprocessable("Validation failed: Domain can't be blank")
            .into_response();
    }
    let overview = match store.admin_accounts_overview(PEER_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let ids: Vec<Id> = overview
        .iter()
        .filter(|row| !row.local && row.domain == domain)
        .map(|row| row.id)
        .collect();
    let accounts = match store.accounts_by_ids(&ids) {
        Ok(accounts) => accounts,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let count = ids
        .iter()
        .filter(|id| accounts.get(id).is_some_and(|account| !account.suspended))
        .count();
    axum::Json(serde_json::json!({ "domain": domain, "accounts_count": count })).into_response()
}

/// Bound for the peer/overview scans backing `peers/search` + preview.
const PEER_SCAN_CAP: usize = 5_000;

/// `GET /api/v1/instance/peers/search?q=` — substring-filtered known remote
/// domains, sorted (same source + order as `peers`; 404 when the peers API
/// is disabled, like `peers`).
pub fn peers_search(store: &dyn Store, peers_enabled: bool, query: Option<&str>) -> Response {
    if !peers_enabled {
        return ApiError::not_found().into_response();
    }
    let pairs = query.map(|q| forms::parse(q.as_bytes())).unwrap_or_default();
    let needle = forms::get(&pairs, "q").unwrap_or("").trim().to_lowercase();
    let overview = match store.admin_accounts_overview(PEER_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut set = std::collections::BTreeSet::new();
    for row in &overview {
        if row.local {
            continue;
        }
        if !needle.is_empty() && !row.domain.to_lowercase().contains(&needle) {
            continue;
        }
        set.insert(row.domain.clone());
    }
    axum::Json(set.into_iter().collect::<Vec<_>>()).into_response()
}

/// `GET /api/v1_alpha/collections` — minimal experimental namespace: the
/// viewer's collections, same shape as the top-level index.
pub fn alpha_collections_index(store: &dyn CollectionStore, auth: &Auth) -> Response {
    collections_index(store, auth)
}

// ---------------------------------------------------------------------------
// `GET /api/v2/notifications` family.
// ---------------------------------------------------------------------------

/// Row-scan bound for v2 grouping (newest 2000 rows; groups derive from real
/// rows only — beyond the bound older rows group on the next page cursor).
const V2_SCAN_ROWS: usize = 2_000;
/// One store page inside the v2 scan.
const V2_SCAN_PAGE: usize = 400;
/// Default/max group counts for the v2 index.
const V2_DEFAULT_LIMIT: usize = 20;
const V2_MAX_LIMIT: usize = 40;
/// Max sample accounts rendered per group.
const V2_SAMPLE_ACCOUNTS: usize = 4;

/// The v2 group key: `<type>-<anchor>` where the anchor is the status id
/// when present, else the actor id, else 0. All three segments parse back
/// (no upstream type string contains `-`), so dismiss/accounts resolve the
/// exact same row set the index grouped.
fn group_key(notification: &Notification) -> String {
    let anchor = notification
        .status_id
        .or(notification.actor_id)
        .map(|id| id.value())
        .unwrap_or(0);
    format!("{}-{anchor}", notification.r#type.as_str())
}

fn parse_group_key(raw: &str) -> Option<(NotificationType, u64)> {
    let (kind, anchor) = raw.split_once('-')?;
    Some((NotificationType::parse(kind)?, anchor.parse::<u64>().ok()?))
}

/// Newest-first row scan for one recipient, bounded by `V2_SCAN_ROWS`.
fn scan_notifications(
    store: &dyn Store,
    scan: CollectionScan,
) -> Result<Vec<Notification>, ApiError> {
    let mut out = Vec::new();
    let mut cursor = scan.max_id;
    loop {
        let remaining = scan.cap.saturating_sub(out.len()).min(V2_SCAN_PAGE);
        if remaining == 0 {
            break;
        }
        let page = notifications::index(
            store,
            scan.recipient,
            NotificationQuery {
                max_id: cursor,
                since_id: scan.since_id,
                min_id: scan.min_id,
                limit: remaining,
                from_account: None,
                include_filtered: false,
            },
        )?;
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|n| n.id);
        let full = page.len() >= remaining;
        out.extend(page);
        if out.len() >= scan.cap || !full {
            break;
        }
    }
    Ok(out)
}

/// One rendered v2 group (every field derives from stored rows).
fn group_json(rows: &[Notification]) -> Value {
    let first = rows.first().expect("non-empty group");
    let mut sample: Vec<String> = Vec::new();
    for row in rows {
        if let Some(actor) = row.actor_id {
            let id = actor.to_string();
            if !sample.contains(&id) {
                sample.push(id);
            }
            if sample.len() >= V2_SAMPLE_ACCOUNTS {
                break;
            }
        }
    }
    let status_id = match first.status_id {
        Some(id) => Value::String(id.to_string()),
        None => Value::Null,
    };
    serde_json::json!({
        "group_key": group_key(first),
        "type": first.r#type.as_str(),
        "notifications_count": rows.len() as u64,
        "most_recent_notification_id": first.id.to_string(),
        "page_min_id": rows.last().map(|n| n.id.to_string()).unwrap_or_default(),
        "page_max_id": first.id.to_string(),
        "sample_account_ids": sample,
        "status_id": status_id,
    })
}

fn group_rows(rows: Vec<Notification>) -> Vec<Vec<Notification>> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<Notification>> =
        std::collections::HashMap::new();
    for row in rows {
        let key = group_key(&row);
        groups.entry(key.clone()).or_default().push(row);
        if !order.contains(&key) {
            order.push(key);
        }
    }
    order
        .into_iter()
        .filter_map(|key| groups.remove(&key))
        .collect()
}

/// `GET /api/v2/notifications` — grouped notifications, newest group first.
/// `limit` bounds the groups (default 20, max 40); `max_id`/`since_id`/
/// `min_id` are numeric row cursors over the underlying notifications.
pub fn v2_index(store: &dyn Store, auth: &Auth, query: Option<&str>) -> Response {
    let recipient = match authed(auth, NOTIF_READ_SCOPES) {
        Ok(recipient) => recipient,
        Err(response) => return response,
    };
    let pairs = query.map(|q| forms::parse(q.as_bytes())).unwrap_or_default();
    let page = match pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        V2_DEFAULT_LIMIT,
        V2_MAX_LIMIT,
    ) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let rows = match scan_notifications(
        store,
        CollectionScan {
            recipient,
            max_id: page.max_id,
            since_id: page.since_id,
            min_id: page.min_id,
            cap: V2_SCAN_ROWS,
        },
    ) {
        Ok(rows) => rows,
        Err(err) => return err.into_response(),
    };
    let first = rows.first().map(|n| n.id);
    let last = rows.last().map(|n| n.id);
    let body: Vec<Value> = group_rows(rows)
        .into_iter()
        .take(page.limit)
        .map(|group| group_json(&group))
        .collect();
    let mut response = axum::Json(body).into_response();
    let uri: axum::http::Uri = query
        .map(|q| format!("/?{q}"))
        .unwrap_or_else(|| "/".to_string())
        .parse()
        .unwrap_or_else(|_| "/".parse().unwrap());
    if let Some(link) = pagination::link_header(&uri, last, first) {
        if let Ok(value) = axum::http::header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(axum::http::header::LINK, value);
        }
    }
    response
}

/// `POST /api/v2/notifications/clear` — delete every notification (`200 {}`).
pub fn v2_clear(store: &dyn Store, auth: &Auth) -> Response {
    let recipient = match authed(auth, NOTIF_WRITE_SCOPES) {
        Ok(recipient) => recipient,
        Err(response) => return response,
    };
    match notifications::clear(store, recipient) {
        Ok(()) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v2/notifications/unread_count` — `{"count": N}` against the
/// notifications marker (limit default 100, max 1000).
pub fn v2_unread_count(store: &dyn Store, auth: &Auth, query: Option<&str>) -> Response {
    let recipient = match authed(auth, NOTIF_READ_SCOPES) {
        Ok(recipient) => recipient,
        Err(response) => return response,
    };
    let pairs = query.map(|q| forms::parse(q.as_bytes())).unwrap_or_default();
    let limit: u64 = match forms::get(&pairs, "limit").map(|raw| raw.parse::<u64>().unwrap_or(COUNT_DEFAULT_LIMIT)) {
        Some(value) => value.min(COUNT_MAX_LIMIT).max(1),
        None => COUNT_DEFAULT_LIMIT,
    };
    match notifications::unread_count(store, recipient, limit) {
        Ok(count) => axum::Json(serde_json::json!({ "count": count })).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Resolve a group key to its rows (404 for unknown keys — never an empty
/// success that hides a typo'd key).
fn group_lookup(store: &dyn Store, recipient: Id, raw: &str) -> Result<Vec<Notification>, Response> {
    let (kind, _anchor) = parse_group_key(raw).ok_or_else(|| ApiError::not_found().into_response())?;
    let rows = scan_notifications(
        store,
        CollectionScan {
            recipient,
            max_id: None,
            since_id: None,
            min_id: None,
            cap: V2_SCAN_ROWS,
        },
    )
    .map_err(|err| err.into_response())?;
    let matching: Vec<Notification> = rows
        .into_iter()
        .filter(|n| n.r#type == kind && group_key(n) == raw)
        .collect();
    if matching.is_empty() {
        return Err(ApiError::not_found().into_response());
    }
    Ok(matching)
}

/// `POST /api/v2/notifications/:group_key/dismiss` — delete every
/// notification in the group (`200 {}`; 404 when the group is empty).
pub fn v2_dismiss(store: &dyn Store, auth: &Auth, raw: &str) -> Response {
    let recipient = match authed(auth, NOTIF_WRITE_SCOPES) {
        Ok(recipient) => recipient,
        Err(response) => return response,
    };
    let matching = match group_lookup(store, recipient, raw) {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    for row in &matching {
        if let Err(err) = store.delete_notification(recipient, row.id) {
            return ApiError::from(err).into_response();
        }
    }
    axum::Json(serde_json::json!({})).into_response()
}

/// `GET /api/v2/notifications/:group_key/accounts` — the group's actors
/// (suspended/gone actors excluded, like the v1 index; 404 when empty).
pub fn v2_group_accounts(
    store: &dyn Store,
    server: &ServerInfo,
    auth: &Auth,
    raw: &str,
) -> Response {
    let recipient = match authed(auth, NOTIF_READ_SCOPES) {
        Ok(recipient) => recipient,
        Err(response) => return response,
    };
    let matching = match group_lookup(store, recipient, raw) {
        Ok(rows) => rows,
        Err(response) => return response,
    };
    let mut ordered: Vec<Id> = Vec::new();
    for row in &matching {
        if let Some(actor) = row.actor_id {
            if !ordered.contains(&actor) {
                ordered.push(actor);
            }
        }
    }
    let accounts = match store.accounts_by_ids(&ordered) {
        Ok(accounts) => accounts,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let stats = match store.account_stats_by_ids(&ordered) {
        Ok(stats) => stats,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let body: Vec<Value> = ordered
        .iter()
        .filter_map(|id| accounts.get(id))
        .filter(|account| !account.suspended)
        .map(|account| dto::account_json_with_stats(server, account, stats.get(&account.id)))
        .collect();
    axum::Json(body).into_response()
}

/// `GET /api/v2/notifications/policy` — same shape as the v1 policy singleton
/// (`filter_*` flags + pending summary; no separate v2 policy row exists).
pub fn v2_policy_show(store: &dyn Store, auth: &Auth) -> Response {
    let recipient = match authed(auth, NOTIF_READ_SCOPES) {
        Ok(recipient) => recipient,
        Err(response) => return response,
    };
    let policy = match notifications::policy(store, recipient) {
        Ok(policy) => policy,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let requests = match store.notification_requests(recipient) {
        Ok(requests) => requests,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut pending_notifications = 0u64;
    for row in &requests {
        pending_notifications += store
            .count_filtered_notifications(recipient, row.from_account_id)
            .unwrap_or(0);
    }
    axum::Json(serde_json::json!({
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
    .into_response()
}

// ---------------------------------------------------------------------------
// Axum adapters (parent mount; thin extractor wrappers over the fns above).
// Both store handles come from the same `Arc<dyn Store>` (`CollectionStore`
// joined `Store` at the parent merge).
// ---------------------------------------------------------------------------

use axum::extract::{Path, RawQuery, State};
use crate::api::forms::FormBody;
use crate::api::mastodon::ApiState;

fn collection_handles(state: &ApiState) -> (&dyn Store, &dyn CollectionStore) {
    (&*state.context.store, &*state.context.store)
}

pub async fn route_collections_index(State(state): State<ApiState>, auth: Auth) -> Response {
    let (_, collections) = collection_handles(&state);
    collections_index(collections, &auth)
}

pub async fn route_collections_create(
    State(state): State<ApiState>,
    auth: Auth,
    FormBody(pairs): FormBody,
) -> Response {
    let (_, collections) = collection_handles(&state);
    collections_create(collections, &auth, &pairs, state.context.now_ms())
}

pub async fn route_collections_show(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
) -> Response {
    let (_, collections) = collection_handles(&state);
    collections_show(collections, &auth, &raw)
}

pub async fn route_collections_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    FormBody(pairs): FormBody,
) -> Response {
    let (_, collections) = collection_handles(&state);
    collections_update(collections, &auth, &raw, &pairs, state.context.now_ms())
}

pub async fn route_collections_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    let (_, collections) = collection_handles(&state);
    collections_destroy(collections, &auth, &raw, state.context.now_ms())
}

pub async fn route_items_index(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let (store, collections) = collection_handles(&state);
    let ctx = CollectionCtx {
        store,
        collections,
        server: &state.server,
        now_ms: state.context.now_ms(),
    };
    items_index(&ctx, &auth, &raw, query.as_deref())
}

pub async fn route_items_add(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    FormBody(pairs): FormBody,
) -> Response {
    let (store, collections) = collection_handles(&state);
    let ctx = CollectionCtx {
        store,
        collections,
        server: &state.server,
        now_ms: state.context.now_ms(),
    };
    items_add(&ctx, &auth, &raw, &pairs)
}

pub async fn route_items_remove(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    FormBody(pairs): FormBody,
) -> Response {
    let (_, collections) = collection_handles(&state);
    items_remove(collections, &auth, &raw, &pairs, state.context.now_ms())
}

pub async fn route_collections_revoke(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    let (_, collections) = collection_handles(&state);
    collections_revoke(collections, &auth, &raw, state.context.now_ms())
}

pub async fn route_in_collections(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    let (_, collections) = collection_handles(&state);
    in_collections(collections, &auth, &raw)
}

pub async fn route_account_collections(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
) -> Response {
    let (store, collections) = collection_handles(&state);
    account_collections(store, collections, &auth, &raw)
}

pub async fn route_account_lists(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    let (store, _) = collection_handles(&state);
    account_lists(store, &auth, &raw)
}

pub async fn route_domain_blocks_preview(
    State(state): State<ApiState>,
    auth: Auth,
    FormBody(pairs): FormBody,
) -> Response {
    let (store, _) = collection_handles(&state);
    domain_blocks_preview(store, &auth, &pairs)
}

pub async fn route_peers_search(
    State(state): State<ApiState>,
    RawQuery(query): RawQuery,
) -> Response {
    let (store, _) = collection_handles(&state);
    peers_search(
        store,
        state.server.peers_api_enabled,
        query.as_deref(),
    )
}

pub async fn route_alpha_collections_index(
    State(state): State<ApiState>,
    auth: Auth,
) -> Response {
    let (_, collections) = collection_handles(&state);
    alpha_collections_index(collections, &auth)
}

pub async fn route_v2_index(
    State(state): State<ApiState>,
    auth: Auth,
    RawQuery(query): RawQuery,
) -> Response {
    let (store, _) = collection_handles(&state);
    v2_index(store, &auth, query.as_deref())
}

pub async fn route_v2_clear(State(state): State<ApiState>, auth: Auth) -> Response {
    let (store, _) = collection_handles(&state);
    v2_clear(store, &auth)
}

pub async fn route_v2_unread_count(
    State(state): State<ApiState>,
    auth: Auth,
    RawQuery(query): RawQuery,
) -> Response {
    let (store, _) = collection_handles(&state);
    v2_unread_count(store, &auth, query.as_deref())
}

pub async fn route_v2_dismiss(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    let (store, _) = collection_handles(&state);
    v2_dismiss(store, &auth, &raw)
}

pub async fn route_v2_group_accounts(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    let (store, _) = collection_handles(&state);
    v2_group_accounts(store, &state.server, &auth, &raw)
}

pub async fn route_v2_policy_show(State(state): State<ApiState>, auth: Auth) -> Response {
    let (store, _) = collection_handles(&state);
    v2_policy_show(store, &auth)
}

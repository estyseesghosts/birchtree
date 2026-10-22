//! Announcements: reads, dismissals, reactions and admin authoring.
//!
//! Upstream pin (`8ff11c2b`) cites live in `application/announcements.rs`
//! (durable `redb` backing, schema v14).
//!
//! Public behavior at the pin:
//! - `GET /api/v1/announcements` — `require_user!` only (no scope check),
//!   published announcements chronological
//!   (`announcements_controller.rb:8-10`).
//! - `POST .../dismiss` — `write`/`write:accounts`, `200 {}`
//!   (`announcements_controller.rb:5,12-15`; `render_empty` is `200 {}`, per
//!   `api/base_controller.rb:86-88`).
//! - `PUT`/`DELETE .../reactions/:id` — `write`/`write:favourites`, `200 {}`
//!   (`reactions_controller.rb:4,11-21`); no feature flag gates either action
//!   at the pin (verified by grep). 404 when the announcement is unpublished
//!   or the reaction is missing (`published` scope / `find_by!`).
//! - Serializer (`announcement_serializer.rb`): `id, content, starts_at,
//!   ends_at, all_day, published_at, updated_at, read`, `mentions[{id,
//!   username, url, acct}]`, `statuses`, `tags[{name, url}]` (`ShallowTag` =
//!   name+url only — note: NO `published` field), `emojis`,
//!   `reactions[{name, count, me?, url?, static_url?}]`.
//! - Admin authoring has no REST equivalent upstream (HTML controller only);
//!   role needs `manage_announcements`, which is administration-only
//!   (Admin/Owner; Moderator lacks it — `config/roles.yml`). The
//!   `admin_*` handlers below are new API surface gated on
//!   `AdminArea::AdminOnly`, mounted in `router.rs`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::api::mastodon::auth::{AdminArea, Auth};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::{ApiState, ServerInfo};
use crate::application::announcements as uc;
use crate::application::ports::Store;
use crate::domain::{Account, CustomEmoji, Id};

// ---------------------------------------------------------------------------
// Durable backing: `AnnouncementStore` is a `Store` supertrait implemented by
// `redb` (schema v14). One transaction commits record + index + mute/reaction
// rows; no fan-out jobs attach.
// ---------------------------------------------------------------------------

fn db_for(store: &Arc<dyn Store>) -> &dyn uc::AnnouncementStore {
    &**store
}

// ---------------------------------------------------------------------------
// Public reads.
// ---------------------------------------------------------------------------

const DISMISS_SCOPES: &[&str] = &["write", "write:accounts"];
const REACTION_SCOPES: &[&str] = &["write", "write:favourites"];

fn parse_id(raw: &str) -> Result<Id, ApiError> {
    raw.parse::<Id>().map_err(|_| ApiError::not_found())
}

/// `GET /api/v1/announcements`: published announcements, chronological.
/// Upstream applies `require_user!` with no scope check.
pub async fn index(State(state): State<ApiState>, auth: Auth) -> Response {
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let db = db_for(&state.context.store);
    let store = &*state.context.store;
    let rows = match uc::list_published(&*db) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match render_announcement(&state.server, store, db, &row, viewer).await {
            Ok(value) => out.push(value),
            Err(err) => return err.into_response(),
        }
    }
    axum::Json(out).into_response()
}

/// `POST /api/v1/announcements/:id/dismiss`: `200 {}` + mute (404 unless
/// published).
pub async fn dismiss(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(DISMISS_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let id = match parse_id(&raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let db = db_for(&state.context.store);
    match uc::dismiss(&*db, id, viewer, state.context.now_ms()) {
        Ok(_) => (StatusCode::OK, axum::Json(json!({}))).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT /api/v1/announcements/:announcement_id/reactions/:id`
/// (`PATCH` alias): `200 {}` (404 unless published; 422 on invalid emoji).
pub async fn reaction_put(
    State(state): State<ApiState>,
    auth: Auth,
    Path((announcement_raw, name)): Path<(String, String)>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(REACTION_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let announcement_id = match parse_id(&announcement_raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    react(&state, viewer, announcement_id, &name, true).await
}

/// `DELETE /api/v1/announcements/:announcement_id/reactions/:id`: `200 {}`
/// (404 when unpublished or when the viewer never reacted with `name`).
pub async fn reaction_delete(
    State(state): State<ApiState>,
    auth: Auth,
    Path((announcement_raw, name)): Path<(String, String)>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(REACTION_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let announcement_id = match parse_id(&announcement_raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    react(&state, viewer, announcement_id, &name, false).await
}

async fn react(
    state: &ApiState,
    viewer: Id,
    announcement_id: Id,
    name: &str,
    put: bool,
) -> Response {
    let db = db_for(&state.context.store);
    if put {
        let custom = match resolve_custom_emoji(&*state.context.store, name) {
            Ok(custom) => custom,
            Err(err) => return err.into_response(),
        };
        match uc::add_reaction(
            &*db,
            announcement_id,
            viewer,
            name,
            custom.as_deref(),
            state.context.now_ms(),
            state.context.next_id(),
        ) {
            Ok(_) => {
                // Announcement streaming (gaps G-B): reactions ride the
                // `announcement.reaction` event on `user` streams.
                state.streams.publish_announcement(
                    crate::streaming::hub::AnnouncementEvent::Reaction {
                        announcement_id,
                        name: name.to_string(),
                    },
                );
                (StatusCode::OK, axum::Json(json!({}))).into_response()
            }
            Err(err) => ApiError::from(err).into_response(),
        }
    } else {
        match uc::remove_reaction(&*db, announcement_id, viewer, name) {
            Ok(_) => {
                // Announcement streaming (gaps G-B): reactions ride the
                // `announcement.reaction` event on `user` streams.
                state.streams.publish_announcement(
                    crate::streaming::hub::AnnouncementEvent::Reaction {
                        announcement_id,
                        name: name.to_string(),
                    },
                );
                (StatusCode::OK, axum::Json(json!({}))).into_response()
            }
            Err(err) => ApiError::from(err).into_response(),
        }
    }
}

/// The local enabled custom emoji behind `name`, if any (mirrors
/// `AnnouncementReaction#set_custom_emoji`: `CustomEmoji.local.enabled`).
/// Storage failures surface as 500; absence is `None` (unicode path).
fn resolve_custom_emoji(store: &dyn Store, name: &str) -> Result<Option<String>, ApiError> {
    if !uc_is_shortcode_shaped(name) {
        return Ok(None);
    }
    match store.emoji_by_shortcode(name) {
        Ok(Some(emoji)) if emoji.listed() => Ok(Some(emoji.shortcode)),
        Ok(_) => Ok(None),
        Err(err) => Err(ApiError::from(err)),
    }
}

fn uc_is_shortcode_shaped(name: &str) -> bool {
    let len = name.chars().count();
    (2..=128).contains(&len) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Admin authoring (new API surface; mounted by the parent — see snippet in
// `application/announcements.rs`). Upstream has no REST equivalent (HTML
// controller only); the role gate mirrors `manage_announcements`
// (administration-only).
// ---------------------------------------------------------------------------

const ADMIN_READ_SCOPES: &[&str] = &["admin:read"];
const ADMIN_WRITE_SCOPES: &[&str] = &["admin:write"];

fn require_admin(state: &ApiState, auth: &Auth, scopes: &[&str]) -> Result<(), Response> {
    auth.0
        .require_admin(&*state.context.store, AdminArea::AdminOnly, scopes)
        .map(|_| ())
        .map_err(|err| err.into_response())
}

/// `GET /api/v1/admin/announcements`: every row, newest chronology first
/// (mirrors upstream admin `reverse_chronological`).
pub async fn admin_index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(resp) = require_admin(&state, &auth, ADMIN_READ_SCOPES) {
        return resp;
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let db = db_for(&state.context.store);
    let store = &*state.context.store;
    let rows = match uc::list_all(&*db) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match render_announcement(&state.server, store, db, &row, viewer).await {
            Ok(value) => out.push(value),
            Err(err) => return err.into_response(),
        }
    }
    axum::Json(out).into_response()
}

/// `GET /api/v1/admin/announcements/:id` (404 when missing, drafts included).
pub async fn admin_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(resp) = require_admin(&state, &auth, ADMIN_READ_SCOPES) {
        return resp;
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let id = match parse_id(&raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let db = db_for(&state.context.store);
    match db.announcement_by_id(id) {
        Ok(Some(row)) => {
            match render_announcement(&state.server, &*state.context.store, db, &row, viewer).await
            {
                Ok(value) => axum::Json(value).into_response(),
                Err(err) => err.into_response(),
            }
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// JSON body for admin create/update (timestamps are ISO-8601 or null).
#[derive(Debug, serde::Deserialize, Default)]
pub struct AnnouncementInput {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub all_day: Option<bool>,
    #[serde(default, deserialize_with = "opt_iso_ms")]
    pub starts_at: Option<Option<u64>>,
    #[serde(default, deserialize_with = "opt_iso_ms")]
    pub ends_at: Option<Option<u64>>,
    #[serde(default, deserialize_with = "opt_iso_ms")]
    pub scheduled_at: Option<Option<u64>>,
}

fn opt_iso_ms<'de, D>(d: D) -> Result<Option<Option<u64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Field absent -> `None` (unchanged); explicit `null`/"" -> `Some(None)`
    // (clear); otherwise an ISO-8601 timestamp.
    let raw: Option<Value> = Option::deserialize(d)?;
    match raw {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(Some(None)),
        Some(Value::String(s)) => parse_iso_ms(&s)
            .map(|ms| Some(Some(ms)))
            .map_err(serde::de::Error::custom),
        Some(_) => Err(serde::de::Error::custom(
            "must be an ISO-8601 timestamp or null",
        )),
    }
}

fn parse_iso_ms(s: &str) -> Result<u64, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp_millis().max(0) as u64)
        .map_err(|e| format!("invalid timestamp: {e}"))
}

/// `POST /api/v1/admin/announcements`: 200 + the row (published immediately
/// unless `scheduled_at` is in the future — upstream `set_published`).
pub async fn admin_create(
    State(state): State<ApiState>,
    auth: Auth,
    axum::Json(input): axum::Json<AnnouncementInput>,
) -> Response {
    if let Err(resp) = require_admin(&state, &auth, ADMIN_WRITE_SCOPES) {
        return resp;
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let text = match input.text {
        Some(text) => text,
        None => return ApiError::unprocessable("Text can't be blank").into_response(),
    };
    let now_ms = state.context.now_ms();
    let db = db_for(&state.context.store);
    let row = match uc::create_announcement(
        &*db,
        uc::NewAnnouncement {
            text,
            all_day: input.all_day.unwrap_or(false),
            starts_at_ms: input.starts_at.unwrap_or(None),
            ends_at_ms: input.ends_at.unwrap_or(None),
            scheduled_at_ms: input.scheduled_at.unwrap_or(None),
        },
        now_ms,
        state.context.next_id(),
    ) {
        Ok(row) => {
            // Announcement streaming (gaps G-B): a published row rides the
            // `announcement` event on `user` streams (scheduled rows publish
            // on their explicit publish instead).
            if row.published {
                state.streams.publish_announcement(
                    crate::streaming::hub::AnnouncementEvent::Published {
                        announcement_id: row.id,
                    },
                );
            }
            row
        }
        Err(err) => return ApiError::from(err).into_response(),
    };
    match render_announcement(&state.server, &*state.context.store, db, &row, viewer).await {
        Ok(value) => axum::Json(value).into_response(),
        Err(err) => err.into_response(),
    }
}

/// `PUT`/`PATCH /api/v1/admin/announcements/:id`: 200 + the row (never flips
/// `published` by itself — use publish/unpublish).
pub async fn admin_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    axum::Json(input): axum::Json<AnnouncementInput>,
) -> Response {
    if let Err(resp) = require_admin(&state, &auth, ADMIN_WRITE_SCOPES) {
        return resp;
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let id = match parse_id(&raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let db = db_for(&state.context.store);
    let row = match uc::update_announcement(
        &*db,
        id,
        uc::AnnouncementPatch {
            text: input.text,
            all_day: input.all_day,
            starts_at_ms: input.starts_at,
            ends_at_ms: input.ends_at,
            scheduled_at_ms: input.scheduled_at,
        },
        state.context.now_ms(),
    ) {
        Ok(row) => row,
        Err(err) => return ApiError::from(err).into_response(),
    };
    match render_announcement(&state.server, &*state.context.store, db, &row, viewer).await {
        Ok(value) => axum::Json(value).into_response(),
        Err(err) => err.into_response(),
    }
}

/// `DELETE /api/v1/admin/announcements/:id`: `200 {}`.
pub async fn admin_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(resp) = require_admin(&state, &auth, ADMIN_WRITE_SCOPES) {
        return resp;
    }
    let id = match parse_id(&raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let db = db_for(&state.context.store);
    match uc::destroy_announcement(&*db, id) {
        Ok(_) => {
            // Announcement streaming (gaps G-B): destroys ride
            // `announcement.delete` on `user` streams.
            state
                .streams
                .publish_announcement(crate::streaming::hub::AnnouncementEvent::Deleted {
                    announcement_id: id,
                });
            (StatusCode::OK, axum::Json(json!({}))).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/admin/announcements/:id/publish`: 200 + the row.
pub async fn admin_publish(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(resp) = require_admin(&state, &auth, ADMIN_WRITE_SCOPES) {
        return resp;
    }
    admin_flip(&state, &auth, &raw, true).await
}

/// `POST /api/v1/admin/announcements/:id/unpublish`: 200 + the row.
pub async fn admin_unpublish(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(resp) = require_admin(&state, &auth, ADMIN_WRITE_SCOPES) {
        return resp;
    }
    admin_flip(&state, &auth, &raw, false).await
}

async fn admin_flip(state: &ApiState, auth: &Auth, raw: &str, publish: bool) -> Response {
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let id = match parse_id(raw) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let db = db_for(&state.context.store);
    let now_ms = state.context.now_ms();
    let row = if publish {
        uc::publish_announcement(&*db, id, now_ms)
    } else {
        uc::unpublish_announcement(&*db, id, now_ms)
    };
    match row {
        Ok(row) => {
            // Announcement streaming (gaps G-B): explicit publish/unpublish
            // ride `announcement` / `announcement.delete` on `user` streams.
            // (Plain `admin_update` publishes nothing: no upstream broadcast
            // was found for edits.)
            if publish {
                state.streams.publish_announcement(
                    crate::streaming::hub::AnnouncementEvent::Published {
                        announcement_id: row.id,
                    },
                );
            } else {
                state.streams.publish_announcement(
                    crate::streaming::hub::AnnouncementEvent::Deleted {
                        announcement_id: row.id,
                    },
                );
            }
            match render_announcement(&state.server, &*state.context.store, db, &row, viewer).await
            {
                Ok(value) => axum::Json(value).into_response(),
                Err(err) => err.into_response(),
            }
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Serializer.
// ---------------------------------------------------------------------------

/// `REST::AnnouncementSerializer` at the pin (`announcement_serializer.rb`).
/// Divergences: `content` is HTML-escaped text rather than upstream's full
/// `linkify` pass; `statuses` is always `[]` (the upstream admin form carries
/// no `status_ids`, so authored rows have none).
async fn render_announcement(
    server: &ServerInfo,
    store: &dyn Store,
    db: &dyn uc::AnnouncementStore,
    row: &uc::AnnouncementRecord,
    viewer: Id,
) -> Result<Value, ApiError> {
    let read = db
        .announcement_muted(row.id, viewer)
        .map_err(ApiError::from)?;
    let mentions = resolve_mentions(store, server, &row.text).map_err(ApiError::from)?;
    let tags = extract_tags(&row.text)
        .into_iter()
        .map(|tag| {
            json!({
                "name": tag,
                "url": server.asset_url(&format!("tags/{tag}")),
            })
        })
        .collect::<Vec<_>>();
    let emojis = resolve_emojis(store, server, &row.text).map_err(ApiError::from)?;
    let reactions = uc::reaction_views(db, row.id, viewer).map_err(ApiError::from)?;
    // Reaction URLs follow the emoji ROW's keys (a replaced emoji commits
    // fresh keys; the canonical shortcode path would 404). One batched
    // lookup covers every custom reaction.
    let reaction_shortcodes: Vec<String> = reactions
        .iter()
        .filter_map(|r| r.custom_emoji_shortcode.clone())
        .collect();
    let reaction_emoji: std::collections::HashMap<String, CustomEmoji> = store
        .emojis_by_shortcodes(&reaction_shortcodes)
        .map_err(ApiError::from)?
        .into_iter()
        .map(|emoji| (emoji.shortcode.clone(), emoji))
        .collect();
    let reactions = reactions
        .into_iter()
        .map(|r| reaction_json(server, &r, reaction_emoji.get(r.custom_emoji_shortcode.as_deref().unwrap_or_default())))
        .collect::<Vec<_>>();
    let mut object = Map::new();
    object.insert("id".into(), json!(row.id.to_string()));
    object.insert("content".into(), json!(escape_html(&row.text)));
    object.insert("starts_at".into(), opt_time(row.starts_at_ms));
    object.insert("ends_at".into(), opt_time(row.ends_at_ms));
    object.insert("all_day".into(), json!(row.all_day));
    object.insert("published_at".into(), opt_time(row.published_at_ms));
    object.insert("updated_at".into(), json!(iso_ms(row.updated_at_ms)));
    object.insert("read".into(), json!(read));
    object.insert(
        "mentions".into(),
        mentions
            .into_iter()
            .map(|a: Account| {
                json!({
                    "id": a.id.to_string(),
                    "username": a.username,
                    "url": a.actor_uri,
                    "acct": a.acct(),
                })
            })
            .collect::<Vec<_>>()
            .into(),
    );
    object.insert("statuses".into(), json!([]));
    object.insert("tags".into(), tags.into());
    object.insert(
        "emojis".into(),
        emojis
            .into_iter()
            .map(|e: CustomEmoji| dto::custom_emoji_json(server, &e, false))
            .collect::<Vec<_>>()
            .into(),
    );
    object.insert("reactions".into(), reactions.into());
    Ok(Value::Object(object))
}

/// `REST::ReactionSerializer` at the pin (`reaction_serializer.rb`): `name,
/// count`, `me` (signed in — always here), `url`/`static_url` for custom
/// emoji only. URLs follow the emoji row's keys when the row is known (a
/// replaced emoji commits fresh keys), falling back to the canonical
/// shortcode path for rows that predate the lookup (fresh rows keep the
/// canonical keys, so the fallback renders identically).
fn reaction_json(
    server: &ServerInfo,
    r: &uc::ReactionView,
    emoji: Option<&CustomEmoji>,
) -> Value {
    let mut object = Map::new();
    object.insert("name".into(), json!(r.name));
    object.insert("count".into(), json!(r.count));
    object.insert("me".into(), json!(r.me));
    match &r.custom_emoji_shortcode {
        Some(shortcode) => {
            let (url, static_url) = match emoji {
                Some(row) => (
                    server.asset_url(&format!("system/{}", row.image_key)),
                    server.asset_url(&format!("system/{}", row.static_image_key)),
                ),
                None => {
                    let partition = crate::application::emoji::emoji_partition(shortcode);
                    (
                        server.asset_url(&format!(
                            "system/custom_emojis/images/{partition}/original/{shortcode}.png"
                        )),
                        server.asset_url(&format!(
                            "system/custom_emojis/images/{partition}/static/{shortcode}.png"
                        )),
                    )
                }
            };
            object.insert("url".into(), json!(url));
            object.insert("static_url".into(), json!(static_url));
        }
        None => {
            object.insert("url".into(), Value::Null);
            object.insert("static_url".into(), Value::Null);
        }
    }
    Value::Object(object)
}

/// Upstream `Account.from_text`: resolve `@mention` tokens to accounts.
/// Local handles resolve via the handle index; unknown tokens are skipped.
fn resolve_mentions(
    store: &dyn Store,
    server: &ServerInfo,
    text: &str,
) -> crate::domain::Result<Vec<Account>> {
    let mut seen: Vec<String> = Vec::new();
    for handle in scan_mentions(text) {
        if !seen.iter().any(|h| h == &handle) {
            seen.push(handle);
        }
    }
    let mut out = Vec::new();
    for handle in seen {
        let (user, domain) = match handle.split_once('@') {
            Some((u, d)) => (u.to_string(), d.to_lowercase()),
            None => (handle.clone(), server.domain.clone()),
        };
        if domain != server.domain {
            continue;
        }
        if let Some(account) = store.account_by_handle(&user.to_lowercase(), &server.domain)? {
            out.push(account);
        }
    }
    Ok(out)
}

fn scan_mentions(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'.')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'@' {
                let mut k = j + 1;
                while k < bytes.len()
                    && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'-' || bytes[k] == b'.')
                {
                    k += 1;
                }
                if k > j + 1 {
                    out.push(text[i + 1..k].to_string());
                    i = k;
                    continue;
                }
            }
            if j > i + 1 {
                out.push(text[i + 1..j].to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Upstream `Extractor.extract_hashtags`: `#tag` tokens, normalized lowercase.
fn extract_tags(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > i + 1 {
                let tag = text[i + 1..j].to_lowercase();
                if !out.iter().any(|t| t == &tag) {
                    out.push(tag);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Upstream `CustomEmoji.from_text`: `:shortcode:` tokens resolved to local
/// enabled emoji; unknown shortcodes are skipped.
fn resolve_emojis(
    store: &dyn Store,
    _server: &ServerInfo,
    text: &str,
) -> crate::domain::Result<Vec<CustomEmoji>> {
    let mut shortcodes: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' && j > i + 1 {
                let code = text[i + 1..j].to_string();
                if !shortcodes.iter().any(|c| c == &code) {
                    shortcodes.push(code);
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    let mut out = Vec::new();
    for code in shortcodes {
        if let Some(emoji) = store.emoji_by_shortcode(&code)? {
            if emoji.listed() {
                out.push(emoji);
            }
        }
    }
    Ok(out)
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn iso_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

fn opt_time(ms: Option<u64>) -> Value {
    ms.map(|t| json!(iso_ms(t))).unwrap_or(Value::Null)
}

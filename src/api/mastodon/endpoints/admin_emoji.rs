//! Admin custom-emoji authoring (`/api/v1/admin/custom_emojis*`).
//!
//! There is no upstream REST equivalent in Mastodon v4.7.3 (pin `8ff11c2b`):
//! `config/routes/api.rb:78` exposes only `custom_emojis#index` on the public
//! API, and emoji authoring upstream is the HTML
//! `Admin::CustomEmojisController` (index/new/create plus an enable/disable/
//! list/unlist/copy/delete batch form). Like the announcements admin surface,
//! this module is new API surface gated admins-only: `manage_custom_emojis`
//! is administration-only in upstream `config/roles.yml` (moderators lack
//! it), so the gate uses [`AdminArea::AdminOnly`].
//!
//! Auth mirrors `admin_reports` (role-before-scope): app-only tokens get
//! `422`, role-less users `403 {"error":"Forbidden"}`, and out-of-scope
//! tokens `403` with the scopes message. Scopes reuse the bare
//! `admin:read`/`admin:write` — upstream `doorkeeper.rb` defines no
//! custom-emoji sub-scopes (same choice as the announcements admin module;
//! no scope-registry additions needed).
//!
//! Bodies: `POST` takes `multipart/form-data` (`shortcode`, `image`,
//! optional `category`, optional `visible_in_picker`), following the media
//! endpoint's multipart precedent (`endpoints/media.rs`) and upstream's
//! Paperclip file upload — not base64 JSON — so operator tooling can reuse
//! the same upload path. `PUT` takes JSON (`category` string-or-null,
//! `visible_in_picker`/`disabled` booleans).
//!
//! Validation reuses the [`CustomEmoji`] rules (`shortcode_valid`,
//! `IMAGE_LIMIT`, `is_emoji_image` → `422`, never new checks). Rows render
//! with [`dto::custom_emoji_json`] plus a `disabled` flag (the shared
//! serializer has no disabled field, and an admin client cannot otherwise
//! tell a disabled row from an enabled one).

use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::mastodon::auth::{AdminArea, Auth, Authenticated};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::ApiState;
use crate::application::emoji::{self, EmojiFlags};
use crate::application::ports::{AccountStore, Store};
use crate::domain::Id;

const READ_SCOPES: &[&str] = &["admin:read"];
const WRITE_SCOPES: &[&str] = &["admin:write"];

/// Role-before-scope gate for every route in this module (same shape as
/// `admin_reports::gate`): app-only tokens `422`, role-less users
/// `403 {"error":"Forbidden"}`, out-of-scope tokens `403`. Returns the actor.
fn gate(
    auth: &Authenticated,
    store: &dyn Store,
    scopes: &[&str],
) -> ApiResult<Id> {
    let store: &dyn AccountStore = store;
    let _role = auth.require_admin(store, AdminArea::AdminOnly, scopes)?;
    let viewer = auth.require_user()?;
    Ok(viewer)
}

/// Admin row JSON: the shared serializer's full fields plus `disabled` (see
/// module docs; `dto.rs` is parent-owned so the flag is added here).
fn emoji_json(server: &crate::api::mastodon::ServerInfo, row: &crate::domain::CustomEmoji) -> Value {
    let mut value = dto::custom_emoji_json(server, row, true);
    if let Value::Object(ref mut map) = value {
        map.insert("disabled".to_string(), Value::Bool(row.disabled));
    }
    value
}

/// `GET /api/v1/admin/custom_emojis`: every local emoji, shortcode order —
/// including disabled and picker-hidden rows (unlike the public index).
pub async fn index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = gate(&auth.0, &*state.context.store, READ_SCOPES) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    match store.all_emojis(1_000) {
        Ok(rows) => {
            let body: Vec<Value> = rows
                .iter()
                .map(|row| emoji_json(&state.server, row))
                .collect();
            axum::Json(body).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/admin/custom_emojis`: multipart `shortcode` + `image`
/// (+ optional `category`, `visible_in_picker`) → `200` + the row.
/// Re-posting a shortcode replaces image bytes and metadata (CLI parity).
pub async fn create(
    State(state): State<ApiState>,
    auth: Auth,
    request: Request,
) -> Response {
    if let Err(err) = gate(&auth.0, &*state.context.store, WRITE_SCOPES) {
        return err.into_response();
    }
    let fields = match multipart_fields(&state, request).await {
        Ok(fields) => fields,
        Err(err) => return err.into_response(),
    };
    let shortcode = match fields.shortcode {
        Some(raw) if !raw.trim().is_empty() => raw.trim().to_string(),
        _ => return ApiError::unprocessable("Shortcode can't be blank").into_response(),
    };
    match emoji::add_emoji(
        &*state.context.store,
        &*state.context.blobs,
        &shortcode,
        &fields.image,
        fields.category.as_deref(),
        fields.visible_in_picker,
    ) {
        Ok(row) => (StatusCode::OK, axum::Json(emoji_json(&state.server, &row))).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT /api/v1/admin/custom_emojis/:shortcode`: JSON `category`
/// (string-or-null; `""` clears like `null`), `visible_in_picker` /
/// `disabled` booleans → `200` + the row, `404` when missing.
pub async fn update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(shortcode): Path<String>,
    axum::Json(input): axum::Json<Value>,
) -> Response {
    if let Err(err) = gate(&auth.0, &*state.context.store, WRITE_SCOPES) {
        return err.into_response();
    }
    let Some(obj) = input.as_object() else {
        return ApiError::unprocessable("Expected a JSON object").into_response();
    };
    let mut patch = EmojiFlags::default();
    let mut touched = false;
    if let Some(raw) = obj.get("category") {
        touched = true;
        match raw {
            Value::Null => patch.category = Some(None),
            Value::String(text) => patch.category = Some(Some(text.clone())),
            _ => {
                return ApiError::unprocessable("Category must be a string or null")
                    .into_response();
            }
        }
    }
    if let Some(raw) = obj.get("visible_in_picker") {
        touched = true;
        match raw.as_bool() {
            Some(flag) => patch.visible_in_picker = Some(flag),
            None => {
                return ApiError::unprocessable("Visible in picker must be true or false")
                    .into_response();
            }
        }
    }
    if let Some(raw) = obj.get("disabled") {
        touched = true;
        match raw.as_bool() {
            Some(flag) => patch.disabled = Some(flag),
            None => {
                return ApiError::unprocessable("Disabled must be true or false").into_response();
            }
        }
    }
    if !touched {
        return ApiError::unprocessable("No updatable fields provided").into_response();
    }
    let store: &dyn Store = &*state.context.store;
    match emoji::set_emoji_flags(store, &shortcode, patch) {
        Ok(row) => (StatusCode::OK, axum::Json(emoji_json(&state.server, &row))).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/admin/custom_emojis/:shortcode`: remove the row and its
/// image bytes → `200 {}`; `404` when missing.
pub async fn destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(shortcode): Path<String>,
) -> Response {
    if let Err(err) = gate(&auth.0, &*state.context.store, WRITE_SCOPES) {
        return err.into_response();
    }
    match emoji::rm_emoji(&*state.context.store, &*state.context.blobs, &shortcode) {
        Ok(true) => axum::Json(json!({})).into_response(),
        Ok(false) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Multipart plumbing (mirrors `endpoints/media.rs`; the declared file
// content type is never trusted — magic bytes decide).
// ---------------------------------------------------------------------------

/// Parsed create fields; absent parts stay at their defaults and a
/// non-multipart body yields the blanks (→ the shortcode 422, like media's
/// blank-file 422).
#[derive(Debug, Default)]
struct CreateFields {
    shortcode: Option<String>,
    image: Vec<u8>,
    category: Option<String>,
    visible_in_picker: bool,
}

async fn multipart_fields(state: &ApiState, request: Request) -> ApiResult<CreateFields> {
    let mut fields = CreateFields {
        visible_in_picker: true,
        ..CreateFields::default()
    };
    if !is_multipart(&request) {
        return Ok(fields);
    }
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?
    {
        let name = field.name().map(str::to_string);
        let value = field
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?;
        match name.as_deref() {
            Some("shortcode") => {
                fields.shortcode = Some(String::from_utf8_lossy(&value).into_owned());
            }
            Some("image") => fields.image = value,
            Some("category") => {
                fields.category = Some(String::from_utf8_lossy(&value).into_owned());
            }
            Some("visible_in_picker") => {
                let raw = String::from_utf8_lossy(&value);
                match parse_bool(&raw) {
                    Some(flag) => fields.visible_in_picker = flag,
                    None => {
                        return Err(ApiError::unprocessable(
                            "Visible in picker must be true or false",
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(fields)
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn is_multipart(request: &Request) -> bool {
    request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"))
}

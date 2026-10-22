// ---------------------------------------------------------------------------
// Tags.
// ---------------------------------------------------------------------------

use axum::Json;
use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::ApiError;
use crate::application::ports::{AdminTagPatch, Store};
use crate::domain::AdminTag;

use super::{audit, bool_cast, paged, require_admin, require_id};

fn admin_tag_json(server_public_url: &str, row: &AdminTag) -> Value {
    let base = server_public_url.trim_end_matches('/');
    json!({
        "id": row.id.to_string(),
        "name": row.name,
        "url": format!("{base}/tags/{}", row.name),
        "history": [],
        "following": false,
        "featuring": false,
        "display_name": row.display_name,
        "trendable": row.trendable.map(Value::from).unwrap_or(Value::Null),
        "usable": row.usable.map(Value::from).unwrap_or(Value::Null),
        "requires_review": row.reviewed_at_ms.is_none(),
        "listable": row.listable.map(Value::from).unwrap_or(Value::Null),
    })
}

/// `GET /api/v1/admin/tags`: every real hashtag gets a registry row
/// (materialized from the hashtag index on read — never fabricated), then the
/// registry pages newest first.
pub async fn tags_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let names = match store.admin_hashtag_names(1_000) {
        Ok(names) => names,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let now = state.context.now_ms();
    for name in &names {
        if let Err(err) = store.admin_ensure_tag(name, now) {
            return ApiError::from(err).into_response();
        }
    }
    let server = state.server.public_url.clone();
    paged(
        &uri,
        100,
        200,
        |max, since, min, limit| store.admin_tags_page(max, since, min, limit),
        |row| admin_tag_json(&server, row),
        |row| row.id,
    )
}

pub async fn tags_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = require_admin(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_tag_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(admin_tag_json(&state.server.public_url, &row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

/// `PUT|PATCH /api/v1/admin/tags/:id`: `display_name`, `listable`,
/// `trendable`, `usable` (upstream's permitted params). Stamps `reviewed_at`.
pub async fn tags_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(&auth.0, &*state.context.store, &["admin:write"]) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let patch = AdminTagPatch {
        display_name: forms::get(&body, "display_name").map(|item| item.to_string()),
        trendable: forms::get(&body, "trendable").map(bool_cast),
        usable: forms::get(&body, "usable").map(bool_cast),
        listable: forms::get(&body, "listable").map(bool_cast),
    };
    let store: &dyn Store = &*state.context.store;
    match store
        .admin_update_tag(id, patch, state.context.now_ms())
        .map_err(ApiError::from)
    {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "update", "Tag", &row.id.to_string()) {
                return err.into_response();
            }
            Json(admin_tag_json(&state.server.public_url, &row)).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

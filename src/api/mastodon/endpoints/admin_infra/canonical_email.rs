// ---------------------------------------------------------------------------
// Canonical email blocks.
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
use crate::application::admin_infra as use_cases;
use crate::application::ports::Store;
use crate::domain::AdminCanonicalEmailBlock;

use super::{audit, paged, require_admin, require_id};

fn canonical_email_block_json(row: &AdminCanonicalEmailBlock) -> Value {
    json!({
        "id": row.id.to_string(),
        "canonical_email_hash": row.canonical_email_hash,
    })
}

pub async fn canonical_email_blocks_index(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:canonical_email_blocks"),
    ) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(
        &uri,
        100,
        200,
        |max, since, min, limit| store.admin_canonical_email_blocks_page(max, since, min, limit),
        canonical_email_block_json,
        |row| row.id,
    )
}

/// `POST /api/v1/admin/canonical_email_blocks/test`: which blocks match an
/// email. Takes the **read** scope upstream (`test` is in the read guard) —
/// kept here.
pub async fn canonical_email_blocks_test(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:canonical_email_blocks"),
    ) {
        return err.into_response();
    }
    let Some(email) = forms::get(&body, "email") else {
        return ApiError::bad_request("param is missing or the value is empty: email")
            .into_response();
    };
    if use_cases::validate_canonical_email(email).is_err() {
        return ApiError::bad_request("param is missing or the value is empty: email")
            .into_response();
    }
    let hash = use_cases::canonical_email_hash(email);
    let store: &dyn Store = &*state.context.store;
    match store
        .admin_canonical_blocks_for_hash(&hash)
        .map_err(ApiError::from)
    {
        Ok(rows) => {
            let body: Vec<Value> = rows.iter().map(canonical_email_block_json).collect();
            Json(body).into_response()
        }
        Err(err) => err.into_response(),
    }
}

pub async fn canonical_email_blocks_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:canonical_email_blocks"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let hash = if let Some(email) = forms::get(&body, "email") {
        if use_cases::validate_canonical_email(email).is_err() {
            return ApiError::unprocessable("Validation failed: Email is invalid").into_response();
        }
        use_cases::canonical_email_hash(email)
    } else if let Some(hash) = forms::get(&body, "canonical_email_hash") {
        let hash = hash.trim().to_string();
        let is_hex = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        if !is_hex {
            return ApiError::unprocessable("Validation failed: Canonical email hash is invalid")
                .into_response();
        }
        hash.to_ascii_lowercase()
    } else {
        return ApiError::unprocessable("Validation failed: Email can't be blank").into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_canonical_email_block(&hash, state.context.now_ms()) {
        Ok(row) => {
            if let Err(err) = audit(
                &state,
                actor,
                "create",
                "CanonicalEmailBlock",
                &row.id.to_string(),
            ) {
                return err.into_response();
            }
            // Stored only: registration-time enforcement needs email
            // confirmation (Phase 2 gap).
            Json(canonical_email_block_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn canonical_email_blocks_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:canonical_email_blocks"),
    ) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store
        .admin_canonical_email_block_by_id(id)
        .map_err(ApiError::from)
    {
        Ok(Some(row)) => Json(canonical_email_block_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn canonical_email_blocks_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:canonical_email_blocks"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store
        .admin_delete_canonical_email_block(id)
        .map_err(ApiError::from)
    {
        Ok(Some(row)) => {
            if let Err(err) = audit(
                &state,
                actor,
                "destroy",
                "CanonicalEmailBlock",
                &row.id.to_string(),
            ) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

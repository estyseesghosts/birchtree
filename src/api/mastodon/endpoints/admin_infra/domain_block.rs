// ---------------------------------------------------------------------------
// Domain blocks.
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
use crate::application::ports::{AdminDomainBlockPatch, Store};
use crate::domain::{AdminDomainBlock, AdminDomainSeverity};

use super::{audit, bool_cast, paged, require_admin, require_id, rfc3339_ms};

fn sha256_hex_domain(domain: &str) -> String {
    crate::application::crypto::sha256_hex(domain.as_bytes())
}

fn domain_block_json(row: &AdminDomainBlock) -> Value {
    json!({
        "id": row.id.to_string(),
        "domain": row.domain,
        "digest": sha256_hex_domain(&row.domain),
        "created_at": rfc3339_ms(row.created_at_ms),
        "severity": row.severity.as_str(),
        "reject_media": row.reject_media,
        "reject_reports": row.reject_reports,
        "private_comment": row.private_comment.clone().map(Value::String).unwrap_or(Value::Null),
        "public_comment": row.public_comment.clone().map(Value::String).unwrap_or(Value::Null),
        "obfuscate": row.obfuscate,
    })
}

fn domain_block_patch(body: &[(String, String)]) -> AdminDomainBlockPatch {
    AdminDomainBlockPatch {
        severity: forms::get(body, "severity").and_then(AdminDomainSeverity::parse),
        reject_media: forms::get(body, "reject_media").map(bool_cast),
        reject_reports: forms::get(body, "reject_reports").map(bool_cast),
        private_comment: forms::get(body, "private_comment").map(|s| {
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }),
        public_comment: forms::get(body, "public_comment").map(|s| {
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }),
        obfuscate: forms::get(body, "obfuscate").map(bool_cast),
    }
}

pub async fn domain_blocks_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:domain_blocks"),
    ) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(
        &uri,
        100,
        500,
        |max, since, min, limit| store.admin_domain_blocks_page(max, since, min, limit),
        domain_block_json,
        |row| row.id,
    )
}

pub async fn domain_blocks_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:domain_blocks"),
    ) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_domain_block_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(domain_block_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn domain_blocks_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:domain_blocks"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let domain = forms::get(&body, "domain").unwrap_or("");
    let patch = domain_block_patch(&body);
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_domain_block(domain, patch, state.context.now_ms()) {
        Ok(row) => {
            if let Err(err) = audit(&state, actor, "create", "DomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            // Federation enforcement (Phase 6) picks the row up from the
            // store; until then the row is stored but not enforced.
            Json(domain_block_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn domain_blocks_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:domain_blocks"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let patch = domain_block_patch(&body);
    let store: &dyn Store = &*state.context.store;
    match store
        .admin_update_domain_block(id, patch, state.context.now_ms())
        .map_err(ApiError::from)
    {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "update", "DomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(domain_block_json(&row)).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn domain_blocks_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:domain_blocks"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_delete_domain_block(id).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "destroy", "DomainBlock", &row.id.to_string()) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

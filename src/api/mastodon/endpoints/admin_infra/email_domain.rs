// ---------------------------------------------------------------------------
// Email-domain blocks.
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
use crate::application::ports::Store;
use crate::domain::AdminEmailDomainBlock;

use super::{audit, bool_cast, paged, require_admin, require_id, rfc3339_ms};

fn email_domain_block_json(row: &AdminEmailDomainBlock) -> Value {
    json!({
        "id": row.id.to_string(),
        "domain": row.domain,
        "created_at": rfc3339_ms(row.created_at_ms),
        "history": [],
        "allow_with_approval": row.allow_with_approval,
    })
}

pub async fn email_domain_blocks_index(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:email_domain_blocks"),
    ) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(
        &uri,
        100,
        200,
        |max, since, min, limit| store.admin_email_domain_blocks_page(max, since, min, limit),
        email_domain_block_json,
        |row| row.id,
    )
}

pub async fn email_domain_blocks_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:email_domain_blocks"),
    ) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store
        .admin_email_domain_block_by_id(id)
        .map_err(ApiError::from)
    {
        Ok(Some(row)) => Json(email_domain_block_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn email_domain_blocks_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:email_domain_blocks"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let domain = forms::get(&body, "domain").unwrap_or("");
    let allow_with_approval = forms::get(&body, "allow_with_approval").is_some_and(bool_cast);
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_email_domain_block(domain, allow_with_approval, state.context.now_ms())
    {
        Ok(row) => {
            if let Err(err) = audit(
                &state,
                actor,
                "create",
                "EmailDomainBlock",
                &row.id.to_string(),
            ) {
                return err.into_response();
            }
            // Stored only: registration-time enforcement needs email
            // confirmation (Phase 2 gap).
            Json(email_domain_block_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn email_domain_blocks_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:email_domain_blocks"),
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
        .admin_delete_email_domain_block(id)
        .map_err(ApiError::from)
    {
        Ok(Some(row)) => {
            if let Err(err) = audit(
                &state,
                actor,
                "destroy",
                "EmailDomainBlock",
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

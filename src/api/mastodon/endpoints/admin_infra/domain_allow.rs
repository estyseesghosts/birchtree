// ---------------------------------------------------------------------------
// Domain allows.
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
use crate::domain::AdminDomainAllow;

use super::{audit, paged, require_admin, require_id, rfc3339_ms};

fn domain_allow_json(row: &AdminDomainAllow) -> Value {
    json!({
        "id": row.id.to_string(),
        "domain": row.domain,
        "created_at": rfc3339_ms(row.created_at_ms),
    })
}

pub async fn domain_allows_index(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:domain_allows"),
    ) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    paged(
        &uri,
        100,
        500,
        |max, since, min, limit| store.admin_domain_allows_page(max, since, min, limit),
        domain_allow_json,
        |row| row.id,
    )
}

pub async fn domain_allows_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    if let Err(err) = require_admin(
        &auth.0,
        &*state.context.store,
        read_scopes!("admin:read:domain_allows"),
    ) {
        return err.into_response();
    }
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_domain_allow_by_id(id).map_err(ApiError::from) {
        Ok(Some(row)) => Json(domain_allow_json(&row)).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

pub async fn domain_allows_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:domain_allows"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let domain = forms::get(&body, "domain").unwrap_or("");
    let store: &dyn Store = &*state.context.store;
    match store.admin_create_domain_allow(domain, state.context.now_ms()) {
        Ok(row) => {
            // One audit record per admin write, dupes included.
            if let Err(err) = audit(&state, actor, "create", "DomainAllow", &row.id.to_string()) {
                return err.into_response();
            }
            Json(domain_allow_json(&row)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

pub async fn domain_allows_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let actor = match require_admin(
        &auth.0,
        &*state.context.store,
        write_scopes!("admin:write:domain_allows"),
    ) {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let id = match require_id(&id) {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.admin_delete_domain_allow(id).map_err(ApiError::from) {
        Ok(Some(row)) => {
            if let Err(err) = audit(&state, actor, "destroy", "DomainAllow", &row.id.to_string()) {
                return err.into_response();
            }
            Json(json!({})).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => err.into_response(),
    }
}

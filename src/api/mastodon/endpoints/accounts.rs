//! Account endpoints: registration, profile read/update, credentials and
//! relationships.
//!
//! Contracts follow Mastodon v4.7.3: `POST /api/v1/accounts` requires an
//! app-only (client-credentials) token with `write`/`write:accounts`; read
//! endpoints accept `read`/`read:accounts`; `verify_credentials` and
//! `update_credentials` require a user token.

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::forms::{self, FormBody};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::accounts::{self, RegisterAccountCommand};
use crate::application::ports::{
    AccountProfileUpdate, Store, UserSettingsUpdate,
};
use crate::application::relationships;
use crate::domain::Error as DomainError;

/// Scope alternatives accepted by the write endpoints.
const WRITE_SCOPES: &[&str] = &["write", "write:accounts"];

/// Attach the caching headers Mastodon uses for token responses.
fn with_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// `POST /api/v1/accounts` — public registration.
pub async fn register(
    State(state): State<ApiState>,
    auth: Auth,
    FormBody(pairs): FormBody,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    // Mastodon's `require_client_credentials!`.
    if !auth.0.token.is_app_only() {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "This method requires an client credentials authentication",
        )
        .into_response();
    }
    // Mastodon's `check_enabled_registrations` answers 403 Forbidden.
    if !state.server.registrations_open {
        return ApiError::forbidden("Forbidden").into_response();
    }

    let field = |key: &str| forms::get(&pairs, key).map(str::to_string);
    let username = field("username").unwrap_or_default();
    let email = field("email").unwrap_or_default();
    let password = field("password").unwrap_or_default();
    let agreement = forms::get(&pairs, "agreement")
        .map(bool_cast)
        .unwrap_or(false);

    let cmd = RegisterAccountCommand {
        username,
        email,
        password,
        agreement,
        locale: field("locale"),
        reason: field("reason"),
        invite_code: field("invite_code"),
        display_name: None,
    };

    let store: &dyn Store = &*state.context.store;
    match accounts::register_account(
        store,
        store,
        &state.server.public_url,
        state.context.now_ms(),
        state.server.registrations_open,
        &auth.0.app,
        cmd,
    ) {
        Ok(registered) => {
            let body = dto::token_json(
                &registered.token_plaintext,
                &registered.token.scopes,
                registered.token.created_at_ms,
            );
            with_no_store((StatusCode::OK, axum::Json(body)).into_response())
        }
        Err(DomainError::AlreadyExists(_)) => validation_error(
            "Validation failed: Username has already been taken",
            json!({
                "username": [{
                    "error": "ERR_TAKEN",
                    "description": "has already been taken",
                }],
            }),
        ),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/{id}` — public account profile.
pub async fn show(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<crate::domain::Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.account_by_id(id) {
        Ok(Some(account)) => {
            let stats = store.account_stats(id).ok();
            axum::Json(dto::account_json_with_stats(
                &state.server,
                &account,
                stats.as_ref(),
            ))
            .into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/verify_credentials`.
pub async fn verify_credentials(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["profile", "read", "read:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    credential_response(&state, account_id)
}

/// `PATCH /api/v1/accounts/update_credentials`.
pub async fn update_credentials(
    State(state): State<ApiState>,
    auth: Auth,
    FormBody(pairs): FormBody,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };

    let field = |key: &str| forms::get(&pairs, key);

    let display_name = field("display_name").map(str::to_string);
    if display_name.as_deref().is_some_and(|v| v.chars().count() > 40) {
        return validation_error(
            "Validation failed: Display name is too long (maximum is 40 characters)",
            json!({}),
        );
    }
    let note = field("note").map(str::to_string);
    if note.as_deref().is_some_and(|v| v.chars().count() > 500) {
        return validation_error(
            "Validation failed: Note is too long (maximum is 500 characters)",
            json!({}),
        );
    }

    let store: &dyn Store = &*state.context.store;
    let profile = AccountProfileUpdate {
        display_name,
        note,
        locked: field("locked").map(bool_cast),
        discoverable: field("discoverable").map(bool_cast),
        indexable: field("indexable").map(bool_cast),
        hide_collections: field("hide_collections").map(bool_cast),
    };
    if let Err(err) = store.update_account_profile(account_id, profile) {
        return ApiError::from(err).into_response();
    }

    // `source[...]` carries posting preferences.
    let source = |key: &str| forms::get(&pairs, key);
    if source("source[privacy]").is_some()
        || source("source[sensitive]").is_some()
        || source("source[language]").is_some()
        || source("source[quote_policy]").is_some()
    {
        let update = UserSettingsUpdate {
            default_privacy: source("source[privacy]").map(str::to_string),
            default_sensitive: source("source[sensitive]").map(bool_cast),
            default_language: source("source[language]").map(str::to_string),
            default_quote_policy: source("source[quote_policy]").map(str::to_string),
        };
        if let Err(err) = store.update_user_settings(account_id, update) {
            return ApiError::from(err).into_response();
        }
    }

    credential_response(&state, account_id)
}

/// `GET /api/v1/accounts/relationships`.
pub async fn relationships(
    State(state): State<ApiState>,
    auth: Auth,
    RawQuery(query): RawQuery,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:follows"]) {
        return err.into_response();
    }
    let viewer_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };

    let pairs = forms::parse(query.unwrap_or_default().as_bytes());
    let store: &dyn Store = &*state.context.store;

    // Resolve targets in request order; unknown ids are silently skipped
    // (Mastodon skips deleted/suspended accounts).
    let mut target_ids: Vec<crate::domain::Id> = Vec::new();
    for raw in forms::all(&pairs, "id") {
        let Ok(id) = raw.parse::<crate::domain::Id>() else {
            continue;
        };
        if store.account_by_id(id).ok().flatten().is_some() {
            target_ids.push(id);
        }
    }
    match relationships::relationships_for(store, viewer_id, &target_ids, state.context.now_ms()) {
        Ok(relations) => {
            let out: Vec<Value> = relations.iter().map(dto::relationship_json).collect();
            axum::Json(out).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Build the `CredentialAccountSerializer` body for `account_id`.
fn credential_response(state: &ApiState, account_id: crate::domain::Id) -> Response {
    let store: &dyn Store = &*state.context.store;
    let account = match store.account_by_id(account_id) {
        Ok(Some(account)) => account,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    let user = store.user_by_account_id(account_id).ok().flatten();
    let stats = store.account_stats(account_id).ok();
    axum::Json(dto::credential_account_json_with_stats(
        &state.server,
        &account,
        user.as_ref(),
        stats.as_ref(),
    ))
    .into_response()
}

/// 422 with Mastodon's `ValidationErrorFormatter` shape.
fn validation_error(message: &str, details: Value) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(json!({ "error": message, "details": details })),
    )
        .into_response()
}

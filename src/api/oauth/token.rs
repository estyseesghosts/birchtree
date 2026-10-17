//! `POST /oauth/token` and `POST /oauth/revoke`.
//!
//! Supports the two grants Mastodon's Doorkeeper configuration enables for
//! this build: `authorization_code` (with PKCE `S256`) and
//! `client_credentials`. Refresh tokens are disabled (Mastodon issues
//! non-expiring access tokens), so `refresh_token` is reported as
//! `unsupported_grant_type`.
//!
//! Errors use Doorkeeper's OAuth error body (`error` + `error_description`)
//! rather than the Mastodon REST `{ "error": ... }` shape.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

use crate::api::forms::{self, FormBody};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::dto;
use crate::api::oauth::scopes;
use crate::application::auth::{self, CodeExchange};
use crate::application::crypto;
use crate::application::ports::Store;
use crate::application::streaming::StreamEvent;
use crate::domain::OAuthApplication;

/// Doorkeeper-style OAuth error response.
fn oauth_error(status: StatusCode, error: &str, description: Option<&str>) -> Response {
    let mut body = Map::new();
    body.insert("error".into(), json!(error));
    if let Some(description) = description {
        body.insert("error_description".into(), json!(description));
    }
    (status, axum::Json(Value::Object(body))).into_response()
}

/// Attach the caching headers required for token responses.
fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn json_ok(value: Value) -> Response {
    no_store(axum::Json(value).into_response())
}

/// Decode HTTP Basic credentials as `(client_id, client_secret)`.
///
/// Both halves are form-decoded, matching Rack/Doorkeeper's handling of a
/// URL-encoded `client_id:client_secret` pair.
fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (id, secret) = text.split_once(':')?;
    Some((forms::decode(id), forms::decode(secret)))
}

/// Authenticate the calling client from the Basic header or the request body.
///
/// Doorkeeper accepts either form. A confidential app must present a matching
/// secret; a public app (no stored secret) authenticates on `client_id` alone.
fn authenticate_client(
    state: &ApiState,
    headers: &HeaderMap,
    pairs: &[(String, String)],
) -> Result<OAuthApplication, Response> {
    let invalid = || {
        oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            Some("Client authentication failed due to unknown client, no client authentication included, or unsupported authentication method."),
        )
    };

    let (client_id, client_secret) = match basic_credentials(headers) {
        Some((id, secret)) => (id, Some(secret)),
        None => match forms::get(pairs, "client_id") {
            Some(id) => (id.to_string(), forms::get(pairs, "client_secret").map(str::to_string)),
            None => return Err(invalid()),
        },
    };

    let store: &dyn Store = &*state.context.store;
    let app = store
        .app_by_client_id(&client_id)
        .ok()
        .flatten()
        .ok_or_else(invalid)?;

    if let Some(expected_hash) = &app.client_secret_hash {
        let Some(secret) = client_secret else {
            return Err(invalid());
        };
        if crypto::sha256_hex(secret.as_bytes()) != *expected_hash {
            return Err(invalid());
        }
    }
    Ok(app)
}

/// `POST /oauth/token`.
pub async fn token(
    State(state): State<ApiState>,
    headers: HeaderMap,
    FormBody(pairs): FormBody,
) -> Response {
    let grant_type = forms::get(&pairs, "grant_type").unwrap_or("");

    let app = match authenticate_client(&state, &headers, &pairs) {
        Ok(app) => app,
        Err(response) => return response,
    };

    match grant_type {
        "authorization_code" => exchange_code(&state, &app, &pairs),
        "client_credentials" => client_credentials(&state, &app, &pairs),
        "" => oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("Missing grant_type"),
        ),
        other => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            Some(&format!("The authorization grant type is not supported by the authorization server: {other}")),
        ),
    }
}

/// Authorization-code grant: one-time code + PKCE verifier for a user token.
fn exchange_code(state: &ApiState, app: &OAuthApplication, pairs: &[(String, String)]) -> Response {
    let Some(code) = forms::get(pairs, "code") else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("Missing code"),
        );
    };
    let store: &dyn Store = &*state.context.store;
    let exchange = CodeExchange {
        code,
        redirect_uri: forms::get(pairs, "redirect_uri"),
        code_verifier: forms::get(pairs, "code_verifier"),
    };
    match auth::exchange_authorization_code(store, app, exchange, state.context.now_ms()) {
        Ok((plaintext, token)) => {
            json_ok(dto::token_json(&plaintext, &token.scopes, token.created_at_ms))
        }
        Err(_) => oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            Some("The provided authorization grant is invalid, expired, revoked, does not match the redirection URI used in the authorization request, or was issued to another client."),
        ),
    }
}

/// Client-credentials grant: an app-only (no user) token.
fn client_credentials(
    state: &ApiState,
    app: &OAuthApplication,
    pairs: &[(String, String)],
) -> Response {
    // Only confidential apps may use this grant (Doorkeeper requires a client
    // secret for client_credentials).
    if app.client_secret_hash.is_none() {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized_client",
            Some("This application is not allowed to use the client_credentials grant"),
        );
    }

    let requested = match forms::get(pairs, "scope") {
        Some(scope) if !scope.trim().is_empty() => scopes::parse(scope),
        _ => app.scopes.clone(),
    };
    if !scopes::is_subset(&requested, &app.scopes) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_scope",
            Some("The requested scope is invalid, unknown, or malformed."),
        );
    }

    let store: &dyn Store = &*state.context.store;
    match auth::issue_client_credentials_token(store, app.id, requested, state.context.now_ms()) {
        Ok((plaintext, token)) => {
            json_ok(dto::token_json(&plaintext, &token.scopes, token.created_at_ms))
        }
        Err(_) => oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", None),
    }
}

/// `POST /oauth/revoke`.
///
/// Always answers `200 {}` for a well-formed request (including an empty or
/// unknown token). A token that belongs to a different client is rejected with
/// `403 unauthorized_client`.
pub async fn revoke(
    State(state): State<ApiState>,
    headers: HeaderMap,
    FormBody(pairs): FormBody,
) -> Response {
    let token = forms::get(&pairs, "token").unwrap_or("");
    if token.trim().is_empty() {
        return json_ok(json!({}));
    }

    let app = match authenticate_client(&state, &headers, &pairs) {
        Ok(app) => app,
        Err(response) => return response,
    };

    let store: &dyn Store = &*state.context.store;
    let existing = store.token_by_plaintext(token).ok().flatten();
    if let Some(existing) = &existing {
        if existing.app_id != app.id {
            return oauth_error(
                StatusCode::FORBIDDEN,
                "unauthorized_client",
                Some("The token was not issued to this client"),
            );
        }
    }
    if let Ok(true) = store.revoke_token(token) {
        // Upstream's `kill`: the token's open streams close.
        if let Some(existing) = existing {
            state.context.streaming.publish(StreamEvent::TokenRevoked {
                token_id: existing.id,
            });
        }
    }
    json_ok(json!({}))
}

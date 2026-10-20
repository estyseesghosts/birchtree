//! `GET`/`POST /oauth/authorize` — the authorization-code + PKCE flow.
//!
//! A browser session is established by posting a username/password to this same
//! endpoint; it is persisted as a hashed session token in a cookie. The user is
//! then shown a consent page and, on approval, redirected back to the client
//! with a one-time code (and the original `state`).

use std::collections::HashMap;

use axum::extract::{Query, RawForm, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::forms;
use crate::api::mastodon::ApiState;
use crate::api::oauth::consent_page;
use crate::api::oauth::pkce;
use crate::api::oauth::scopes;
use crate::application::auth;
use crate::application::crypto;
use crate::application::ports::Store;
use crate::domain::{Id, OAuthApplication};

/// Cookie name for the browser session.
pub const SESSION_COOKIE: &str = "birchtree_session";

#[derive(Debug, serde::Deserialize)]
pub struct AuthorizeQuery {
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    pub response_type: Option<String>,
    pub scope: Option<String>,
    pub state: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub force_login: Option<String>,
    pub prompt: Option<String>,
}

/// A validated authorize request.
struct Prepared {
    app: OAuthApplication,
    redirect_uri: String,
    scopes: Vec<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

impl Prepared {
    /// OAuth parameters carried through the login/consent forms.
    fn hidden_fields(&self) -> Vec<(String, String)> {
        let mut fields = vec![
            ("client_id".to_string(), self.app.client_id.clone()),
            ("redirect_uri".to_string(), self.redirect_uri.clone()),
            ("response_type".to_string(), "code".to_string()),
            ("scope".to_string(), self.scopes.join(" ")),
        ];
        if let Some(state) = &self.state {
            fields.push(("state".to_string(), state.clone()));
        }
        if let Some(challenge) = &self.code_challenge {
            fields.push(("code_challenge".to_string(), challenge.clone()));
            fields.push((
                "code_challenge_method".to_string(),
                self.code_challenge_method
                    .clone()
                    .unwrap_or_else(|| "S256".to_string()),
            ));
        }
        fields
    }
}

fn error_page(status: StatusCode, message: &str) -> Response {
    (status, consent_page::error_page(message)).into_response()
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    forms::get(pairs, key)
}

/// Validate the client, redirect URI and scope of an authorize request.
fn prepare(state: &ApiState, pairs: &[(String, String)]) -> Result<Prepared, String> {
    let client_id = field(pairs, "client_id").ok_or("missing client_id")?;
    let store: &dyn Store = &*state.context.store;
    let app = store
        .app_by_client_id(client_id)
        .map_err(|_| "storage error".to_string())?
        .ok_or("invalid client_id")?;

    let redirect_uri = match field(pairs, "redirect_uri") {
        Some(uri) if app.redirect_uris.iter().any(|r| r == uri) => uri.to_string(),
        Some(_) => return Err("redirect_uri does not match the registered client".to_string()),
        None if app.redirect_uris.len() == 1 => app.redirect_uris[0].clone(),
        None => return Err("missing redirect_uri".to_string()),
    };

    match field(pairs, "response_type") {
        Some("code") | None => {}
        Some(_) => return Err("unsupported response_type".to_string()),
    }

    let requested = match field(pairs, "scope") {
        Some(scope) if !scope.trim().is_empty() => scopes::parse(scope),
        _ => app.scopes.clone(),
    };
    if !scopes::is_subset(&requested, &app.scopes) {
        return Err("requested scope is not allowed for this application".to_string());
    }

    let (code_challenge, code_challenge_method) = match field(pairs, "code_challenge") {
        Some(challenge) if !challenge.is_empty() => {
            if !pkce::is_valid_challenge(challenge) {
                return Err("invalid code_challenge".to_string());
            }
            let method = field(pairs, "code_challenge_method").unwrap_or("S256");
            if !pkce::is_supported_method(method) {
                return Err("unsupported code_challenge_method".to_string());
            }
            (Some(challenge.to_string()), Some("S256".to_string()))
        }
        _ => (None, None),
    };

    Ok(Prepared {
        app,
        redirect_uri,
        scopes: requested,
        state: field(pairs, "state").map(str::to_string),
        code_challenge,
        code_challenge_method,
    })
}

/// Extract the session's account id and username from the cookie, if valid.
fn session_account(state: &ApiState, headers: &HeaderMap) -> Option<(Id, String)> {
    let token = cookie_value(headers, SESSION_COOKIE)?;
    let store: &dyn Store = &*state.context.store;
    let session = store.session_by_plaintext(&token).ok()??;
    if !session.is_valid_at(state.context.now_ms()) {
        return None;
    }
    let account = store.account_by_id(session.user_id).ok()??;
    Some((account.id, account.username))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            if k == name {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn session_cookie(state: &ApiState, token: &str) -> String {
    let secure = if state.server.public_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400{secure}")
}

/// Double-submit CSRF cookie for `/api/web/*` (readable by first-party JS;
/// must be echoed back as `X-CSRF-Token` on state-changing requests).
fn csrf_cookie(state: &ApiState, token: &str) -> String {
    let secure = if state.server.public_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!("birchtree_csrf={token}; Path=/; SameSite=Lax; Max-Age=86400{secure}")
}

fn redirect_with(redirect_uri: &str, params: &[(&str, String)]) -> Response {
    let separator = if redirect_uri.contains('?') { '&' } else { '?' };
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let location = format!("{redirect_uri}{separator}{query}");
    (
        StatusCode::FOUND,
        [(header::LOCATION, location)],
    )
        .into_response()
}

fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `GET /oauth/authorize`.
pub async fn authorize_get(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    let pairs = query_to_pairs(query);
    let prepared = match prepare(&state, &pairs) {
        Ok(p) => p,
        Err(message) => return error_page(StatusCode::BAD_REQUEST, &message),
    };
    let force_login = field(&pairs, "force_login").is_some_and(|v| v == "true" || v == "1");
    let hidden = prepared.hidden_fields();
    match session_account(&state, &headers) {
        Some((_id, username)) if !force_login => (StatusCode::OK, consent_page::consent_page(
            &hidden,
            &username,
            &prepared.app.name,
            &prepared.scopes,
        ))
            .into_response(),
        _ => (StatusCode::OK, consent_page::login_page(&hidden, None)).into_response(),
    }
}

fn query_to_pairs(query: AuthorizeQuery) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (key, value) in [
        ("client_id", query.client_id),
        ("redirect_uri", query.redirect_uri),
        ("response_type", query.response_type),
        ("scope", query.scope),
        ("state", query.state),
        ("code_challenge", query.code_challenge),
        ("code_challenge_method", query.code_challenge_method),
        ("force_login", query.force_login),
        ("prompt", query.prompt),
    ] {
        if let Some(value) = value {
            pairs.push((key.to_string(), value));
        }
    }
    pairs
}

/// `POST /oauth/authorize`: either a login submission or a consent decision.
pub async fn authorize_post(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawForm(body): RawForm,
) -> Response {
    let pairs = forms::parse(&body);

    // Login submission.
    if let Some(username) = field(&pairs, "username") {
        let password = field(&pairs, "password").unwrap_or("");
        let store: &dyn Store = &*state.context.store;
        let domain = state.server.domain.clone();
        match auth::authenticate_password(store, username, &domain, password) {
            Ok(Some(user)) => {
                match auth::create_login_session(store, user.account_id, state.context.now_ms()) {
                    Ok((plaintext, _session)) => {
                        let prepared = match prepare(&state, &pairs) {
                            Ok(p) => p,
                            Err(message) => {
                                return error_page(StatusCode::BAD_REQUEST, &message);
                            }
                        };
                        let hidden = prepared.hidden_fields();
                        let body = consent_page::consent_page(
                            &hidden,
                            username,
                            &prepared.app.name,
                            &prepared.scopes,
                        );
                        let mut response = (StatusCode::OK, body).into_response();
                        if let Ok(value) =
                            header::HeaderValue::from_str(&session_cookie(&state, &plaintext))
                        {
                            response.headers_mut().insert(header::SET_COOKIE, value);
                        }
                        // Double-submit CSRF cookie for `/api/web/*`
                        // (readable by first-party JS; must be echoed back
                        // as `X-CSRF-Token` on state-changing requests).
                        let csrf = crypto::random_token_hex(32);
                        if let Ok(value) = header::HeaderValue::from_str(&csrf_cookie(&state, &csrf))
                        {
                            response.headers_mut().append(header::SET_COOKIE, value);
                        }
                        return response;
                    }
                    Err(_) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, "login failed"),
                }
            }
            Ok(None) | Err(_) => {
                let prepared = prepare(&state, &pairs);
                let hidden = prepared.map(|p| p.hidden_fields()).unwrap_or_default();
                let body = consent_page::login_page(&hidden, Some("Invalid username or password"));
                return (StatusCode::OK, body).into_response();
            }
        }
    }

    // Consent decision.
    let Some((_account_id, _username)) = session_account(&state, &headers) else {
        let prepared = prepare(&state, &pairs);
        let hidden = prepared.map(|p| p.hidden_fields()).unwrap_or_default();
        let body = consent_page::login_page(&hidden, Some("Session expired; sign in again"));
        return (StatusCode::UNAUTHORIZED, body).into_response();
    };

    let prepared = match prepare(&state, &pairs) {
        Ok(p) => p,
        Err(message) => return error_page(StatusCode::BAD_REQUEST, &message),
    };

    match field(&pairs, "decision") {
        Some("deny") => {
            let mut params: Vec<(&str, String)> = vec![("error", "access_denied".to_string())];
            if let Some(state_param) = &prepared.state {
                params.push(("state", state_param.clone()));
            }
            redirect_with(&prepared.redirect_uri, &params)
        }
        Some("authorize") => {
            let account_id = match session_account(&state, &headers) {
                Some((id, _)) => id,
                None => return error_page(StatusCode::UNAUTHORIZED, "session expired"),
            };
            let store: &dyn Store = &*state.context.store;
            match auth::create_authorization_code(
                store,
                prepared.app.id,
                account_id,
                prepared.redirect_uri.clone(),
                prepared.scopes.clone(),
                prepared.code_challenge.clone(),
                prepared.code_challenge_method.clone(),
                state.context.now_ms(),
            ) {
                Ok((code, _record)) => {
                    let mut params = vec![("code", code)];
                    if let Some(state_param) = &prepared.state {
                        params.push(("state", state_param.clone()));
                    }
                    redirect_with(&prepared.redirect_uri, &params)
                }
                Err(_) => error_page(StatusCode::INTERNAL_SERVER_ERROR, "could not issue code"),
            }
        }
        _ => error_page(StatusCode::BAD_REQUEST, "missing decision"),
    }
}

/// Unused helper retained so `consent_page`'s username lookup is available to
/// future GET-session handling.
#[allow(dead_code)]
fn account_username(state: &ApiState, id: Id) -> Option<String> {
    let store: &dyn Store = &*state.context.store;
    store.account_by_id(id).ok()??.username.into()
}

/// Placeholder to keep `HashMap` import meaningful for future query merging.
#[allow(dead_code)]
type QueryMap = HashMap<String, String>;

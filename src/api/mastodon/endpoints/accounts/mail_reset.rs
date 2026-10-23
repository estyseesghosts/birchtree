use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

use crate::api::forms::{self, FormBody};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::ApiError;
use crate::application::accounts;
use crate::application::ports::Store;
use crate::domain::EmailTokenKind;
use crate::domain::Error as DomainError;
use crate::mailer::{MailEnv, MailError};

// ---------------------------------------------------------------------------
// Invites + email (round 3, area A; D12 remainder closed here).
//
// Router wiring is parent-owned; the proposed routes are in the area report:
//   GET  /invite/:invite_code               -> invite_show
//   GET  /auth/confirmation                  -> confirmation::confirmation_get
//   POST /api/v1/emails/confirmations       -> email_confirmation_create
//   GET  /api/v1/emails/check_confirmation  -> email_confirmation_check
//   POST /api/v1/emails/password_reset      -> password_reset_create (public)
// ---------------------------------------------------------------------------

/// Map a [`MailError`] to an explicit HTTP refusal. Disabled mail is `503`
/// (never a silent success); transport failure is `502`; a bad address is
/// `422`.
fn mail_error_response(err: MailError) -> Response {
    match err {
        MailError::Disabled(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({ "error": "Email delivery is not configured" })),
        )
            .into_response(),
        MailError::InvalidRecipient(message) => {
            ApiError::unprocessable(format!("email {message}")).into_response()
        }
        MailError::Transport(_) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(json!({ "error": "Email delivery failed" })),
        )
            .into_response(),
    }
}

/// `GET /invite/:invite_code` — mirrors `Api::V1::InvitesController#show`
/// (`app/controllers/api/v1/invites_controller.rb:19-29`): `200
/// {invite_code, instance_api_url}`, `404` for an unknown code (`find_by!`),
/// `401 {error: "Invalid invitation code"}` for a known-but-unusable one
/// (`check_valid_usage!`, before the registrations gate, as upstream).
pub async fn invite_show(
    State(state): State<ApiState>,
    Extension(invites): Extension<Arc<dyn accounts::InviteStore>>,
    Path(code): Path<String>,
) -> Response {
    match accounts::invite_status(&*invites, &code, state.context.now_ms()) {
        Ok(_) => axum::Json(json!({
            "invite_code": code,
            "instance_api_url": format!("{}/api/v2/instance", state.server.public_url),
        }))
        .into_response(),
        Err(DomainError::NotFound(_)) => ApiError::not_found().into_response(),
        Err(DomainError::Unauthorized(message)) => {
            ApiError::new(StatusCode::UNAUTHORIZED, message).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/emails/confirmations` — mirrors
/// `Api::V1::Emails::ConfirmationsController#create`
/// (`app/controllers/api/v1/emails/confirmations_controller.rb:13-18`):
/// re-sends the confirmation mail; `200 {}`. Requires a user token with
/// `write`/`write:accounts` and an unconfirmed address (`403` otherwise, like
/// upstream `require_user_not_confirmed!`), and the caller's app must own the
/// signup (`403` on mismatch, like upstream
/// `require_user_owned_by_application!`; pre-attribution rows with no recorded
/// app are grandfathered). Without a mail provider the
/// answer is an explicit `503`, never a fake success. Delivery is a durable
/// `SendConfirmationEmail` job (worker sends after commit, like upstream
/// `deliver_later`); the token is random per request and is stored durably
/// BEFORE the job enqueues (a crash between the two leaves a harmless orphan
/// token, never a mailed link with no backing row).
pub async fn email_confirmation_create(
    State(state): State<ApiState>,
    auth: Auth,
    Extension(env): Extension<Arc<MailEnv>>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let user = match store.user_by_account_id(account_id) {
        Ok(Some(user)) => user,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    if let Err(err) = accounts::check_confirmation_ownership(&user, auth.0.app.id) {
        return ApiError::from(err).into_response();
    }
    if user.email_verified {
        return ApiError::forbidden(
            "This method is only available while the e-mail is awaiting confirmation",
        )
        .into_response();
    }
    let Some(email) = user.email else {
        return ApiError::unprocessable("email address is missing").into_response();
    };
    if !env.enabled {
        return mail_error_response(MailError::Disabled(
            "no SMTP provider configured (set smtp_url and mail_from)",
        ));
    }
    if let Err(err) = crate::mailer::validate_address(&email) {
        return mail_error_response(err);
    }
    // The store mints the random bearer, persists only its digest and queues
    // the mail job in one transaction (no orphan token, no plaintext at rest).
    let now_ms = state.context.now_ms();
    match store.issue_email_token(account_id, &email, EmailTokenKind::Confirmation, now_ms) {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/emails/check_confirmation` — mirrors
/// `Api::V1::Emails::ConfirmationsController#check`: `200 true`/`false` for
/// whether the caller's address is confirmed. User token, `read` scope. The
/// caller's app must own the signup (`403` on mismatch, like upstream
/// `require_user_owned_by_application!`; pre-attribution rows are
/// grandfathered).
pub async fn email_confirmation_check(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.user_by_account_id(account_id) {
        Ok(Some(user)) => {
            if let Err(err) = accounts::check_confirmation_ownership(&user, auth.0.app.id) {
                return ApiError::from(err).into_response();
            }
            axum::Json(json!(user.email_verified)).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/emails/password_reset` — public forgot-password flow
/// (`200 {}` always, when mail is configured).
///
/// This replaces the earlier authenticated-only extension. Upstream has no API
/// reset route at all (password reset is web-only: `POST /auth/password`,
/// Devise `PasswordsController#create`, which is unauthenticated and always
/// redirects visibly-successful). The Devise parity shape is therefore a
/// public endpoint taking an `email` param: a known address stores a token
/// and queues a `SendPasswordResetEmail` job; an unknown address queues
/// nothing — and every case answers the same `200 {}` so the endpoint is
/// never an account-enumeration oracle. (The authed-only variant is dropped
/// because it matches neither upstream surface: no client that speaks the
/// Devise flow can use it, and it cannot serve a user who forgot their
/// password but still holds a token.)
///
/// When no mail provider is configured the answer is the same explicit `503`
/// for known and unknown addresses alike (still no oracle: the response
/// carries no information about the address).
pub async fn password_reset_create(
    State(state): State<ApiState>,
    Extension(env): Extension<Arc<MailEnv>>,
    FormBody(pairs): FormBody,
) -> Response {
    let email_param = forms::get(&pairs, "email").unwrap_or("").trim().to_string();
    let now_ms = state.context.now_ms();
    if !env.enabled {
        return mail_error_response(MailError::Disabled(
            "no SMTP provider configured (set smtp_url and mail_from)",
        ));
    }
    let store: &dyn Store = &*state.context.store;
    let account = match store.account_by_email(&email_param) {
        Ok(account) => account,
        Err(err) => return ApiError::from(err).into_response(),
    };
    // Unknown address: queue nothing, answer 200 (no enumeration oracle).
    let Some(account) = account else {
        return axum::Json(json!({})).into_response();
    };
    let address = match store.user_by_account_id(account.id) {
        Ok(Some(user)) => user.email,
        Ok(None) => None,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let Some(address) = address else {
        return axum::Json(json!({})).into_response();
    };
    // A stored address that no longer validates is dropped silently (answering
    // 422 here would turn the endpoint into an oracle for known addresses).
    if crate::mailer::validate_address(&address).is_err() {
        return axum::Json(json!({})).into_response();
    }
    match store.issue_email_token(account.id, &address, EmailTokenKind::PasswordReset, now_ms) {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Password-reset redemption (gaps batch G-A; closes the D12 remainder).
//
// The mailed "forgot password" link (`SendPasswordResetEmail` job) lands on
// `GET /auth/password/edit?reset_password_token=…` (Devise
// `PasswordsController#edit`, which renders the form without checking the
// token) and submits to `POST /auth/password` (Devise `#update`, here POST
// per the frozen shape). Bearer lifetime is
// `Config::password_reset_ttl_hours` (default 6h); redemption consumes the
// single-use bearer; unknown/used/expired/wrong-kind tokens answer one
// indistinguishable 422 page (never an oracle). No new mailer shape.
// Router wiring is parent-owned; the proposed routes are:
//   GET  /auth/password/edit -> password_reset_page
//   POST /auth/password      -> password_reset_update
// ---------------------------------------------------------------------------

/// Query shape for the mailed link: `?reset_password_token=…`.
#[derive(Debug, serde::Deserialize)]
pub struct PasswordResetQuery {
    pub reset_password_token: Option<String>,
}

/// Bearer lifetime in ms for `ttl_hours` (`Config::password_reset_ttl_hours`).
pub fn reset_bearer_ttl_ms(ttl_hours: u64) -> u64 {
    ttl_hours.saturating_mul(3_600_000)
}

/// Validate a candidate new password (+ confirmation) BEFORE the bearer is
/// touched: an invalid password must not consume the single-use token
/// (upstream Devise clears the reset token only on a valid save, so a typo
/// never burns the link).
pub fn validate_reset_password(password: &str, confirmation: &str) -> Result<(), String> {
    if password != confirmation {
        return Err("Password confirmation doesn't match".to_string());
    }
    if password.chars().count() < accounts::MIN_PASSWORD_LEN {
        return Err(format!(
            "Password is too short (minimum is {} characters)",
            accounts::MIN_PASSWORD_LEN
        ));
    }
    Ok(())
}

/// Whether a stored reset bearer is still live at `now_ms`.
pub fn reset_bearer_live(created_at_ms: u64, now_ms: u64, ttl_hours: u64) -> bool {
    now_ms.saturating_sub(created_at_ms) <= reset_bearer_ttl_ms(ttl_hours)
}

fn reset_page(status: StatusCode, title: &str, heading: &str, body: &str) -> Response {
    use crate::api::oauth::consent_page::escape;
    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title></head><body><main><h1>{}</h1><p>{}</p></main></body></html>\n",
        escape(title),
        escape(heading),
        body,
    );
    (status, axum::response::Html(html)).into_response()
}

/// The set-password form. `error` re-renders the form with a message (422)
/// keeping the token in the hidden field so the user can retry without
/// burning the link; `None` is the fresh form (200).
fn reset_form_page(token: &str, error: Option<&str>) -> Response {
    use crate::api::oauth::consent_page::escape;
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let error_html = error
        .map(|message| format!("<p role=\"alert\">{}</p>", escape(message)))
        .unwrap_or_default();
    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Set a new password</title></head><body><main>\
         <h1>Set a new password</h1>{error_html}\
         <form method=\"post\" action=\"/auth/password\">\
         <input type=\"hidden\" name=\"reset_password_token\" value=\"{}\">\
         <label>New password <input type=\"password\" name=\"password\" autocomplete=\"new-password\" required minlength=\"8\"></label>\
         <label>Confirm new password <input type=\"password\" name=\"password_confirmation\" autocomplete=\"new-password\" required minlength=\"8\"></label>\
         <button type=\"submit\">Change my password</button>\
         </form></main></body></html>\n",
        escape(token),
    );
    (status, axum::response::Html(html)).into_response()
}

/// The one indistinguishable invalid/used/expired-token page (422, never an
/// oracle distinguishing unknown from used).
fn reset_invalid_page() -> Response {
    reset_page(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Password reset failed",
        "Password reset failed",
        "This password reset link is invalid or has already been used. \
         Request a new password reset email and try again.",
    )
}

/// Look up a reset param in both the flat shape (`password`) and the Devise
/// nested shape (`user[password]`).
fn reset_param<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    forms::get(pairs, key)
        .or_else(|| forms::get(pairs, &format!("user[{key}]")))
        .unwrap_or("")
}

/// `GET /auth/password/edit?reset_password_token=…` — the server-rendered
/// set-password form. Like Devise's `#edit`, the token is not checked here;
/// it rides the hidden field and is validated on submit.
pub async fn password_reset_page(
    axum::extract::Query(query): axum::extract::Query<PasswordResetQuery>,
) -> Response {
    reset_form_page(query.reset_password_token.as_deref().unwrap_or(""), None)
}

/// `POST /auth/password` — consume the bearer and set the new password.
pub async fn password_reset_update(
    State(state): State<ApiState>,
    FormBody(pairs): FormBody,
) -> Response {
    let token = reset_param(&pairs, "reset_password_token");
    let password = reset_param(&pairs, "password");
    let confirmation = reset_param(&pairs, "password_confirmation");
    if token.is_empty() {
        return reset_invalid_page();
    }
    // Password problems answer before the bearer is touched, so a typo
    // never burns the single-use link.
    if let Err(message) = validate_reset_password(password, confirmation) {
        return reset_form_page(token, Some(&message));
    }
    let store: &dyn Store = &*state.context.store;
    let now_ms = state.context.now_ms();
    let password_hash = match crate::application::crypto::hash_password(password) {
        Ok(hash) => hash,
        Err(_) => return ApiError::internal().into_response(),
    };
    // One transaction validates kind and lifetime, burns the bearer and sets
    // the password: a failure cannot consume the link without the change.
    let ttl_ms = reset_bearer_ttl_ms(state.server.password_reset_ttl_hours);
    match accounts::reset_password(store, token, password_hash, now_ms, ttl_ms) {
        Ok(_) => reset_page(
            StatusCode::OK,
            "Password changed",
            "Password changed",
            "Your password has been changed. You can now sign in.",
        ),
        // Unknown, used, expired, wrong-kind or vanished account: one page,
        // so the answer never says which case occurred.
        Err(crate::domain::Error::NotFound(_) | crate::domain::Error::InvalidInput(_)) => {
            reset_invalid_page()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

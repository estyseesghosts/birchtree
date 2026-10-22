//! `GET /auth/confirmation` — email-confirmation redemption page (D12).
//!
//! The mailed confirmation link (`UserMailer#confirmation_instructions`,
//! `{base}/auth/confirmation?confirmation_token=…`) lands here. This mirrors
//! upstream's Devise `ConfirmationsController#show`: a valid token confirms
//! the address and renders success; an unknown or already-used token renders
//! the invalid shape (never a success-shaped page, never an oracle that
//! distinguishes "unknown" from "used").
//!
//! Privacy: the `confirmation_token` bearer is a one-time secret. This handler
//! never logs it, and the HTTP tracing span records the path without the query
//! string (see [`crate::observability::RedactedHttpMakeSpan`]), so the token
//! cannot leak through access logs. Reverse proxies in front of this server
//! must likewise strip or redact the query string before logging request
//! lines.
//!
//! Wiring is parent-owned (`src/api/oauth/mod.rs` is a shared file):
//! ```ignore
//! // src/api/oauth/mod.rs
//! pub mod confirmation;
//! // router.rs (next to the other OAuth routes)
//! .route("/auth/confirmation", get(crate::api::oauth::confirmation::confirmation_get))
//! ```
//! Like the login/consent pages there is no client-side JavaScript; the page
//! reuses the [`super::consent_page`] escaping helper.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::api::mastodon::ApiState;
use crate::api::oauth::consent_page::escape;
use crate::application::accounts;
use crate::application::ports::Store;

/// Query shape for the mailed link: `?confirmation_token=…`.
#[derive(Debug, Deserialize)]
pub struct ConfirmationQuery {
    pub confirmation_token: Option<String>,
}

fn page(title: &str, heading: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title></head><body><main><h1>{}</h1><p>{}</p></main></body></html>\n",
        escape(title),
        escape(heading),
        escape(body),
    )
}

/// Human-readable outcome of a redemption attempt (pure, unit-tested without
/// HTTP state).
pub fn outcome_page(confirmed: bool) -> (StatusCode, String) {
    if confirmed {
        (
            StatusCode::OK,
            page(
                "Email confirmed",
                "Email confirmed",
                "Your email address has been confirmed. You can now sign in.",
            ),
        )
    } else {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            page(
                "Confirmation failed",
                "Confirmation failed",
                "This confirmation link is invalid or has already been used. \
                 Request a new confirmation email and try again.",
            ),
        )
    }
}

/// `GET /auth/confirmation?confirmation_token=…`.
pub async fn confirmation_get(
    State(state): State<ApiState>,
    Query(query): Query<ConfirmationQuery>,
) -> Response {
    let store: &dyn Store = &*state.context.store;
    let confirmed = match query
        .confirmation_token
        .as_deref()
        .filter(|t| !t.is_empty())
    {
        Some(token) => accounts::confirm_email(store, token, state.context.now_ms()).is_ok(),
        None => false,
    };
    let (status, body) = outcome_page(confirmed);
    (status, body).into_response()
}

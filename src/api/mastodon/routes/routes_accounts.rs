//! Accounts (+ nearby profile/invite/email/password) routes (S2 slice group B fragment).

use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use axum::Router;
use axum::routing::{delete, get, patch, post};

pub fn router() -> Router<ApiState> {
    Router::new()
        // Accounts: static sub-paths before the `{id}` form.
        .route(
            "/api/v1/accounts",
            get(endpoints::accounts::index).post(endpoints::accounts::register),
        )
        .route("/api/v1/accounts/lookup", get(endpoints::accounts::lookup))
        .route("/api/v1/accounts/search", get(endpoints::accounts::search))
        .route(
            "/api/v1/accounts/verify_credentials",
            get(endpoints::accounts::verify_credentials),
        )
        .route(
            "/api/v1/accounts/update_credentials",
            patch(endpoints::accounts::update_credentials),
        )
        .route(
            "/api/v1/accounts/relationships",
            get(endpoints::accounts::relationships),
        )
        // Profile namespace (4.6+ alias of the credentials endpoints, G-A).
        .route(
            "/api/v1/profile",
            get(endpoints::accounts::profile_show).patch(endpoints::accounts::profile_update),
        )
        .route(
            "/api/v1/profile/avatar",
            delete(endpoints::accounts::profile_avatar_destroy),
        )
        .route(
            "/api/v1/profile/header",
            delete(endpoints::accounts::profile_header_destroy),
        )
        // Identity proofs (deprecated upstream: always `[]` here, G-A).
        .route(
            "/api/v1/accounts/:id/identity_proofs",
            get(endpoints::accounts::identity_proofs_show),
        )
        .route("/api/v1/accounts/:id", get(endpoints::accounts::show))
        // Invites + email. Without a mail provider the mail endpoints
        // answer an explicit 503 (never fake success); invites read the
        // configured store (durable redb in production).
        .route(
            "/invite/:invite_code",
            get(endpoints::accounts::invite_show),
        )
        .route(
            "/api/v1/emails/confirmations",
            post(endpoints::accounts::email_confirmation_create),
        )
        .route(
            "/api/v1/emails/check_confirmation",
            get(endpoints::accounts::email_confirmation_check),
        )
        .route(
            "/api/v1/emails/password_reset",
            post(endpoints::accounts::password_reset_create),
        )
}

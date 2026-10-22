use crate::api::mastodon::ApiState;
use crate::api::mastodon::endpoints;
use crate::api::oauth::{authorization, token};
use axum::Router;
use axum::routing::{get, post};

pub fn router() -> Router<ApiState> {
    Router::new()
        // OAuth (browser authorize flow + token/revoke).
        .route(
            "/oauth/authorize",
            get(authorization::authorize_get).post(authorization::authorize_post),
        )
        .route("/oauth/token", post(token::token))
        .route("/oauth/revoke", post(token::revoke))
        .route(
            "/oauth/userinfo",
            get(crate::api::oauth::userinfo::show).post(crate::api::oauth::userinfo::show),
        )
        // Email confirmation redemption (D12): browser link from the mailed
        // token; success vs invalid/used shapes, no JS.
        .route(
            "/auth/confirmation",
            get(crate::api::oauth::confirmation::confirmation_get),
        )
        // Password-reset redemption (D12, G-A): browser link from the mailed
        // token; the form posts back to set the password. No JS.
        .route(
            "/auth/password/edit",
            get(endpoints::accounts::password_reset_page),
        )
        .route(
            "/auth/password",
            post(endpoints::accounts::password_reset_update),
        )
}

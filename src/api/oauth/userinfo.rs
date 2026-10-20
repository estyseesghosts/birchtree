//! `GET|POST /oauth/userinfo` — OpenID Connect claims (fix round P3).
//!
//! Upstream (`app/controllers/oauth/userinfo_controller.rb` and
//! `app/serializers/oauth_userinfo_serializer.rb` at pin `8ff11c2b`, wired by
//! `config/routes.rb:36-39` as `match 'userinfo', via: [:get, :post]` inside
//! the Doorkeeper block):
//!
//! * requires the `profile` scope (`doorkeeper_authorize! :profile`) and a
//!   user token (`require_user!`);
//! * renders `{iss, sub, name, preferred_username, profile, picture}` where
//!   `iss` is the site root URL (trailing slash), `sub` the actor URI,
//!   `name` the display name, `profile` the short profile URL and `picture`
//!   the avatar URL.
//!
//! This build stores no avatar blobs, so `picture` is the conventional
//! missing-avatar asset URL — the same placeholder `dto::account_json`
//! already emits (not a fabricated upload). Compiled via a `#[path]` shim in
//! `endpoints::suggestions` until the parent wires `pub mod userinfo;` here +
//! the route.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::ports::Store;

/// The missing-avatar asset path (`dto.rs` `MISSING_AVATAR`; inlined so this
/// module does not depend on the Mastodon DTO packers).
const MISSING_AVATAR: &str = "avatars/original/missing.png";

/// `GET|POST /oauth/userinfo`. One handler serves both methods (upstream
/// `via: [:get, :post]`, OpenID Connect Core §5.3.1); the request body is
/// ignored.
pub async fn show(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["profile"]) {
        return err.into_response();
    }
    let user_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let account = match store.account_by_id(user_id) {
        Ok(Some(account)) => account,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    let base = state.server.public_url.trim_end_matches('/');
    // Upstream `RoutingHelper#short_account_url` / `url_for`: local accounts
    // get the short `https://domain/@username` profile URL.
    let profile = if account.local {
        format!("{base}/@{}", account.username)
    } else {
        account.actor_uri.clone()
    };
    axum::Json(json!({
        // Upstream `root_url` carries the trailing slash.
        "iss": format!("{base}/"),
        "sub": account.actor_uri,
        "name": account.display_name,
        "preferred_username": account.username,
        "profile": profile,
        "picture": state.server.asset_url(MISSING_AVATAR),
    }))
    .into_response()
}

//! Follow suggestions (P1/S1): real ranking over local accounts.
//!
//! Upstream (`app/controllers/api/v1/suggestions_controller.rb` and
//! `app/controllers/api/v2/suggestions_controller.rb` at pin `8ff11c2b`):
//!
//! * v1 `index` renders bare `REST::AccountSerializer` objects with a
//!   `Deprecation` header (`deprecate_api '2021-05-16'`, i.e.
//!   `Deprecation: @1621123200` per `deprecation_concern.rb`); v2 renders
//!   `REST::SuggestionSerializer` (`{source, sources, account}`);
//! * auth: `read`/`read:accounts` for `index`, `write`/`write:accounts` for
//!   `destroy`, plus `require_user!` on every action (an app-only token is
//!   refused);
//! * `destroy` answers `200 {}` via `render_empty`
//!   (`api/base_controller.rb:86-88`) — the old `204` here was wrong;
//! * v2 has NO destroy route (`config/routes/api.rb:265`
//!   `resources :suggestions, only: [:index]`); the v2 `DELETE` route in our
//!   router is a divergence proposed for removal (the handler stays
//!   wire-correct meanwhile).
//!
//! The v1 `DELETE` route is member-shaped upstream
//! (`DELETE /api/v1/suggestions/:id`, `config/routes/api.rb:79`); the
//! dismissal persists in `SUGGESTION_MUTES` (schema v13) and rejoins the
//! ranking exclusion set. v2 exposes no destroy route upstream, so none is
//! wired here.

use axum::extract::{Path, State};
use axum::http::{HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use axum::http::StatusCode;

use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::ports::Store;
use crate::application::suggestions;
use crate::domain::Id;

/// Upstream `deprecate_api '2021-05-16'` renders the date as `@<unix>`
/// (`deprecation_concern.rb`); precedent on disk: `TRENDS_DEPRECATION`.
pub const V1_DEPRECATION: &str = "@1621123200";

fn page_params(uri: &Uri) -> (usize, usize) {
    let pairs = match uri.query() {
        Some(query) => crate::api::forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    suggestions::parse_limit_offset(
        crate::api::forms::get(&pairs, "limit"),
        crate::api::forms::get(&pairs, "offset"),
    )
}

/// `GET /api/v1/suggestions`: bare account objects, deprecated.
pub async fn v1(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:accounts"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (limit, offset) = page_params(&uri);
    let store: &dyn Store = &*state.context.store;
    let ranked =
        match suggestions::rank_suggestions(store, viewer, limit, offset, state.context.now_ms()) {
            Ok(ranked) => ranked,
            Err(err) => return ApiError::from(err).into_response(),
        };
    let body: Vec<serde_json::Value> = ranked
        .iter()
        .map(|entry| {
            dto::account_json_with_stats(&state.server, &entry.account, entry.stats.as_ref())
        })
        .collect();
    let mut resp = axum::Json(body).into_response();
    resp.headers_mut().insert(
        "Deprecation",
        HeaderValue::from_static(V1_DEPRECATION),
    );
    resp
}

/// `GET /api/v2/suggestions`: `{source, sources, account}` objects.
pub async fn v2(State(state): State<ApiState>, auth: Auth, uri: Uri) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:accounts"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (limit, offset) = page_params(&uri);
    let store: &dyn Store = &*state.context.store;
    let ranked =
        match suggestions::rank_suggestions(store, viewer, limit, offset, state.context.now_ms()) {
            Ok(ranked) => ranked,
            Err(err) => return ApiError::from(err).into_response(),
        };
    let body: Vec<serde_json::Value> = ranked
        .iter()
        .map(|entry| {
            json!({
                "source": entry.source.legacy(),
                "sources": [entry.source.as_str()],
                "account": dto::account_json_with_stats(
                    &state.server,
                    &entry.account,
                    entry.stats.as_ref(),
                ),
            })
        })
        .collect();
    axum::Json(body).into_response()
}

/// `DELETE /api/v1/suggestions/:id` (upstream member shape,
/// `config/routes/api.rb:79` at the pin): persist the dismissal, answer
/// `200 {}` via `render_empty`. Unknown ids 404.
pub async fn delete_v1(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:accounts"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let target = match raw.parse::<Id>() {
        Ok(id) => id,
        Err(_) => return ApiError::not_found().into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.dismiss_suggestion(viewer, target, state.context.now_ms()) {
        Ok(_) => (StatusCode::OK, axum::Json(json!({}))).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

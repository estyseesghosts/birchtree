//! `GET /api/v1/directory` — the public profile directory (fix round P3).
//!
//! Upstream (`app/controllers/api/v1/directories_controller.rb` at pin
//! `8ff11c2b`, wired by `config/routes/api.rb:169`
//! `resource :directory, only: [:show]`):
//!
//! * no authentication — anonymous reads are served (`show` has no auth
//!   before-action; a present token only personalizes exclusions);
//! * `404` unless the profile directory is enabled
//!   (`require_enabled!`, `Setting.profile_directory`). This build has no such
//!   setting: the directory is always served (stated divergence, proposed
//!   manifest row);
//! * `?local=1` restricts to local accounts, `?order=new` sorts by id desc,
//!   default (`active`/absent) sorts by recent status
//!   (`account_order_scope`, lines 37-44);
//! * exclusions apply only when authenticated: either-direction blocks and
//!   mutes always, domain-blocks only when remote accounts are included
//!   (lines 24-31; followed accounts are NOT excluded);
//! * `offset` + `limit` paging over `limit_param(DEFAULT_ACCOUNTS_LIMIT)`;
//! * entries render `REST::AccountSerializer` (a bare account array, not
//!   suggestion objects).
//!
//! Ranking/filtering lives in `crate::application::suggestions` (shared with
//! the suggestions engine); this module only authenticates, parses params and
//! serializes. Compiled via a `#[path]` shim in `endpoints::suggestions` until
//! the parent wires `pub mod directory;` + the route.

use axum::extract::State;
use axum::http::Uri;
use axum::response::{IntoResponse, Response};

use crate::api::forms;
use crate::api::mastodon::auth::OptionalAuth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::ports::Store;
use crate::application::suggestions::{self, DirectoryQuery};

/// Upstream `truthy_param?` for `?local=`: anything but an explicit false-like
/// value counts (`streaming::http::truthy` shares the reading; inlined here so
/// this module stays dependency-light).
fn truthy(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "f" | "false" | "off" | "no" | "n"
        ),
    }
}

/// `GET /api/v1/directory`.
pub async fn show(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    // No scope check and no `require_user!`: anonymous reads are served.
    // (`authorize_if_got_token!` semantics come free: `OptionalAuth` rejects
    // a present-but-invalid token with 401 before we run.)
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    let (limit, offset) = suggestions::parse_limit_offset(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "offset"),
    );
    let query = DirectoryQuery {
        local_only: truthy(forms::get(&pairs, "local")),
        order_new: forms::get(&pairs, "order") == Some("new"),
        limit,
        offset,
    };
    let store: &dyn Store = &*state.context.store;
    let entries = match suggestions::directory_accounts(store, viewer, &query, state.context.now_ms())
    {
        Ok(entries) => entries,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let body: Vec<serde_json::Value> = entries
        .iter()
        .map(|(account, stats)| dto::account_json_with_stats(&state.server, account, stats.as_ref()))
        .collect();
    axum::Json(body).into_response()
}

//! Instance endpoints (`GET /api/v1/instance`, `GET /api/v2/instance`).
//!
//! Both are public. They expose static, config-derived facts plus live counts
//! and an optional contact account.

use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::dto::{self, InstanceStats};
use crate::application::ports::Store;
use crate::domain::Account;

/// `GET /api/v1/instance`.
pub async fn v1(State(state): State<ApiState>) -> Response {
    let (stats, contact) = gather(&state);
    axum::Json(dto::instance_v1_json(&state.server, stats, contact.as_ref())).into_response()
}

/// `GET /api/v2/instance`.
pub async fn v2(State(state): State<ApiState>) -> Response {
    let (stats, contact) = gather(&state);
    axum::Json(dto::instance_v2_json(&state.server, stats, contact.as_ref())).into_response()
}

/// Live counts and the resolved contact account, if one is configured.
fn gather(state: &ApiState) -> (InstanceStats, Option<Account>) {
    let store: &dyn Store = &*state.context.store;
    let stats = InstanceStats {
        user_count: store.account_count().unwrap_or(0),
        status_count: 0,
        domain_count: 0,
    };
    let contact = state
        .server
        .site_contact_username
        .as_deref()
        .and_then(|username| {
            let normalized = crate::domain::account::normalize_username(username);
            store
                .account_by_handle(&normalized, &state.server.domain)
                .ok()
                .flatten()
        });
    (stats, contact)
}

use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::support::bool_cast;
use crate::api::forms;
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::application::ports::Store;
use crate::application::relationships;

/// `GET /api/v1/accounts/relationships`.
pub async fn relationships(
    State(state): State<ApiState>,
    auth: Auth,
    RawQuery(query): RawQuery,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:follows"]) {
        return err.into_response();
    }
    let viewer_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };

    let pairs = forms::parse(query.unwrap_or_default().as_bytes());
    let store: &dyn Store = &*state.context.store;

    // Upstream (`relationships_controller.rb`): `without_suspended` unless
    // the `with_suspended` param is truthy; unknown ids are silently skipped.
    let with_suspended = forms::get(&pairs, "with_suspended").is_some_and(bool_cast);
    let mut target_ids: Vec<crate::domain::Id> = Vec::new();
    for raw in forms::all(&pairs, "id") {
        let Ok(id) = raw.parse::<crate::domain::Id>() else {
            continue;
        };
        let include = match store.account_by_id(id).ok().flatten() {
            Some(account) => with_suspended || !account.suspended,
            None => false,
        };
        if include {
            target_ids.push(id);
        }
    }
    match relationships::relationships_for(store, viewer_id, &target_ids, state.context.now_ms()) {
        Ok(relations) => {
            let out: Vec<Value> = relations.iter().map(dto::relationship_json).collect();
            axum::Json(out).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

//! Web Push subscription endpoints (Phase 7).
//!
//! Upstream reference: Mastodon v4.7.3 (`8ff11c2b`)
//! `app/controllers/api/v1/push/subscriptions_controller.rb`
//! (`doorkeeper_authorize! :push`, `require_user!`, `check_push_subscription`
//! on show/update, create scoped to the token with destroy-then-create, update
//! touches only `data`, destroy clears the token's rows) and
//! `REST::WebPushSubscriptionSerializer` (`id`, `endpoint`, `standard`,
//! `alerts`, `server_key`, `policy`).
//!
//! A subscription is bound to the access token that created it (one per
//! token). The `server_key` is the configured VAPID public key. Handlers stay
//! thin; validation lives in `domain::conversation`, delivery in
//! `application::push`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::api::forms::LenientForm;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::ApiState;
use crate::application::conversations::known_alert_types;
use crate::application::ports::Store;
use crate::application::push;
use crate::domain::conversation::{validate_push_endpoint, validate_push_keys};
use crate::domain::Id;

/// Push endpoints take the single `push` scope (upstream
/// `doorkeeper_authorize! :push`).
const PUSH_SCOPES: &[&str] = &["push"];

/// `404 {"error":"Not Found"}` (upstream `check_push_subscription`).
fn no_subscription() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({ "error": "Not Found" })),
    )
        .into_response()
}

fn validation_failed(message: String) -> Response {
    ApiError::unprocessable(message).into_response()
}

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

struct Actor {
    user_id: Id,
    token_id: Id,
}

async fn actor(auth: &Auth) -> ApiResult<Actor> {
    auth.0.require_scopes(PUSH_SCOPES)?;
    let user_id = auth.0.require_user()?;
    Ok(Actor {
        user_id,
        token_id: auth.0.token.id,
    })
}

fn subscription_params(
    pairs: &[(String, String)],
) -> Result<(String, String, String, bool), Response> {
    let endpoint = crate::api::forms::get(pairs, "subscription[endpoint]").unwrap_or("");
    let p256dh = crate::api::forms::get(pairs, "subscription[keys][p256dh]").unwrap_or("");
    let auth = crate::api::forms::get(pairs, "subscription[keys][auth]").unwrap_or("");
    let standard = crate::api::forms::get(pairs, "subscription[standard]")
        .is_some_and(bool_cast);
    if let Err(message) = validate_push_endpoint(endpoint) {
        return Err(validation_failed(message));
    }
    if let Err(message) = validate_push_keys(p256dh, auth) {
        return Err(validation_failed(message));
    }
    Ok((
        endpoint.trim().to_string(),
        p256dh.trim().to_string(),
        auth.trim().to_string(),
        standard,
    ))
}

fn data_params(pairs: &[(String, String)]) -> (String, std::collections::HashMap<String, bool>) {
    let policy = crate::api::forms::get(pairs, "data[policy]").unwrap_or("all");
    let mut alerts = std::collections::HashMap::new();
    for known in known_alert_types() {
        let key = format!("data[alerts][{known}]");
        alerts.insert(
            (*known).to_string(),
            crate::api::forms::get(pairs, &key).is_some_and(bool_cast),
        );
    }
    (policy.to_string(), alerts)
}

/// `POST /api/v1/push/subscription`: create (replacing the token's row).
pub async fn create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(pairs): LenientForm,
) -> Response {
    let actor = match actor(&auth).await {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let (endpoint, p256dh, auth_key, standard) = match subscription_params(&pairs) {
        Ok(params) => params,
        Err(response) => return response,
    };
    let (policy, alerts) = data_params(&pairs);
    let store: &dyn Store = &*state.context.store;
    match store.upsert_push_subscription(
        actor.token_id,
        actor.user_id,
        endpoint,
        p256dh,
        auth_key,
        standard,
        policy,
        alerts,
        Some(state.server.public_url.as_str()),
        state.context.now_ms(),
    ) {
        Ok(subscription) => {
            axum::Json(push::subscription_json(&subscription, &state.server.vapid_public_key))
                .into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/push/subscription`: the token's subscription, if any.
pub async fn show(State(state): State<ApiState>, auth: Auth) -> Response {
    let actor = match actor(&auth).await {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    // Pre-token backfill (gaps G-B): heal the row's bearer token + absolute
    // `Unsubscribe-URL` once a token exists (send-or-mark, never fabricated).
    // A backfill failure never fails the read: the row is still served.
    let _ = crate::application::notifications::backfill_push_unsubscribe(
        store,
        actor.token_id,
        &state.server.public_url,
        state.context.now_ms(),
    );
    match store.push_subscription_by_token(actor.token_id) {
        Ok(Some(subscription)) => {
            axum::Json(push::subscription_json(&subscription, &state.server.vapid_public_key))
                .into_response()
        }
        Ok(None) => no_subscription(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `PUT /api/v1/push/subscription`: replace the token's `data`
/// (policy/alerts); the endpoint/keys are immutable here.
pub async fn update(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(pairs): LenientForm,
) -> Response {
    let actor = match actor(&auth).await {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let (policy, alerts) = data_params(&pairs);
    let store: &dyn Store = &*state.context.store;
    // Pre-token backfill (gaps G-B): same send-or-mark healing as `show`
    // before the data update is applied.
    let _ = crate::application::notifications::backfill_push_unsubscribe(
        store,
        actor.token_id,
        &state.server.public_url,
        state.context.now_ms(),
    );
    match store.update_push_subscription_data(
        actor.token_id,
        policy,
        alerts,
        state.context.now_ms(),
    ) {
        Ok(Some(subscription)) => {
            axum::Json(push::subscription_json(&subscription, &state.server.vapid_public_key))
                .into_response()
        }
        Ok(None) => no_subscription(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/push/subscription`: clear the token's subscription.
/// `200 {}` (upstream `render_empty`).
pub async fn destroy(State(state): State<ApiState>, auth: Auth) -> Response {
    let actor = match actor(&auth).await {
        Ok(actor) => actor,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.delete_push_subscription(actor.token_id) {
        Ok(_) => axum::Json(json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

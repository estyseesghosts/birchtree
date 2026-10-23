use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::support::{WRITE_SCOPES, bool_cast, validation_error, with_no_store};
use crate::api::forms::{self, FormBody};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::application::accounts::{self, RegisterAccountCommand};
use crate::application::ports::Store;
use crate::domain::Error as DomainError;

/// `POST /api/v1/accounts` — public registration.
pub async fn register(
    State(state): State<ApiState>,
    auth: Auth,
    headers: HeaderMap,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    FormBody(pairs): FormBody,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    if let Err(response) = check_registration_gate(&state, &auth, &headers, &peer, &pairs) {
        return response;
    }

    let cmd = parse_registration_command(&pairs);
    let profile = accounts::RegistrationProfile {
        time_zone: cmd.time_zone.clone(),
        date_of_birth: cmd.date_of_birth.clone(),
    };

    // Fail closed when confirmation mail cannot deliver: refuse before the
    // account (and its `SendConfirmationEmail` job) is created, on both the
    // open and the invite path. The endpoint maps this to 403, the same
    // explicit refusal as the resend endpoints' 503.
    if let Err(err) = accounts::check_registration_mail(&state.mail_env) {
        return ApiError::from(err).into_response();
    }

    let store: &dyn Store = &*state.context.store;
    // Invite-aware registration (D12): a usable invite opens closed
    // registrations and attributes the account; unknown/unusable codes are
    // ignored while open. `state.invites` is the durable redb store in
    // production.
    //
    // Policy-aware path (G-E): `time_zone`/`date_of_birth` capture + min-age
    // gate, email-domain/canonical-email blocks reconciled with confirmation
    // state. Delegates to the invite path for everything else.
    match accounts::register(accounts::RegisterInput {
        accounts: store,
        oauth: store,
        base_url: &state.server.public_url,
        now_ms: state.context.now_ms(),
        registration_open: state.server.registrations_open,
        require_approval: state.server.registrations_require_approval,
        app: &auth.0.app,
        cmd,
        invite: Some(accounts::InviteInput {
            invites: &*state.invites,
            enabled: state.server.invites_enabled,
        }),
        policy: Some(accounts::PolicyInput {
            infra: store,
            minimum_age_years: state.server.minimum_age_years,
            profile: &profile,
        }),
    }) {
        Ok(registered) => {
            let body = dto::token_json(
                &registered.token_plaintext,
                &registered.token.scopes,
                registered.token.created_at_ms,
            );
            with_no_store((StatusCode::OK, axum::Json(body)).into_response())
        }
        Err(DomainError::AlreadyExists(_)) => validation_error(
            "Validation failed: Username has already been taken",
            json!({
                "username": [{
                    "error": "ERR_TAKEN",
                    "description": "has already been taken",
                }],
            }),
        ),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Mastodon's client-credentials gate, the invite fail-closed rule and the
/// Phase 7.3 IP sign-up block, in order.
fn check_registration_gate(
    state: &ApiState,
    auth: &Auth,
    headers: &HeaderMap,
    peer: &Option<ConnectInfo<std::net::SocketAddr>>,
    pairs: &[(String, String)],
) -> Result<(), Response> {
    // Mastodon's `require_client_credentials!`.
    if !auth.0.token.is_app_only() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "This method requires an client credentials authentication",
        )
        .into_response());
    }
    // Mastodon's `check_enabled_registrations` answers 403 Forbidden.
    // Upstream parity (`allowed_registration?`): a usable invite opens
    // closed registrations. Unknown/unusable codes while closed stay 403;
    // while open they are ignored by the use case below. The check is a
    // boolean only (no validity oracle: both refusals answer the same 403).
    let invite_opens = state.server.invites_enabled
        && forms::get(pairs, "invite_code").is_some_and(|code| !code.trim().is_empty())
        && forms::get(pairs, "invite_code").is_some_and(|code| {
            accounts::invite_status(&*state.invites, code, state.context.now_ms()).is_ok()
        });
    if !state.server.registrations_open && !invite_opens {
        return Err(ApiError::forbidden("Forbidden").into_response());
    }
    // Phase 7.3 IP sign-up blocks (`sign_up_block` severity): refuse
    // registration from a blocked client IP with the same 403.
    {
        use crate::api::mastodon::endpoints::admin_infra;
        let peer_ip = peer.map(|info| info.0.ip());
        let client =
            admin_infra::request_client_ip(headers, peer_ip, state.trusted_proxies.as_slice());
        if client.is_some_and(|ip| admin_infra::is_sign_up_blocked_for(&state, ip)) {
            return Err(ApiError::forbidden("Forbidden").into_response());
        }
    }
    Ok(())
}

/// Parse the registration form into the use-case command.
fn parse_registration_command(pairs: &[(String, String)]) -> RegisterAccountCommand {
    let field = |key: &str| forms::get(pairs, key).map(str::to_string);
    RegisterAccountCommand {
        username: field("username").unwrap_or_default(),
        email: field("email").unwrap_or_default(),
        password: field("password").unwrap_or_default(),
        agreement: forms::get(pairs, "agreement")
            .map(bool_cast)
            .unwrap_or(false),
        locale: field("locale"),
        reason: field("reason"),
        invite_code: field("invite_code"),
        display_name: None,
        time_zone: field("time_zone"),
        date_of_birth: field("date_of_birth"),
    }
}

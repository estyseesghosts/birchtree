use axum::extract::{FromRequest, Request, State};
use axum::response::{IntoResponse, Response};

use super::credentials::apply_credentials_update;
use super::profile_images::{credential_multipart, is_multipart};
use super::support::{WRITE_SCOPES, credential_response};
use crate::api::forms::FormBody;
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::ApiError;
use crate::application::ports::{AccountProfileUpdate, Store};

// ---------------------------------------------------------------------------
// Profile namespace (gaps batch G-A; 4.6+ alias of the credentials
// endpoints). Router wiring is parent-owned; the proposed routes are:
//   GET    /api/v1/profile          -> profile_show
//   PATCH  /api/v1/profile          -> profile_update
//   DELETE /api/v1/profile/avatar   -> profile_avatar_destroy
//   DELETE /api/v1/profile/header   -> profile_header_destroy
// ---------------------------------------------------------------------------

/// `GET /api/v1/profile` — the caller's own credential account.
///
/// Upstream (`profiles_controller.rb#show`, 4.6+): user token with `profile`
/// or `read:accounts`; the same `CredentialAccount` body as
/// `verify_credentials`.
pub async fn profile_show(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["profile", "read", "read:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    credential_response(&state, account_id)
}

/// `PATCH /api/v1/profile` — same params and semantics as
/// `PATCH /api/v1/accounts/update_credentials` (upstream
/// `profiles_controller.rb#update`, 4.6+; user token + `write:accounts`).
pub async fn profile_update(
    State(state): State<ApiState>,
    auth: Auth,
    request: Request,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let (pairs, avatar_upload, header_upload) = if is_multipart(&request) {
        match credential_multipart(&state, request).await {
            Ok(fields) => (fields.pairs, fields.avatar, fields.header),
            Err(err) => return err.into_response(),
        }
    } else {
        match FormBody::from_request(request, &state).await {
            Ok(FormBody(pairs)) => (pairs, None, None),
            Err(rejection) => return rejection,
        }
    };
    apply_credentials_update(&state, account_id, &pairs, avatar_upload, header_upload).await
}

/// Clear one profile-image slot (`avatar` or `header`) and answer with the
/// credential account body. Success even when no image is set (upstream
/// `profiles_controller.rb#delete_avatar/#delete_header`: "If there were no
/// avatar associated with the profile, the response will still indicate a
/// successful deletion"). The stale blob is dropped best-effort after the
/// commit, like the update path.
async fn clear_profile_image(
    state: &ApiState,
    account_id: crate::domain::Id,
    slot: &str,
) -> Response {
    debug_assert!(slot == "avatar" || slot == "header");
    let store: &dyn Store = &*state.context.store;
    let previous = store
        .account_by_id(account_id)
        .ok()
        .flatten()
        .map(|account| (account.avatar_key, account.header_key));
    let profile = match slot {
        "avatar" => AccountProfileUpdate {
            avatar: Some(None),
            ..Default::default()
        },
        _ => AccountProfileUpdate {
            header: Some(None),
            ..Default::default()
        },
    };
    if let Err(err) = store.update_account_profile(account_id, profile, state.context.now_ms()) {
        return ApiError::from(err).into_response();
    }
    if let Some((old_avatar, old_header)) = previous
        && let Ok(Some(account)) = store.account_by_id(account_id)
    {
        for (old, new) in [
            (old_avatar, account.avatar_key),
            (old_header, account.header_key),
        ] {
            if old != new
                && let Some(key) = old
                && !key.is_empty()
                && let Err(err) = state.context.blobs.delete(&key)
            {
                tracing::warn!(account = account_id.value(), error = %err, "stale profile image left in place");
            }
        }
    }
    credential_response(state, account_id)
}

/// `DELETE /api/v1/profile/avatar` (4.2+; user token + `write:accounts`).
pub async fn profile_avatar_destroy(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    clear_profile_image(&state, account_id, "avatar").await
}

/// `DELETE /api/v1/profile/header` (4.2+; user token + `write:accounts`).
pub async fn profile_header_destroy(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    clear_profile_image(&state, account_id, "header").await
}

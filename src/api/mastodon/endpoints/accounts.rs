//! Account endpoints: registration, profile read/update, credentials and
//! relationships.
//!
//! Contracts follow Mastodon v4.7.3: `POST /api/v1/accounts` requires an
//! app-only (client-credentials) token with `write`/`write:accounts`; read
//! endpoints accept `read`/`read:accounts`; `verify_credentials` and
//! `update_credentials` require a user token.

use axum::body::Body;
use axum::extract::multipart::Field;
use axum::extract::{
    ConnectInfo, Extension, FromRequest, Multipart, Path, RawQuery, Request, State,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::api::forms::{self, FormBody};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::application::accounts::{self, RegisterAccountCommand};
use crate::application::media::human_size;
use crate::application::ports::{
    AccountProfileUpdate, ProfileImageData, Store, UserSettingsUpdate,
};
use crate::application::relationships;
use crate::domain::EmailTokenKind;
use crate::domain::Error as DomainError;
use crate::mailer::{MailEnv, MailError};
use crate::media::staging::StagingGuard;
use crate::media::upload_gate;
use crate::media::upload_spool::{SpoolError, UploadSpool};

/// Scope alternatives accepted by the write endpoints.
const WRITE_SCOPES: &[&str] = &["write", "write:accounts"];

/// Upstream avatar/header image constraints
/// (`Account::Avatar::AVATAR_IMAGE_MIME_TYPES` /
/// `Account::Header::HEADER_IMAGE_MIME_TYPES` at the pin): still images only
/// (the probed still-image decoders minus the birchtree D4 extensions —
/// BMP/TIFF/HEIC/HEIF need the ffmpeg toolchain and upstream avatars reject
/// them), 8 MB each, descriptions capped at 150 chars.
const PROFILE_IMAGE_MIME_TYPES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];
const AVATAR_LIMIT: u64 = 8 * 1024 * 1024;
const HEADER_LIMIT: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_DESCRIPTION_LEN: usize = 150;

/// Attach the caching headers Mastodon uses for token responses.
fn with_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

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
    // Mastodon's `require_client_credentials!`.
    if !auth.0.token.is_app_only() {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "This method requires an client credentials authentication",
        )
        .into_response();
    }
    // Mastodon's `check_enabled_registrations` answers 403 Forbidden.
    // Upstream parity (`allowed_registration?`): a usable invite opens
    // closed registrations. Unknown/unusable codes while closed stay 403;
    // while open they are ignored by the use case below. The check is a
    // boolean only (no validity oracle: both refusals answer the same 403).
    let invite_opens = state.server.invites_enabled
        && forms::get(&pairs, "invite_code").is_some_and(|code| !code.trim().is_empty())
        && forms::get(&pairs, "invite_code").is_some_and(|code| {
            accounts::invite_status(&*state.invites, code, state.context.now_ms()).is_ok()
        });
    if !state.server.registrations_open && !invite_opens {
        return ApiError::forbidden("Forbidden").into_response();
    }
    // Phase 7.3 IP sign-up blocks (`sign_up_block` severity): refuse
    // registration from a blocked client IP with the same 403.
    {
        use crate::api::mastodon::endpoints::admin_infra;
        let peer_ip = peer.map(|info| info.0.ip());
        let client =
            admin_infra::request_client_ip(&headers, peer_ip, state.trusted_proxies.as_slice());
        if client.is_some_and(|ip| admin_infra::is_sign_up_blocked_for(&state, ip)) {
            return ApiError::forbidden("Forbidden").into_response();
        }
    }

    let field = |key: &str| forms::get(&pairs, key).map(str::to_string);
    let username = field("username").unwrap_or_default();
    let email = field("email").unwrap_or_default();
    let password = field("password").unwrap_or_default();
    let agreement = forms::get(&pairs, "agreement")
        .map(bool_cast)
        .unwrap_or(false);

    let cmd = RegisterAccountCommand {
        username,
        email: email.clone(),
        password,
        agreement,
        locale: field("locale"),
        reason: field("reason"),
        invite_code: field("invite_code"),
        display_name: None,
        time_zone: field("time_zone"),
        date_of_birth: field("date_of_birth"),
    };
    let profile = accounts::RegistrationProfile {
        time_zone: field("time_zone"),
        date_of_birth: field("date_of_birth"),
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

/// `GET /api/v1/accounts/{id}` — public account profile.
pub async fn show(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<crate::domain::Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.account_by_id(id) {
        Ok(Some(account)) => {
            // D36: unapproved local accounts are invisible (upstream
            // `check_account_approval` → 404). Remote accounts have no
            // credential row and always render.
            if account.local {
                match store.user_by_account_id(id) {
                    Ok(Some(user)) if !user.approved => {
                        return ApiError::not_found().into_response();
                    }
                    Ok(_) => {}
                    Err(err) => return ApiError::from(err).into_response(),
                }
            }
            let stats = store.account_stats(id).ok();
            axum::Json(dto::account_json_with_stats(
                &state.server,
                &account,
                stats.as_ref(),
            ))
            .into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/verify_credentials`.
pub async fn verify_credentials(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["profile", "read", "read:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    credential_response(&state, account_id)
}

/// `PATCH /api/v1/accounts/update_credentials`.
///
/// Mastodon clients send this endpoint as `multipart/form-data` when an
/// avatar/header file is attached and as `application/x-www-form-urlencoded`
/// (or JSON) otherwise; every encoding carries the same text params. The body
/// is therefore branched on content type: multipart text parts flatten into
/// the same pairs the form path parses, while `avatar`/`header` file parts
/// run the media pipeline below. Urlencoded and JSON bodies keep the exact
/// pre-multipart behavior (including the `415` for other content types).
pub async fn update_credentials(
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

    // Multipart carries files; every other content type keeps the
    // form-or-JSON extractor (and its rejections) untouched.
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

/// Shared `update_credentials` / `PATCH /api/v1/profile` core: validate the
/// flattened params (and any staged avatar/header uploads) and commit the
/// profile + settings patches, answering with the credential account body.
/// Both routes carry the same params and semantics (the profile namespace is
/// the 4.6+ alias of the credentials endpoints).
async fn apply_credentials_update(
    state: &ApiState,
    account_id: crate::domain::Id,
    pairs: &[(String, String)],
    avatar_upload: Option<UploadSpool>,
    header_upload: Option<UploadSpool>,
) -> Response {
    let field = |key: &str| forms::get(&pairs, key);

    let display_name = field("display_name").map(str::to_string);
    if display_name
        .as_deref()
        .is_some_and(|v| v.chars().count() > 40)
    {
        return validation_error(
            "Validation failed: Display name is too long (maximum is 40 characters)",
            json!({}),
        );
    }
    let note = field("note").map(str::to_string);
    if note.as_deref().is_some_and(|v| v.chars().count() > 500) {
        return validation_error(
            "Validation failed: Note is too long (maximum is 500 characters)",
            json!({}),
        );
    }
    // Upstream `credentials_controller.rb#account_params`: `bot`,
    // `fields_attributes[][name|value]` and `attribution_domains[]` are
    // accepted alongside the profile fields above. `avatar`/`header` file
    // parts run the media pipeline below and persist on the account row in
    // the same `update_account_profile` transaction as the rest.
    let bot = field("bot").map(bool_cast);
    // `fields_attributes` absent → leave stored fields unchanged (upstream
    // nested-attributes semantics); present → replace the whole list.
    let profile_fields: Option<Vec<crate::domain::AccountField>> = if pairs
        .iter()
        .any(|(key, _)| key.starts_with("fields_attributes"))
    {
        match parse_profile_fields(&pairs) {
            Ok(fields) => Some(
                fields
                    .into_iter()
                    .map(|(name, value)| crate::domain::AccountField {
                        name,
                        value,
                        verified_at: None,
                    })
                    .collect(),
            ),
            Err(err) => return err,
        }
    } else {
        None
    };
    // Same presence rule for `attribution_domains[]`.
    let attribution_domains: Option<Vec<String>> = pairs
        .iter()
        .any(|(key, _)| key == "attribution_domains" || key == "attribution_domains[]")
        .then(|| {
            forms::all(&pairs, "attribution_domains")
                .into_iter()
                .map(|raw| raw.trim().to_string())
                .filter(|domain| !domain.is_empty())
                // No model length check upstream; the ingest path truncates
                // to the hard limit (`account.rb:87`,
                // `process_account_service.rb:261`).
                .take(256)
                .collect()
        });

    let store: &dyn Store = &*state.context.store;
    // Avatar/header images stage through a [`StagingGuard`]: the guard owns
    // every new blob key until the profile commit succeeds, so a later field
    // validation or a failed account update drops the new keys instead of
    // orphaning `{aa}/{bb}/{rand}/avatar.*` bytes on a 422.
    let mut staging = StagingGuard::new(&*state.context.blobs);
    // Avatar/header images: multipart file parts and present urlencoded/JSON
    // fields share one pipeline. Staging happens BEFORE the profile commit
    // (blob storage cannot join `redb`).
    let avatar = match profile_image_slot(
        state,
        "Avatar",
        AVATAR_LIMIT,
        "avatar",
        avatar_upload,
        field("avatar"),
        &mut staging,
    )
    .await
    {
        Ok(slot) => slot,
        Err(err) => return err,
    };
    let header = match profile_image_slot(
        state,
        "Header",
        HEADER_LIMIT,
        "header",
        header_upload,
        field("header"),
        &mut staging,
    )
    .await
    {
        Ok(slot) => slot,
        Err(err) => return err,
    };
    for (key, label) in [
        ("avatar_description", "Avatar"),
        ("header_description", "Header"),
    ] {
        if field(key).is_some_and(|v| v.chars().count() > MAX_IMAGE_DESCRIPTION_LEN) {
            return validation_error(
                format!(
                    "Validation failed: {label} description is too long (maximum is {MAX_IMAGE_DESCRIPTION_LEN} characters)"
                ),
                json!({}),
            );
        }
    }
    // Previous blob keys, for best-effort cleanup of replaced images after
    // the commit (mirrors the media thumbnail swap: the row commits first,
    // stale bytes are dropped after, never before).
    let previous = store
        .account_by_id(account_id)
        .ok()
        .flatten()
        .map(|account| (account.avatar_key, account.header_key));
    // Core extension CORE-L: top-level `default_license` sets the author's
    // default attachment license. Under `off` the param is
    // accepted-and-ignored; otherwise a present value validates (422 on
    // out-of-enum) and present-blank clears.
    let default_license = match field("default_license") {
        None => None,
        Some(_) if state.server.license_policy == "off" => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Some(None)
            } else {
                match trimmed.parse::<i64>() {
                    Ok(value) => match crate::domain::license::parse_license(Some(value)) {
                        Ok(parsed) => Some(parsed),
                        Err(err) => return ApiError::from(err).into_response(),
                    },
                    Err(_) => {
                        return ApiError::unprocessable(
                            crate::domain::license::ERR_LICENSE_INVALID,
                        )
                        .into_response();
                    }
                }
            }
        }
    };
    let profile = AccountProfileUpdate {
        display_name,
        note,
        locked: field("locked").map(bool_cast),
        discoverable: field("discoverable").map(bool_cast),
        indexable: field("indexable").map(bool_cast),
        hide_collections: field("hide_collections").map(bool_cast),
        fields: profile_fields,
        attribution_domains,
        bot,
        avatar,
        avatar_description: field("avatar_description").map(str::to_string),
        header,
        header_description: field("header_description").map(str::to_string),
        default_license,
        ..Default::default()
    };
    if let Err(err) = store.update_account_profile(account_id, profile, state.context.now_ms()) {
        return ApiError::from(err).into_response();
    }
    // The row commits first: the staged keys are live now, so the guard
    // must not clean them. Stale keys are dropped after, never before.
    staging.commit();
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

    // `source[...]` carries posting preferences.
    let source = |key: &str| forms::get(&pairs, key);
    // D21: `source[chosen_languages][]` (JSON `{"source":
    // `{"chosen_languages": [...]}}` flattens to the same key). Presence —
    // even with no values — replaces the preference; absence leaves it.
    let mut chosen_languages: Option<Vec<String>> = None;
    if pairs
        .iter()
        .any(|(key, _)| key == "source[chosen_languages][]" || key == "source[chosen_languages]")
    {
        let mut raw: Vec<String> = forms::all(&pairs, "source[chosen_languages][]")
            .into_iter()
            .map(str::to_string)
            .collect();
        raw.extend(
            forms::all(&pairs, "source[chosen_languages]")
                .into_iter()
                .map(str::to_string),
        );
        chosen_languages = Some(raw);
    }
    if source("source[privacy]").is_some()
        || source("source[sensitive]").is_some()
        || source("source[language]").is_some()
        || source("source[quote_policy]").is_some()
        || source("source[display_media]").is_some()
        || source("source[expand_spoilers]").is_some()
        || source("source[autoplay_gifs]").is_some()
        || field("home_mention_policy").is_some()
        || chosen_languages.is_some()
    {
        // `display_media` enum (`user_settings.rb:38`); unknown values are
        // refused with the 422 shape upstream's inclusion validation gives.
        let display_media = match source("source[display_media]") {
            None => None,
            Some(raw) => match raw.trim() {
                "default" | "show_all" | "hide_all" => Some(raw.trim().to_string()),
                _ => {
                    return validation_error(
                        "Validation failed: Display media is not included in the list",
                        json!({}),
                    );
                }
            },
        };
        let update = UserSettingsUpdate {
            default_privacy: source("source[privacy]").map(str::to_string),
            default_sensitive: source("source[sensitive]").map(bool_cast),
            default_language: source("source[language]").map(str::to_string),
            default_quote_policy: source("source[quote_policy]").map(str::to_string),
            home_mention_policy: field("home_mention_policy").map(|v| {
                crate::application::policy::HomeMentionPolicy::from_stored(Some(&v))
                    .as_str()
                    .to_string()
            }),
            chosen_languages,
            web_display_media: display_media,
            web_expand_spoilers: source("source[expand_spoilers]").map(bool_cast),
            web_auto_play_gifs: source("source[autoplay_gifs]").map(bool_cast),
        };
        if let Err(err) = store.update_user_settings(account_id, update) {
            return ApiError::from(err).into_response();
        }
    }

    credential_response(state, account_id)
}

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

/// Build the `CredentialAccountSerializer` body for `account_id`.
fn credential_response(state: &ApiState, account_id: crate::domain::Id) -> Response {
    let store: &dyn Store = &*state.context.store;
    let account = match store.account_by_id(account_id) {
        Ok(Some(account)) => account,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    let user = store.user_by_account_id(account_id).ok().flatten();
    let stats = store.account_stats(account_id).ok();
    axum::Json(dto::credential_account_json_with_stats(
        &state.server,
        &account,
        user.as_ref(),
        stats.as_ref(),
    ))
    .into_response()
}

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

/// `GET /api/v1/accounts/:id/identity_proofs` — deprecated upstream (3.5.0:
/// "now returns an empty array"); the only provider was Keybase. This server
/// stores no identity proofs, so the empty array is the real per-account
/// state, not a stub. Requires a user token like upstream (no scope gate is
/// documented); unknown ids 404 like the sibling account reads.
pub async fn identity_proofs_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Response {
    let _account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(target) = id.parse::<crate::domain::Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match store.account_by_id(target) {
        Ok(Some(_)) => axum::Json(json!([])).into_response(),
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// 422 with Mastodon's `ValidationErrorFormatter` shape.
fn validation_error(message: impl Into<String>, details: Value) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(json!({ "error": message.into(), "details": details })),
    )
        .into_response()
}

/// Multipart fields of an `update_credentials` body: every text part flattens
/// into the same pairs the urlencoded path parses (order preserved, so the
/// `fields_attributes[][name]` sequencing keeps working); `avatar`/`header`
/// file parts stream into capped spool files (`Some` spool when the part is
/// present — empty spool when it is present but empty, the clearing
/// signal). Dropping the fields removes the spool files.
struct CredentialMultipart {
    pairs: Vec<(String, String)>,
    avatar: Option<UploadSpool>,
    header: Option<UploadSpool>,
}

/// Parse a `multipart/form-data` credentials body. A malformed frame is a
/// `400` (like the media upload path); content problems inside a well-formed
/// part are `422`s from the image pipeline below, never here.
///
/// File parts stream into a spool capped at the same ceiling the image
/// pipeline enforces (`max_file_size` and the per-image limit, whichever is
/// smaller — the media-ingest ceiling), so an oversized part is rejected
/// before it is resident. The body is paced one frame per task wake (the
/// media upload path's `PacedBody`), so a fast sender cannot make the whole
/// part resident before the spool cap trips.
async fn credential_multipart(
    state: &ApiState,
    request: Request,
) -> ApiResult<CredentialMultipart> {
    let (parts, body) = request.into_parts();
    let paced = Body::from_stream(super::media::PacedBody::new(body.into_data_stream()));
    let request = Request::from_parts(parts, paced);
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?;
    let mut fields = CredentialMultipart {
        pairs: Vec::new(),
        avatar: None,
        header: None,
    };
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?
    {
        let name = field.name().map(str::to_string);
        match name.as_deref() {
            Some("avatar") => {
                fields.avatar = Some(
                    spool_field(
                        state,
                        field,
                        "Avatar",
                        state.server.max_file_size.min(AVATAR_LIMIT),
                    )
                    .await?,
                );
            }
            Some("header") => {
                fields.header = Some(
                    spool_field(
                        state,
                        field,
                        "Header",
                        state.server.max_file_size.min(HEADER_LIMIT),
                    )
                    .await?,
                );
            }
            Some(name) => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?
                    .to_vec();
                fields.pairs.push((
                    name.to_string(),
                    String::from_utf8_lossy(&bytes).into_owned(),
                ));
            }
            None => {}
        }
    }
    Ok(fields)
}

/// Stream one avatar/header part into a capped spool file. The cap is
/// enforced as the bytes arrive, so an oversized part is rejected before it
/// is resident, and every failure path drops the spool (which deletes its
/// file).
async fn spool_field(
    state: &ApiState,
    mut field: Field<'_>,
    label: &str,
    limit: u64,
) -> ApiResult<UploadSpool> {
    let root = state.context.blobs.root_path();
    let mut spool = UploadSpool::create(root.as_deref(), limit)
        .await
        .map_err(|err| spool_error(label, err))?;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|err| ApiError::bad_request(format!("Invalid multipart body: {err}")))?
    {
        spool
            .write_chunk(&chunk)
            .await
            .map_err(|err| spool_error(label, err))?;
    }
    spool
        .finish()
        .await
        .map_err(|err| spool_error(label, err))?;
    Ok(spool)
}

/// Map a spool failure onto the file's existing 422 shape (never 415/500):
/// an over-cap part reports the same size message the image pipeline would.
fn spool_error(label: &str, err: SpoolError) -> ApiError {
    match err {
        SpoolError::TooLarge { limit } => ApiError::unprocessable(format!(
            "Validation failed: {label} file size must be less than {}",
            human_size(limit)
        )),
        SpoolError::Storage(err) => ApiError::from(err),
    }
}

fn is_multipart(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"))
}

/// Resolve one profile-image slot to its `AccountProfileUpdate` presence:
/// `None` = absent (leave stored), `Some(None)` = present-but-empty (clear to
/// the default), `Some(Some(..))` = a staged upload (validate + stage now).
///
/// A present-but-non-empty urlencoded/JSON value carries no bytes and can
/// never become an image, so it 422s instead of silently ignoring the intent
/// (upstream Paperclip likewise refuses a string where a file belongs).
///
/// The spool is read back bounded (never more than the cap it was created
/// with); sniff/decode runs on the bounded media work gate, never on the
/// async worker; bytes stage through `staging`, so dropping it (a later
/// field validation or a failed account update) deletes the new keys.
#[allow(clippy::too_many_arguments)]
async fn profile_image_slot(
    state: &ApiState,
    label: &str,
    limit: u64,
    kind: &str,
    upload: Option<UploadSpool>,
    field: Option<&str>,
    staging: &mut StagingGuard<'_>,
) -> Result<Option<Option<ProfileImageData>>, Response> {
    match upload {
        Some(spool) if !spool.is_empty() => {
            let bytes = spool
                .read_all()
                .map_err(|_| ApiError::internal().into_response())?;
            let max_file_size = state.server.max_file_size;
            let owned_label = label.to_string();
            let (bytes, mime_type, extension) = upload_gate::media_work_gate()
                .run(move || {
                    validate_profile_image(bytes.len(), &bytes, &owned_label, limit, max_file_size)
                        .map(|(mime_type, extension)| (bytes, mime_type, extension))
                })
                .await
                .map_err(|err| ApiError::from(err).into_response())??;
            let hex = crate::application::crypto::random_token_hex(8);
            let key = format!("{}/{}/{}/{}.{extension}", &hex[0..2], &hex[2..4], hex, kind);
            staging
                .put(&key, &bytes)
                .map_err(|err| ApiError::from(err).into_response())?;
            Ok(Some(Some(ProfileImageData {
                key,
                content_type: mime_type.to_string(),
            })))
        }
        Some(_) => Ok(Some(None)),
        None => match field {
            None => Ok(None),
            Some(raw) if raw.is_empty() => Ok(Some(None)),
            Some(_) => Err(validation_error(
                format!("Validation failed: {label} content type is invalid"),
                json!({}),
            )),
        },
    }
}

/// Sniff, cap and decode-check one avatar/header upload: the declared content
/// type is never trusted (magic bytes decide, exactly like media uploads).
/// Failures are the file's existing 422 shape, never 415/500.
fn validate_profile_image(
    len: usize,
    bytes: &[u8],
    label: &str,
    limit: u64,
    max_file_size: u64,
) -> Result<(&'static str, &'static str), Response> {
    let sniffed = crate::media::ingest::sniff(bytes).filter(|sniffed| {
        PROFILE_IMAGE_MIME_TYPES.contains(&sniffed.mime_type) && sniffed.r#type == "image"
    });
    let Some(sniffed) = sniffed else {
        return Err(validation_error(
            format!("Validation failed: {label} content type is invalid"),
            json!({}),
        ));
    };
    // Both caps apply: the operator's `max_file_size` and the upstream
    // per-image limit, whichever is smaller.
    let cap = max_file_size.min(limit);
    if len as u64 > cap {
        let human = human_size(cap);
        return Err(validation_error(
            format!("Validation failed: {label} file size must be less than {human}"),
            json!({}),
        ));
    }
    // Decode-check through the media path (undecodable bytes are an invalid
    // upload, not a server fault). GIF/BMP previews are PNG; only the
    // verdict matters here — avatars serve the staged original.
    match crate::media::processing::process(sniffed.mime_type, bytes) {
        Ok(_) => Ok((sniffed.mime_type, sniffed.extension)),
        Err(crate::media::processing::ProcessError::Unidentifiable(_)) => Err(validation_error(
            format!("Validation failed: {label} is invalid"),
            json!({}),
        )),
        Err(_) => Err(ApiError::internal().into_response()),
    }
}

/// Parse `fields_attributes` profile fields from flattened params.
///
/// Accepts both the form encoding (`fields_attributes[][name|value]`) and the
/// JSON-flattened indexed encoding (`fields_attributes[N][name|value]`,
/// like the filters' `keywords_attributes[N][...]`). Upstream rules
/// (`account.rb:133` max 4 fields; `Account::Field#sanitize` strip + 255
/// chars; `EmptyProfileFieldNamesValidator`) are enforced here with 422s;
/// persistence is the named follow-up (no `Account` storage yet), so valid
/// fields are returned only for validation.
fn parse_profile_fields(pairs: &[(String, String)]) -> Result<Vec<(String, String)>, Response> {
    use std::collections::BTreeMap;
    let mut indexed: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut ordered: Vec<(Option<String>, Option<String>)> = Vec::new();
    for (key, value) in pairs {
        if let Some(rest) = key.strip_prefix("fields_attributes[]") {
            // Form encoding: `fields_attributes[][name]` — pairs arrive in
            // order; a `[name]` starts a new field, `[value]` completes it.
            if rest == "[name]" {
                ordered.push((Some(value.clone()), None));
            } else if rest == "[value]" {
                match ordered.last_mut() {
                    Some(last) if last.1.is_none() => last.1 = Some(value.clone()),
                    _ => ordered.push((None, Some(value.clone()))),
                }
            }
            continue;
        }
        if let Some(rest) = key.strip_prefix("fields_attributes[") {
            // Indexed encoding: `fields_attributes[N][name|value]`.
            let Some((index, field)) = rest.split_once("][") else {
                continue;
            };
            let field = field.trim_end_matches(']');
            if field != "name" && field != "value" {
                continue;
            }
            let entry = indexed.entry(index.to_string()).or_insert((None, None));
            if field == "name" {
                entry.0 = Some(value.clone());
            } else {
                entry.1 = Some(value.clone());
            }
        }
    }
    for (_, entry) in indexed {
        ordered.push(entry);
    }
    // Rails drops fully-blank rows (`reject_if: :all_blank` on the nested
    // attributes). Over-long names/values are truncated, not rejected
    // (`Account::Field#sanitize`: strip + first 255 local chars).
    let truncate = |raw: String| {
        let stripped = raw.trim();
        stripped.chars().take(255).collect::<String>()
    };
    let fields: Vec<(String, String)> = ordered
        .into_iter()
        .map(|(name, value)| {
            (
                truncate(name.unwrap_or_default()),
                truncate(value.unwrap_or_default()),
            )
        })
        .filter(|(name, value)| !(name.is_empty() && value.is_empty()))
        .collect();
    // `account.rb:133`: at most 4 fields (Rails length-validator message).
    if fields.len() > 4 {
        return Err(validation_error(
            "Validation failed: Fields is too long (maximum is 4 characters)",
            json!({}),
        ));
    }
    // `EmptyProfileFieldNamesValidator`: a value without a label 422s.
    if fields
        .iter()
        .any(|(name, value)| name.is_empty() && !value.is_empty())
    {
        return Err(validation_error(
            "Validation failed: Fields contains values with missing labels",
            json!({}),
        ));
    }
    Ok(fields)
}

/// `GET /api/v1/accounts?id[]=…` — multi-fetch up to 40 accounts.
pub async fn index(
    State(state): State<ApiState>,
    auth: crate::api::mastodon::auth::OptionalAuth,
    RawQuery(query): RawQuery,
) -> Response {
    if let Some(authenticated) = &auth.0
        && let Err(err) = authenticated.require_scopes(&["read", "read:accounts"])
    {
        return err.into_response();
    }

    let pairs = forms::parse(query.unwrap_or_default().as_bytes());
    let raw_ids = forms::all(&pairs, "id");

    // Upstream caps at 40.
    if raw_ids.len() > 40 {
        return ApiError::unprocessable("Validation failed: Too many ids").into_response();
    }

    let mut ids: Vec<crate::domain::Id> = Vec::new();
    for raw in raw_ids {
        if let Ok(id) = raw.parse::<crate::domain::Id>() {
            ids.push(id);
        }
    }

    let store: &dyn Store = &*state.context.store;
    match store.accounts_by_ids(&ids) {
        Ok(map) => {
            // Preserve request order; omit unknown accounts. D36: unapproved
            // local accounts are omitted too (upstream `set_accounts` →
            // `without_unapproved`).
            let mut out: Vec<Value> = Vec::new();
            for id in &ids {
                if let Some(account) = map.get(id) {
                    // A store error fails closed (the account is omitted).
                    let unapproved = match store.user_by_account_id(*id) {
                        Ok(user) => user.is_some_and(|u| !u.approved),
                        Err(_) => true,
                    };
                    if account.local && unapproved {
                        continue;
                    }
                    let stats = store.account_stats(*id).ok();
                    out.push(dto::account_json_with_stats(
                        &state.server,
                        account,
                        stats.as_ref(),
                    ));
                }
            }
            axum::Json(out).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/lookup` — resolve an account by acct.
///
/// Upstream (`lookup_controller.rb` + `ResolveAccountService` with
/// `skip_webfinger: true`): a leading `@` is stripped, the handle resolves
/// local accounts and already-known remotes, and suspended accounts are
/// returned (no suspension gate on this route). Remote handles unknown to
/// this server resolve live over webfinger when the caller is authenticated
/// (G-D); 404 only when truly missing, 401 for anonymous remote unknowns.
/// (The v2 search `resolve=` path already attempts live resolution.)
pub async fn lookup(
    State(state): State<ApiState>,
    auth: crate::api::mastodon::auth::OptionalAuth,
    RawQuery(query): RawQuery,
) -> Response {
    if let Some(authenticated) = &auth.0
        && let Err(err) = authenticated.require_scopes(&["read", "read:accounts"])
    {
        return err.into_response();
    }

    let pairs = forms::parse(query.unwrap_or_default().as_bytes());
    let Some(acct) = forms::get(&pairs, "acct") else {
        return ApiError::bad_request("param is missing or the value is empty: acct")
            .into_response();
    };

    // `ResolveAccountService` strips one leading `@`.
    let stripped = acct.strip_prefix('@').unwrap_or(acct);
    let (username, domain) = match stripped.split_once('@') {
        Some((u, d)) => (u.to_string(), d.to_ascii_lowercase()),
        None => (stripped.to_string(), state.server.domain.clone()),
    };

    let store: &dyn Store = &*state.context.store;
    match store.account_by_handle(&username.to_ascii_lowercase(), &domain) {
        Ok(Some(account)) => {
            let stats = store.account_stats(account.id).ok();
            axum::Json(dto::account_json_with_stats(
                &state.server,
                &account,
                stats.as_ref(),
            ))
            .into_response()
        }
        Ok(None) => {
            // G-D live WebFinger: unknown REMOTE handles resolve live over
            // webfinger when the caller is authenticated; 404 only when
            // truly missing. Local unknowns still 404.
            let Some((_, domain)) = crate::activitypub::objects::split_remote_acct(stripped) else {
                return ApiError::not_found().into_response();
            };
            if !crate::activitypub::objects::lookup_should_resolve_live(
                &domain,
                &state.server.domain,
            ) {
                return ApiError::not_found().into_response();
            }
            // Live resolve is an authenticated affordance (never anonymous SSRF).
            if auth.0.is_none() {
                return ApiError::new(StatusCode::UNAUTHORIZED, "Authentication required")
                    .into_response();
            }
            match crate::activitypub::resolve::resolve_handle(&state.federation, stripped).await {
                Ok(Some(account)) => {
                    let stats = store.account_stats(account.id).ok();
                    // Federation tail refresh: a freshly resolved remote
                    // row's counters re-sync from its served collections. An
                    // enqueue failure never fails the lookup itself.
                    if !account.local {
                        let _ = store.enqueue(
                            crate::domain::JobKind::SyncFollowers,
                            serde_json::json!({"account_id": account.id.value()}),
                            state.context.now_ms(),
                        );
                    }
                    axum::Json(dto::account_json_with_stats(
                        &state.server,
                        &account,
                        stats.as_ref(),
                    ))
                    .into_response()
                }
                Ok(None) => ApiError::not_found().into_response(),
                Err(err) => {
                    use crate::activitypub::error::FedError;
                    match &err {
                        FedError::Protocol(
                            activitypub_federation::error::Error::NotFound
                            | activitypub_federation::error::Error::ObjectDeleted(_)
                            | activitypub_federation::error::Error::WebfingerResolveFailed(_),
                        )
                        | FedError::Domain(crate::domain::Error::NotFound(_))
                        | FedError::Gone(_) => ApiError::not_found().into_response(),
                        _ => ApiError::from(crate::domain::Error::from(err)).into_response(),
                    }
                }
            }
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/search` — search accounts by query.
///
/// Upstream (`accounts/search_controller.rb` + `AccountSearchService`):
/// requires a user token; `q` blank (after `@`-strip) or `limit < 1`
/// returns `[]`; an exact handle match (`user@domain`, `@`-prefixed or
/// not) sorts first when `offset` is zero; `following=true` restricts both
/// the exact match and the substring results to followed accounts;
/// `resolve=true` resolves a remote handle over federation.
/// Default limit 40, max 80 (`limit_param(DEFAULT_ACCOUNTS_LIMIT)`).
pub async fn search(
    State(state): State<ApiState>,
    auth: Auth,
    RawQuery(query): RawQuery,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:accounts"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };

    let pairs = forms::parse(query.unwrap_or_default().as_bytes());
    let raw_q = forms::get(&pairs, "q").unwrap_or("");
    let q = raw_q.strip_prefix('@').unwrap_or(raw_q).trim().to_string();
    let limit: usize = match forms::get(&pairs, "limit") {
        None => 40,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            if value < 1 {
                return axum::Json(Vec::<Value>::new()).into_response();
            }
            (value as usize).min(80)
        }
    };
    let offset: usize = match forms::get(&pairs, "offset") {
        None => 0,
        Some(raw) => match raw.parse::<i64>() {
            Ok(value) if value >= 0 => value as usize,
            _ => {
                return ApiError::bad_request(
                    "Pagination values for `offset` and `limit` must be positive",
                )
                .into_response();
            }
        },
    };
    let following = forms::get(&pairs, "following").is_some_and(bool_cast);
    let resolve = forms::get(&pairs, "resolve").is_some_and(bool_cast);

    let store: &dyn Store = &*state.context.store;
    if q.is_empty() {
        return axum::Json(Vec::<Value>::new()).into_response();
    }

    // Exact handle match first (upstream `exact_match`, `offset.zero?` only).
    let mut exact: Option<crate::domain::Account> = None;
    if offset == 0 && q.contains('@') {
        let (username, domain) = match q.split_once('@') {
            Some((u, d)) => (u.to_ascii_lowercase(), d.to_ascii_lowercase()),
            None => (q.to_ascii_lowercase(), state.server.domain.clone()),
        };
        if domain == state.server.domain {
            exact = store.account_by_handle(&username, &domain).ok().flatten();
        } else {
            exact = store.account_by_handle(&username, &domain).ok().flatten();
            // `resolve=true` on a remote handle: fetch it over federation
            // (mirrors the v2 search resolve path; unknown handles add
            // nothing, like upstream's `ResolveAccountService` miss).
            if exact.is_none() && resolve {
                match crate::activitypub::resolve::resolve_handle(&state.federation, &q).await {
                    Ok(account) => exact = account,
                    Err(err) => tracing::info!(error = %err, "remote account did not resolve"),
                }
            }
        }
        // `following=true` gates the exact match too.
        if following && let Some(ref account) = exact {
            let followed = match store.follow_of(viewer, account.id) {
                Ok(Some(follow)) => follow.state == crate::domain::FollowState::Active,
                _ => false,
            };
            if !followed {
                exact = None;
            }
        }
        if exact.as_ref().is_some_and(|account| account.suspended) {
            exact = None;
        }
    }

    let needle = q.to_ascii_lowercase();
    let mut out: Vec<Value> = Vec::new();
    if let Some(ref account) = exact {
        let stats = store.account_stats(account.id).ok();
        out.push(dto::account_json_with_stats(
            &state.server,
            account,
            stats.as_ref(),
        ));
    }
    // Substring results fill the remainder (`limit - 1` when exact matched).
    let room = limit.saturating_sub(out.len());
    if room > 0 {
        match store.search_accounts(&needle, 500) {
            Ok(candidates) => {
                let exact_id = exact.as_ref().map(|account| account.id);
                let mut skipped = 0usize;
                let mut taken = 0usize;
                for account in candidates {
                    if Some(account.id) == exact_id || account.suspended {
                        continue;
                    }
                    if following {
                        let followed = match store.follow_of(viewer, account.id) {
                            Ok(Some(follow)) => follow.state == crate::domain::FollowState::Active,
                            _ => false,
                        };
                        if !followed {
                            continue;
                        }
                    }
                    // `offset` pages the substring results (the exact match
                    // only exists at offset zero).
                    if skipped < offset {
                        skipped += 1;
                        continue;
                    }
                    if taken >= room {
                        break;
                    }
                    let stats = store.account_stats(account.id).ok();
                    out.push(dto::account_json_with_stats(
                        &state.server,
                        &account,
                        stats.as_ref(),
                    ));
                    taken += 1;
                }
            }
            Err(err) => return ApiError::from(err).into_response(),
        }
    }
    axum::Json(out).into_response()
}

// ---------------------------------------------------------------------------
// Invites + email (round 3, area A; D12 remainder closed here).
//
// Router wiring is parent-owned; the proposed routes are in the area report:
//   GET  /invite/:invite_code               -> invite_show
//   GET  /auth/confirmation                  -> confirmation::confirmation_get
//   POST /api/v1/emails/confirmations       -> email_confirmation_create
//   GET  /api/v1/emails/check_confirmation  -> email_confirmation_check
//   POST /api/v1/emails/password_reset      -> password_reset_create (public)
// ---------------------------------------------------------------------------

/// Map a [`MailError`] to an explicit HTTP refusal. Disabled mail is `503`
/// (never a silent success); transport failure is `502`; a bad address is
/// `422`.
fn mail_error_response(err: MailError) -> Response {
    match err {
        MailError::Disabled(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({ "error": "Email delivery is not configured" })),
        )
            .into_response(),
        MailError::InvalidRecipient(message) => {
            ApiError::unprocessable(format!("email {message}")).into_response()
        }
        MailError::Transport(_) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(json!({ "error": "Email delivery failed" })),
        )
            .into_response(),
    }
}

/// `GET /invite/:invite_code` — mirrors `Api::V1::InvitesController#show`
/// (`app/controllers/api/v1/invites_controller.rb:19-29`): `200
/// {invite_code, instance_api_url}`, `404` for an unknown code (`find_by!`),
/// `401 {error: "Invalid invitation code"}` for a known-but-unusable one
/// (`check_valid_usage!`, before the registrations gate, as upstream).
pub async fn invite_show(
    State(state): State<ApiState>,
    Extension(invites): Extension<Arc<dyn accounts::InviteStore>>,
    Path(code): Path<String>,
) -> Response {
    match accounts::invite_status(&*invites, &code, state.context.now_ms()) {
        Ok(_) => axum::Json(json!({
            "invite_code": code,
            "instance_api_url": format!("{}/api/v2/instance", state.server.public_url),
        }))
        .into_response(),
        Err(DomainError::NotFound(_)) => ApiError::not_found().into_response(),
        Err(DomainError::Unauthorized(message)) => {
            ApiError::new(StatusCode::UNAUTHORIZED, message).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/emails/confirmations` — mirrors
/// `Api::V1::Emails::ConfirmationsController#create`
/// (`app/controllers/api/v1/emails/confirmations_controller.rb:13-18`):
/// re-sends the confirmation mail; `200 {}`. Requires a user token with
/// `write`/`write:accounts` and an unconfirmed address (`403` otherwise, like
/// upstream `require_user_not_confirmed!`), and the caller's app must own the
/// signup (`403` on mismatch, like upstream
/// `require_user_owned_by_application!`; pre-attribution rows with no recorded
/// app are grandfathered). Without a mail provider the
/// answer is an explicit `503`, never a fake success. Delivery is a durable
/// `SendConfirmationEmail` job (worker sends after commit, like upstream
/// `deliver_later`); the token is random per request and is stored durably
/// BEFORE the job enqueues (a crash between the two leaves a harmless orphan
/// token, never a mailed link with no backing row).
pub async fn email_confirmation_create(
    State(state): State<ApiState>,
    auth: Auth,
    Extension(env): Extension<Arc<MailEnv>>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["write", "write:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let user = match store.user_by_account_id(account_id) {
        Ok(Some(user)) => user,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    if let Err(err) = accounts::check_confirmation_ownership(&user, auth.0.app.id) {
        return ApiError::from(err).into_response();
    }
    if user.email_verified {
        return ApiError::forbidden(
            "This method is only available while the e-mail is awaiting confirmation",
        )
        .into_response();
    }
    let Some(email) = user.email else {
        return ApiError::unprocessable("email address is missing").into_response();
    };
    if !env.enabled {
        return mail_error_response(MailError::Disabled(
            "no SMTP provider configured (set smtp_url and mail_from)",
        ));
    }
    if let Err(err) = crate::mailer::validate_address(&email) {
        return mail_error_response(err);
    }
    // The store mints the random bearer, persists only its digest and queues
    // the mail job in one transaction (no orphan token, no plaintext at rest).
    let now_ms = state.context.now_ms();
    match store.issue_email_token(account_id, &email, EmailTokenKind::Confirmation, now_ms) {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/emails/check_confirmation` — mirrors
/// `Api::V1::Emails::ConfirmationsController#check`: `200 true`/`false` for
/// whether the caller's address is confirmed. User token, `read` scope. The
/// caller's app must own the signup (`403` on mismatch, like upstream
/// `require_user_owned_by_application!`; pre-attribution rows are
/// grandfathered).
pub async fn email_confirmation_check(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(&["read", "read:accounts"]) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match store.user_by_account_id(account_id) {
        Ok(Some(user)) => {
            if let Err(err) = accounts::check_confirmation_ownership(&user, auth.0.app.id) {
                return ApiError::from(err).into_response();
            }
            axum::Json(json!(user.email_verified)).into_response()
        }
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/emails/password_reset` — public forgot-password flow
/// (`200 {}` always, when mail is configured).
///
/// This replaces the earlier authenticated-only extension. Upstream has no API
/// reset route at all (password reset is web-only: `POST /auth/password`,
/// Devise `PasswordsController#create`, which is unauthenticated and always
/// redirects visibly-successful). The Devise parity shape is therefore a
/// public endpoint taking an `email` param: a known address stores a token
/// and queues a `SendPasswordResetEmail` job; an unknown address queues
/// nothing — and every case answers the same `200 {}` so the endpoint is
/// never an account-enumeration oracle. (The authed-only variant is dropped
/// because it matches neither upstream surface: no client that speaks the
/// Devise flow can use it, and it cannot serve a user who forgot their
/// password but still holds a token.)
///
/// When no mail provider is configured the answer is the same explicit `503`
/// for known and unknown addresses alike (still no oracle: the response
/// carries no information about the address).
pub async fn password_reset_create(
    State(state): State<ApiState>,
    Extension(env): Extension<Arc<MailEnv>>,
    FormBody(pairs): FormBody,
) -> Response {
    let email_param = forms::get(&pairs, "email").unwrap_or("").trim().to_string();
    let now_ms = state.context.now_ms();
    if !env.enabled {
        return mail_error_response(MailError::Disabled(
            "no SMTP provider configured (set smtp_url and mail_from)",
        ));
    }
    let store: &dyn Store = &*state.context.store;
    let account = match store.account_by_email(&email_param) {
        Ok(account) => account,
        Err(err) => return ApiError::from(err).into_response(),
    };
    // Unknown address: queue nothing, answer 200 (no enumeration oracle).
    let Some(account) = account else {
        return axum::Json(json!({})).into_response();
    };
    let address = match store.user_by_account_id(account.id) {
        Ok(Some(user)) => user.email,
        Ok(None) => None,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let Some(address) = address else {
        return axum::Json(json!({})).into_response();
    };
    // A stored address that no longer validates is dropped silently (answering
    // 422 here would turn the endpoint into an oracle for known addresses).
    if crate::mailer::validate_address(&address).is_err() {
        return axum::Json(json!({})).into_response();
    }
    match store.issue_email_token(account.id, &address, EmailTokenKind::PasswordReset, now_ms) {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Password-reset redemption (gaps batch G-A; closes the D12 remainder).
//
// The mailed "forgot password" link (`SendPasswordResetEmail` job) lands on
// `GET /auth/password/edit?reset_password_token=…` (Devise
// `PasswordsController#edit`, which renders the form without checking the
// token) and submits to `POST /auth/password` (Devise `#update`, here POST
// per the frozen shape). Bearer lifetime is
// `Config::password_reset_ttl_hours` (default 6h); redemption consumes the
// single-use bearer; unknown/used/expired/wrong-kind tokens answer one
// indistinguishable 422 page (never an oracle). No new mailer shape.
// Router wiring is parent-owned; the proposed routes are:
//   GET  /auth/password/edit -> password_reset_page
//   POST /auth/password      -> password_reset_update
// ---------------------------------------------------------------------------

/// Query shape for the mailed link: `?reset_password_token=…`.
#[derive(Debug, serde::Deserialize)]
pub struct PasswordResetQuery {
    pub reset_password_token: Option<String>,
}

/// Bearer lifetime in ms for `ttl_hours` (`Config::password_reset_ttl_hours`).
pub fn reset_bearer_ttl_ms(ttl_hours: u64) -> u64 {
    ttl_hours.saturating_mul(3_600_000)
}

/// Validate a candidate new password (+ confirmation) BEFORE the bearer is
/// touched: an invalid password must not consume the single-use token
/// (upstream Devise clears the reset token only on a valid save, so a typo
/// never burns the link).
pub fn validate_reset_password(password: &str, confirmation: &str) -> Result<(), String> {
    if password != confirmation {
        return Err("Password confirmation doesn't match".to_string());
    }
    if password.chars().count() < accounts::MIN_PASSWORD_LEN {
        return Err(format!(
            "Password is too short (minimum is {} characters)",
            accounts::MIN_PASSWORD_LEN
        ));
    }
    Ok(())
}

/// Whether a stored reset bearer is still live at `now_ms`.
pub fn reset_bearer_live(created_at_ms: u64, now_ms: u64, ttl_hours: u64) -> bool {
    now_ms.saturating_sub(created_at_ms) <= reset_bearer_ttl_ms(ttl_hours)
}

fn reset_page(status: StatusCode, title: &str, heading: &str, body: &str) -> Response {
    use crate::api::oauth::consent_page::escape;
    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title></head><body><main><h1>{}</h1><p>{}</p></main></body></html>\n",
        escape(title),
        escape(heading),
        body,
    );
    (status, axum::response::Html(html)).into_response()
}

/// The set-password form. `error` re-renders the form with a message (422)
/// keeping the token in the hidden field so the user can retry without
/// burning the link; `None` is the fresh form (200).
fn reset_form_page(token: &str, error: Option<&str>) -> Response {
    use crate::api::oauth::consent_page::escape;
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let error_html = error
        .map(|message| format!("<p role=\"alert\">{}</p>", escape(message)))
        .unwrap_or_default();
    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Set a new password</title></head><body><main>\
         <h1>Set a new password</h1>{error_html}\
         <form method=\"post\" action=\"/auth/password\">\
         <input type=\"hidden\" name=\"reset_password_token\" value=\"{}\">\
         <label>New password <input type=\"password\" name=\"password\" autocomplete=\"new-password\" required minlength=\"8\"></label>\
         <label>Confirm new password <input type=\"password\" name=\"password_confirmation\" autocomplete=\"new-password\" required minlength=\"8\"></label>\
         <button type=\"submit\">Change my password</button>\
         </form></main></body></html>\n",
        escape(token),
    );
    (status, axum::response::Html(html)).into_response()
}

/// The one indistinguishable invalid/used/expired-token page (422, never an
/// oracle distinguishing unknown from used).
fn reset_invalid_page() -> Response {
    reset_page(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Password reset failed",
        "Password reset failed",
        "This password reset link is invalid or has already been used. \
         Request a new password reset email and try again.",
    )
}

/// Look up a reset param in both the flat shape (`password`) and the Devise
/// nested shape (`user[password]`).
fn reset_param<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    forms::get(pairs, key)
        .or_else(|| forms::get(pairs, &format!("user[{key}]")))
        .unwrap_or("")
}

/// `GET /auth/password/edit?reset_password_token=…` — the server-rendered
/// set-password form. Like Devise's `#edit`, the token is not checked here;
/// it rides the hidden field and is validated on submit.
pub async fn password_reset_page(
    axum::extract::Query(query): axum::extract::Query<PasswordResetQuery>,
) -> Response {
    reset_form_page(query.reset_password_token.as_deref().unwrap_or(""), None)
}

/// `POST /auth/password` — consume the bearer and set the new password.
pub async fn password_reset_update(
    State(state): State<ApiState>,
    FormBody(pairs): FormBody,
) -> Response {
    let token = reset_param(&pairs, "reset_password_token");
    let password = reset_param(&pairs, "password");
    let confirmation = reset_param(&pairs, "password_confirmation");
    if token.is_empty() {
        return reset_invalid_page();
    }
    // Password problems answer before the bearer is touched, so a typo
    // never burns the single-use link.
    if let Err(message) = validate_reset_password(password, confirmation) {
        return reset_form_page(token, Some(&message));
    }
    let store: &dyn Store = &*state.context.store;
    let now_ms = state.context.now_ms();
    let password_hash = match crate::application::crypto::hash_password(password) {
        Ok(hash) => hash,
        Err(_) => return ApiError::internal().into_response(),
    };
    // One transaction validates kind and lifetime, burns the bearer and sets
    // the password: a failure cannot consume the link without the change.
    let ttl_ms = reset_bearer_ttl_ms(state.server.password_reset_ttl_hours);
    match accounts::reset_password(store, token, password_hash, now_ms, ttl_ms) {
        Ok(_) => reset_page(
            StatusCode::OK,
            "Password changed",
            "Password changed",
            "Your password has been changed. You can now sign in.",
        ),
        // Unknown, used, expired, wrong-kind or vanished account: one page,
        // so the answer never says which case occurred.
        Err(crate::domain::Error::NotFound(_) | crate::domain::Error::InvalidInput(_)) => {
            reset_invalid_page()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

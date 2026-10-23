use axum::extract::{FromRequest, Request, State};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::profile_images::{
    credential_multipart, is_multipart, parse_profile_fields, profile_image_slot,
};
use super::support::{
    AVATAR_LIMIT, HEADER_LIMIT, MAX_IMAGE_DESCRIPTION_LEN, WRITE_SCOPES, bool_cast,
    credential_response, validation_error,
};
use crate::api::forms::{self, FormBody};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::ApiError;
use crate::application::ports::{AccountProfileUpdate, Store, UserSettingsUpdate};
use crate::media::staging::StagingGuard;
use crate::media::upload_spool::UploadSpool;

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
pub(super) async fn apply_credentials_update(
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

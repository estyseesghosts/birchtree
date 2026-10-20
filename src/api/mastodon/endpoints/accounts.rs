//! Account endpoints: registration, profile read/update, credentials and
//! relationships.
//!
//! Contracts follow Mastodon v4.7.3: `POST /api/v1/accounts` requires an
//! app-only (client-credentials) token with `write`/`write:accounts`; read
//! endpoints accept `read`/`read:accounts`; `verify_credentials` and
//! `update_credentials` require a user token.

use axum::extract::{ConnectInfo, Extension, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::api::forms::{self, FormBody};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::accounts::{self, RegisterAccountCommand};
use crate::application::ports::{
    AccountProfileUpdate, Store, UserSettingsUpdate,
};
use crate::application::relationships;
use crate::domain::Error as DomainError;
use crate::domain::{EmailToken, EmailTokenKind};
use crate::mailer::{MailEnv, MailError};

/// Scope alternatives accepted by the write endpoints.
const WRITE_SCOPES: &[&str] = &["write", "write:accounts"];

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
    if !state.server.registrations_open {
        return ApiError::forbidden("Forbidden").into_response();
    }
    // Phase 7.3 IP sign-up blocks (`sign_up_block` severity): refuse
    // registration from a blocked client IP with the same 403.
    {
        use crate::api::mastodon::endpoints::admin_infra;
        let peer_ip = peer.map(|info| info.0.ip());
        let client = admin_infra::request_client_ip(
            &headers,
            peer_ip,
            state.trusted_proxies.as_slice(),
        );
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
        email,
        password,
        agreement,
        locale: field("locale"),
        reason: field("reason"),
        invite_code: field("invite_code"),
        display_name: None,
    };

    let store: &dyn Store = &*state.context.store;
    // Invite-aware registration (D12): a usable invite opens closed
    // registrations and attributes the account; unknown/unusable codes are
    // ignored while open. `state.invites` is the durable redb store in
    // production.
    match accounts::register_account_with_invite(
        store,
        store,
        &*state.invites,
        &state.server.public_url,
        state.context.now_ms(),
        state.server.registrations_open,
        state.server.registrations_require_approval,
        state.server.invites_enabled,
        &auth.0.app,
        cmd,
    ) {
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
pub async fn update_credentials(
    State(state): State<ApiState>,
    auth: Auth,
    FormBody(pairs): FormBody,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let account_id = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };

    let field = |key: &str| forms::get(&pairs, key);

    let display_name = field("display_name").map(str::to_string);
    if display_name.as_deref().is_some_and(|v| v.chars().count() > 40) {
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
    // accepted alongside the profile fields above (`avatar`/`header` need
    // the media pipeline — named follow-up). They persist on the account row
    // in the same `update_account_profile` transaction as the rest.
    let bot = field("bot").map(bool_cast);
    // `fields_attributes` absent → leave stored fields unchanged (upstream
    // nested-attributes semantics); present → replace the whole list.
    let profile_fields: Option<Vec<crate::domain::AccountField>> =
        if pairs.iter().any(|(key, _)| key.starts_with("fields_attributes")) {
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
    };
    if let Err(err) = store.update_account_profile(account_id, profile) {
        return ApiError::from(err).into_response();
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
                    )
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

    credential_response(&state, account_id)
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

/// 422 with Mastodon's `ValidationErrorFormatter` shape.
fn validation_error(message: &str, details: Value) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(json!({ "error": message, "details": details })),
    )
        .into_response()
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
    if fields.iter().any(|(name, value)| name.is_empty() && !value.is_empty()) {
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
/// this server 404 here — live webfinger resolution is the federation
/// follow-up (named; the v2 search `resolve=` path already attempts it).
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
        Ok(None) => ApiError::not_found().into_response(),
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
            exact = store
                .account_by_handle(&username, &domain)
                .ok()
                .flatten();
        } else {
            exact = store
                .account_by_handle(&username, &domain)
                .ok()
                .flatten();
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
        if following
            && let Some(ref account) = exact
        {
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
                            Ok(Some(follow)) => {
                                follow.state == crate::domain::FollowState::Active
                            }
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
    // Random per request (unguessable, unlike the deterministic registration
    // token); the job payload carries it to the worker.
    let token = crate::application::crypto::random_token_hex(32);
    let now_ms = state.context.now_ms();
    if let Err(err) = store.store_email_token(
        &token,
        EmailToken {
            account_id,
            kind: EmailTokenKind::Confirmation,
            created_at_ms: now_ms,
        },
    ) {
        return ApiError::from(err).into_response();
    }
    match store.enqueue(
        crate::domain::JobKind::SendConfirmationEmail,
        json!({
            "account_id": account_id.value(),
            "email": email,
            "token": token,
        }),
        now_ms,
    ) {
        Ok(_) => axum::Json(json!({})).into_response(),
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
    let token = crate::application::crypto::random_token_hex(32);
    // Durable first, as on the confirmation resend: a crash between the two
    // leaves a harmless orphan token, never a mailed link with no backing row.
    if let Err(err) = store.store_email_token(
        &token,
        EmailToken {
            account_id: account.id,
            kind: EmailTokenKind::PasswordReset,
            created_at_ms: now_ms,
        },
    ) {
        return ApiError::from(err).into_response();
    }
    match store.enqueue(
        crate::domain::JobKind::SendPasswordResetEmail,
        json!({
            "account_id": account.id.value(),
            "email": address,
            "token": token,
        }),
        now_ms,
    ) {
        Ok(_) => axum::Json(json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

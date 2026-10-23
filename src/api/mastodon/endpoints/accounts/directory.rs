use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::support::bool_cast;
use crate::api::forms;
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::application::ports::Store;

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
    let window = match parse_search_window(&pairs) {
        Ok(window) => window,
        Err(response) => return response,
    };
    if window.q.is_empty() {
        return axum::Json(Vec::<Value>::new()).into_response();
    }

    let store: &dyn Store = &*state.context.store;
    // Exact handle match first (upstream `exact_match`, `offset.zero?` only).
    let mut exact: Option<crate::domain::Account> =
        resolve_exact_match(&state, store, &window).await;
    // `following=true` gates the exact match too.
    if window.following
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

    let needle = window.q.to_ascii_lowercase();
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
    let room = window.limit.saturating_sub(out.len());
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
                    if window.following {
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
                    if skipped < window.offset {
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

/// Parsed `GET /api/v1/accounts/search` window.
struct SearchWindow {
    q: String,
    limit: usize,
    offset: usize,
    following: bool,
    resolve: bool,
}

fn parse_search_window(pairs: &[(String, String)]) -> Result<SearchWindow, Response> {
    let raw_q = forms::get(pairs, "q").unwrap_or("");
    let q = raw_q.strip_prefix('@').unwrap_or(raw_q).trim().to_string();
    let limit: usize = match forms::get(pairs, "limit") {
        None => 40,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            if value < 1 {
                return Err(axum::Json(Vec::<Value>::new()).into_response());
            }
            (value as usize).min(80)
        }
    };
    let offset: usize = match forms::get(pairs, "offset") {
        None => 0,
        Some(raw) => match raw.parse::<i64>() {
            Ok(value) if value >= 0 => value as usize,
            _ => {
                return Err(ApiError::bad_request(
                    "Pagination values for `offset` and `limit` must be positive",
                )
                .into_response());
            }
        },
    };
    let following = forms::get(pairs, "following").is_some_and(bool_cast);
    let resolve = forms::get(pairs, "resolve").is_some_and(bool_cast);
    Ok(SearchWindow {
        q,
        limit,
        offset,
        following,
        resolve,
    })
}

async fn resolve_exact_match(
    state: &ApiState,
    store: &dyn Store,
    window: &SearchWindow,
) -> Option<crate::domain::Account> {
    if window.offset != 0 || !window.q.contains('@') {
        return None;
    }
    let (username, domain) = match window.q.split_once('@') {
        Some((u, d)) => (u.to_ascii_lowercase(), d.to_ascii_lowercase()),
        None => (window.q.to_ascii_lowercase(), state.server.domain.clone()),
    };
    let mut exact = if domain == state.server.domain {
        store.account_by_handle(&username, &domain).ok().flatten()
    } else {
        store.account_by_handle(&username, &domain).ok().flatten()
    };
    // `resolve=true` on a remote handle: fetch it over federation
    // (mirrors the v2 search resolve path; unknown handles add
    // nothing, like upstream's `ResolveAccountService` miss).
    if exact.is_none() && window.resolve {
        match crate::activitypub::resolve::resolve_handle(&state.federation, &window.q).await {
            Ok(account) => exact = account,
            Err(err) => tracing::info!(error = %err, "remote account did not resolve"),
        }
    }
    exact
}

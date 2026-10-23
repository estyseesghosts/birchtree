//! Search endpoints (Phase 4): `GET /api/v2/search`.
//!
//! Mastodon v4.7.3 removed `GET /api/v1/search`, so only v2 exists here. All
//! four result keys are always present; an unknown `type` returns empty
//! arrays; unauthenticated pagination/resolve answer the pinned 401 bodies.

use axum::extract::State;
use axum::http::Uri;
use axum::response::{IntoResponse, Response};

use crate::api::forms;
use crate::api::mastodon::auth::OptionalAuth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::search;
use crate::application::ports::Store;
use crate::domain::Id;

/// Read scopes for authenticated search.
const READ_SCOPES: &[&str] = &["read", "read:search"];

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// `GET /api/v2/search`.
pub async fn index(State(state): State<ApiState>, auth: OptionalAuth, uri: Uri) -> Response {
    if let Some(authenticated) = &auth.0
        && let Err(err) = authenticated.require_scopes(READ_SCOPES)
    {
        return err.into_response();
    }
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };

    // `params.require(:q)` → 400 with the ParameterMissing message.
    let query = match forms::get(&pairs, "q") {
        Some(raw) => match search::require_query(Some(raw)) {
            Ok(query) => query.to_string(),
            Err(_) => {
                return ApiError::bad_request("param is missing or the value is empty: q")
                    .into_response();
            }
        },
        None => {
            return ApiError::bad_request("param is missing or the value is empty: q")
                .into_response();
        }
    };

    // Pagination: negative limit/offset → 400 (Mastodon
    // `pagination_params`); `offset` without auth → 401.
    for key in ["limit", "offset"] {
        if let Some(raw) = forms::get(&pairs, key)
            && raw.trim().starts_with('-')
        {
            return ApiError::bad_request(
                "Pagination values for `offset` and `limit` must be positive",
            )
            .into_response();
        }
    }
    let offset: usize = forms::get(&pairs, "offset")
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0);
    if offset > 0 && viewer.is_none() {
        return ApiError::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "Search queries pagination is not supported without authentication",
        )
        .into_response();
    }
    if bool_cast(forms::get(&pairs, "resolve").unwrap_or("false")) && viewer.is_none() {
        return ApiError::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "Search queries that resolve remote resources are not supported without authentication",
        )
        .into_response();
    }

    let limit_raw = forms::get(&pairs, "limit");
    let limit: usize = match limit_raw {
        None => search::DEFAULT_LIMIT,
        Some(raw) => {
            let value: i64 = raw.parse().unwrap_or(0);
            if value < 0 {
                return ApiError::bad_request(
                    "Pagination values for `offset` and `limit` must be positive",
                )
                .into_response();
            }
            (value as usize).min(search::MAX_LIMIT)
        }
    };

    let kind = forms::get(&pairs, "type").map(str::to_string);
    let following = bool_cast(forms::get(&pairs, "following").unwrap_or("false"));
    let account_id = match forms::get(&pairs, "account_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => None,
        None => None,
    };
    let max_id = match forms::get(&pairs, "max_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        _ => None,
    };
    let min_id = match forms::get(&pairs, "min_id").map(|raw| raw.parse::<Id>()) {
        Some(Ok(id)) => Some(id),
        _ => None,
    };

    let store: &dyn Store = &*state.context.store;
    // `resolve=true` on a remote handle: fetch it over federation and add it to
    // the accounts. A handle that cannot be resolved adds nothing (upstream's
    // `ResolveAccountService` also returns no account, without an error).
    let resolve = bool_cast(forms::get(&pairs, "resolve").unwrap_or("false"));
    let handle_search = kind.as_deref().is_none_or(|k| k == "accounts");
    let resolved = if resolve && viewer.is_some() && handle_search && query.contains('@') {
        match crate::activitypub::resolve::resolve_handle(&state.federation, &query).await {
            Ok(account) => account,
            Err(err) => {
                tracing::info!(error = %err, "remote account did not resolve");
                None
            }
        }
    } else {
        None
    };
    // A status URL (as Mastodon's `perform_url_search`): a status already stored is returned, and with
    // `resolve=true` a remote one is fetched. The show gate decides whether the viewer may see it.
    let status_search = kind.as_deref().is_none_or(|k| k == "statuses");
    let url_status = if viewer.is_some() && status_search && (query.starts_with("http://") || query.starts_with("https://")) {
        let known = store.status_by_uri(&query).ok().flatten();
        let status = match known {
            Some(status) => Some(status),
            None if resolve => match crate::activitypub::resolve::resolve_status(&state.federation, &query).await {
                Ok(status) => Some(status),
                Err(err) => {
                    tracing::info!(error = %err, "remote status did not resolve");
                    None
                }
            },
            None => None,
        };
        status.and_then(|status| {
            crate::application::statuses::show_status(store, viewer, status.id, state.context.now_ms()).ok()
        })
    } else {
        None
    };
    match search::search_with_mode(
        state.server.search_mode,
        store,
        Some(crate::search::global()),
        &crate::application::search_query::SearchQuery::new(
            viewer,
            &query,
            kind.as_deref(),
            offset,
            limit,
            following,
            account_id,
            max_id,
            min_id,
        ),
    ) {
        Ok(results) => {
            let mut found = results.accounts;
            if let Some(account) = resolved {
                if !found.iter().any(|a| a.id == account.id) {
                    found.insert(0, account);
                }
            }
            let accounts: Vec<serde_json::Value> = found
                .iter()
                .map(|account| dto::account_json(&state.server, account))
                .collect();
            let mut found_statuses = results.statuses;
            if let Some(status) = url_status {
                if !found_statuses.iter().any(|s| s.id == status.id) {
                    found_statuses.insert(0, status);
                }
            }
            let statuses = match found_statuses.is_empty() {
                true => Vec::new(),
                false => {
                    match crate::api::mastodon::status_view::load_status_views(
                        &state.server,
                        store,
                        viewer,
                        found_statuses,
                        state.context.now_ms(),
                    ) {
                        Ok(views) => views
                            .iter()
                            .map(|view| {
                                dto::status_json(
                                    &state.server,
                                    viewer,
                                    state.context.now_ms(),
                                    view,
                                    dto::StatusSourceMode::Content,
                                )
                            })
                            .collect(),
                        Err(err) => return err.into_response(),
                    }
                }
            };
            let hashtags: Vec<serde_json::Value> = results
                .hashtags
                .iter()
                .map(|tag| dto::hashtag_json(&state.server, tag, viewer))
                .collect();
            axum::Json(dto::search_json(accounts, statuses, hashtags)).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

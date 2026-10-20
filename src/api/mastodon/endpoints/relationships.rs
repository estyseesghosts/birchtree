//! Relationship write endpoints: follow/unfollow, block/unblock, mute/unmute,
//! remove-from-followers, account notes, endorsements, follow requests,
//! domain blocks and the follower/following account lists.
//!
//! Every write endpoint answers `200` with a fresh
//! `REST::RelationshipSerializer` (Mastodon semantics); list endpoints return
//! `REST::AccountSerializer[]` with `Link` pagination headers.

use axum::extract::{Path, State};
use axum::http::{Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::pagination;
use crate::api::mastodon::ApiState;
use crate::application::ports::Store;
use crate::application::relationships;
use crate::application::streaming::StreamEvent;
use crate::application::timelines;
use crate::domain::{Account, AccountStats, Id, Status};

/// Scope alternatives for follow/unfollow/follow-request writes.
const FOLLOW_SCOPES: &[&str] = &["follow", "write", "write:follows"];
/// Scope alternatives for block writes.
const BLOCK_SCOPES: &[&str] = &["follow", "write", "write:blocks"];
/// Scope alternatives for mute writes.
const MUTE_SCOPES: &[&str] = &["follow", "write", "write:mutes"];
/// Scope alternatives for notes/endorsement writes.
const WRITE_ACCOUNT_SCOPES: &[&str] = &["write", "write:accounts"];
/// Scope alternatives for account list reads.
const READ_ACCOUNT_SCOPES: &[&str] = &["read", "read:accounts"];

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Render the relationship of `viewer` to `target_id` (200).
fn relationship_response(state: &ApiState, viewer: Id, target_id: Id) -> Response {
    let store: &dyn Store = &*state.context.store;
    match relationships::relationship_for(store, viewer, target_id, state.context.now_ms()) {
        Ok(Some(relationship)) => axum::Json(dto::relationship_json(&relationship)).into_response(),
        // The target vanished between validation and read; Mastodon would 404.
        Ok(None) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Resolve the `{id}` path segment against the store (404 `Record not found`).
fn require_account(store: &dyn Store, raw: &str) -> ApiResult<Account> {
    let id = raw
        .parse::<Id>()
        .map_err(|_| ApiError::not_found())?;
    store
        .account_by_id(id)
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::not_found)
        .map(|account| account)
}

/// `POST /api/v1/accounts/{id}/follow`.
pub async fn follow(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(FOLLOW_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    if !target.local {
        // A remote target is followed by federation: a signed Follow, pending until the Accept.
        return federated_follow(&state, viewer, &target).await;
    }
    let pairs = body;
    let cmd = relationships::FollowCommand {
        follower_id: viewer,
        target_id: target.id,
        show_reblogs: forms::get(&pairs, "reblogs").map(bool_cast),
        notify: forms::get(&pairs, "notify").map(bool_cast),
        languages: {
            let langs = forms::all(&pairs, "languages[]");
            if langs.is_empty() {
                None
            } else {
                Some(langs.into_iter().map(str::to_string).collect())
            }
        },
    };
    match relationships::follow(store, state.context.now_ms(), cmd) {
        Ok(_) => relationship_response(&state, viewer, target.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Follow a remote account: the block check of the local path, then the federated request.
async fn federated_follow(state: &ApiState, viewer: crate::domain::Id, target: &Account) -> Response {
    let store: &dyn Store = &*state.context.store;
    let follower = match store.account_by_id(viewer) {
        Ok(Some(account)) => account,
        Ok(None) => return ApiError::not_found().into_response(),
        Err(err) => return ApiError::from(err).into_response(),
    };
    match store.pair_state(viewer, target.id, state.context.now_ms()) {
        Ok(pair) if pair.blocking || pair.blocked_by => {
            return ApiError::from(crate::domain::Error::Forbidden("This action is not allowed".into()))
                .into_response();
        }
        Ok(_) => {}
        Err(err) => return ApiError::from(err).into_response(),
    }
    match crate::activitypub::outbound::send_follow(&state.federation, &follower, target).await {
        Ok(_) => relationship_response(state, viewer, target.id),
        Err(err) => ApiError::from(crate::domain::Error::from(err)).into_response(),
    }
}

/// `POST /api/v1/accounts/{id}/unfollow`.
pub async fn unfollow(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(FOLLOW_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    if !target.local {
        // A remote target gets a signed Undo; the follow row goes in the same transaction.
        let follower = match store.account_by_id(viewer) {
            Ok(Some(account)) => account,
            Ok(None) => return ApiError::not_found().into_response(),
            Err(err) => return ApiError::from(err).into_response(),
        };
        return match crate::activitypub::outbound::send_unfollow(&state.federation, &follower, &target).await {
            Ok(removed) => {
                if removed.is_some() {
                    publish_policy(&state, vec![viewer], None);
                }
                relationship_response(&state, viewer, target.id)
            }
            Err(err) => ApiError::from(crate::domain::Error::from(err)).into_response(),
        };
    }
    match relationships::unfollow(store, viewer, target.id, state.context.now_ms()) {
        Ok(removed) => {
            if removed.is_some() {
                publish_policy(&state, vec![viewer], None);
            }
            relationship_response(&state, viewer, target.id)
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/accounts/{id}/remove_from_followers`.
pub async fn remove_from_followers(
    State(state): State<ApiState>,
    auth: Auth,
    Path(follower): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(FOLLOW_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let follower = match require_account(store, &follower) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    match relationships::remove_follower(store, viewer, follower.id, state.context.now_ms()) {
        Ok(removed) => {
            // The follower's home stream loses the viewer's statuses.
            if removed.is_some() {
                publish_policy(&state, vec![follower.id], None);
            }
            relationship_response(&state, viewer, follower.id)
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Publish a live policy change after its commit (see `StreamEvent::PolicyChanged`).
/// A retraction follows the policy event, so its frames carry the new epoch.
fn publish_policy(state: &ApiState, viewers: Vec<Id>, retraction: Option<(Id, Vec<Status>)>) {
    state
        .context
        .streaming
        .publish(StreamEvent::PolicyChanged { viewers });
    if let Some((viewer, statuses)) = retraction {
        if !statuses.is_empty() {
            state
                .context
                .streaming
                .publish(StreamEvent::Retract { viewer, statuses });
        }
    }
}

/// Statuses a block or mute of `target` removes from `viewer`'s open streams,
/// read before the commit. A read error skips the retraction: the stale ids
/// stay until the client reconnects; no new status is exposed.
fn retractions(store: &dyn Store, viewer: Id, target: Id) -> Option<(Id, Vec<Status>)> {
    match timelines::retraction_candidates(store, viewer, target) {
        Ok(statuses) => Some((viewer, statuses)),
        Err(err) => {
            tracing::warn!(error = ?err, "retraction candidates unavailable");
            None
        }
    }
}

/// Shared shape for `POST /api/v1/accounts/{id}/block|unblock|mute|unmute`.
async fn relationship_write(
    state: &ApiState,
    auth: Auth,
    target: String,
    scopes: &'static [&'static str],
    action: fn(&dyn Store, Id, Id, u64) -> crate::domain::Result<()>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(scopes) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    match action(store, viewer, target.id, state.context.now_ms()) {
        Ok(()) => relationship_response(&state, viewer, target.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/accounts/{id}/block`.
///
/// Live policy: the block unfollows both ways, so both sides' open streams drop
/// the other's statuses. The blocker's open streams also drop the statuses a
/// block removes from their feeds (upstream `clear_from_home`).
pub async fn block(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(BLOCK_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    // Before the commit: the block unfollows, so the feed's authors change.
    let retraction = retractions(store, viewer, target.id);
    match relationships::block(store, viewer, target.id, state.context.now_ms()) {
        Ok(_) => {
            publish_policy(&state, vec![viewer, target.id], retraction);
            relationship_response(&state, viewer, target.id)
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/accounts/{id}/unblock`.
pub async fn unblock(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
) -> Response {
    relationship_write(&state, auth, target, BLOCK_SCOPES, |store, viewer, target, now| {
        relationships::unblock(store, viewer, target, now).map(|_| ())
    })
    .await
}


/// `POST /api/v1/accounts/{id}/mute` — `duration` (seconds, 0 = permanent)
/// and `notifications` (absent/false => hide notifications).
pub async fn mute(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(MUTE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    let pairs = body;
    let duration = forms::get(&pairs, "duration")
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(0);
    let hide_notifications = !forms::get(&pairs, "notifications").is_some_and(bool_cast);
    // Before the commit, as for block.
    let retraction = retractions(store, viewer, target.id);
    match relationships::mute(
        store,
        viewer,
        target.id,
        duration,
        hide_notifications,
        state.context.now_ms(),
    ) {
        Ok(_) => {
            publish_policy(&state, vec![viewer], retraction);
            relationship_response(&state, viewer, target.id)
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/accounts/{id}/unmute`.
pub async fn unmute(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(MUTE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    match relationships::unmute(store, viewer, target.id, state.context.now_ms()) {
        Ok(_) => relationship_response(&state, viewer, target.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/accounts/{id}/note` — the viewer's private comment
/// (blank deletes the note).
pub async fn note(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    let pairs = body;
    let comment = forms::get(&pairs, "comment").unwrap_or("");
    match relationships::set_account_note(
        store,
        viewer,
        target.id,
        comment,
        state.context.now_ms(),
    ) {
        Ok(_) => relationship_response(&state, viewer, target.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Endorse a followed account: `POST /api/v1/accounts/{id}/pin` and `/endorse`.
pub async fn endorse(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    match relationships::endorse(store, viewer, target.id, state.context.now_ms()) {
        Ok(_) => relationship_response(&state, viewer, target.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Withdraw an endorsement: `POST /api/v1/accounts/{id}/unpin|/unendorse`
/// (idempotent no-op when not endorsed).
pub async fn unendorse(
    State(state): State<ApiState>,
    auth: Auth,
    Path(target): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    match relationships::unendorse(store, viewer, target.id) {
        Ok(_) => relationship_response(&state, viewer, target.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Account list rendering with per-account counters.
fn accounts_json(
    state: &ApiState,
    store: &dyn Store,
    accounts: &[Account],
) -> Vec<Value> {
    accounts
        .iter()
        .map(|account| {
            let stats: Option<AccountStats> = store.account_stats(account.id).ok();
            dto::account_json_with_stats(&state.server, account, stats.as_ref())
        })
        .collect()
}

/// Wrap an account page in a `Link`-headed 200 response. `next`/`prev` are
/// emitted like Mastodon: `next` while the page is full, `prev` whenever the
/// page is non-empty.
fn accounts_page(
    uri: &Uri,
    page: &[Account],
    limit: usize,
    store: &dyn Store,
    state: &ApiState,
) -> Response {
    let next = (page.len() == limit)
        .then(|| page.last().map(|a| a.id))
        .flatten();
    let prev = page.first().map(|a| a.id);
    let mut response = axum::Json(accounts_json(state, store, page)).into_response();
    if let Some(link) = pagination::link_header(uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

/// Scope check for optional-auth reads: anonymous passes; a presented token
/// must carry an accepted scope (Mastodon `authorize_if_got_token!`).
fn optional_scopes(auth: &OptionalAuth, required: &[&str]) -> Result<(), ApiError> {
    match &auth.0 {
        Some(authenticated) => authenticated.require_scopes(required),
        None => Ok(()),
    }
}

fn parse_page(uri: &Uri, default: usize, max: usize) -> ApiResult<pagination::Page> {
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        default,
        max,
    )
}

/// `GET /api/v1/follow_requests` — pending inbound requests as accounts.
pub async fn follow_requests(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["follow", "read", "read:follows"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri, 40, 80) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let requests = match relationships::list_follow_requests(
        store,
        viewer,
        page.max_id,
        page.limit,
    ) {
        Ok(requests) => requests,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut accounts = Vec::with_capacity(requests.len());
    for request in &requests {
        match store.account_by_id(request.follower_id) {
            Ok(Some(account)) => accounts.push(account),
            Ok(None) => continue,
            Err(err) => return ApiError::from(err).into_response(),
        }
    }
    accounts_page(&uri, &accounts, page.limit, store, &state)
}

/// `POST /api/v1/follow_requests/{id}/authorize`.
pub async fn authorize_request(
    State(state): State<ApiState>,
    auth: Auth,
    Path(follower): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(FOLLOW_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let follower = match require_account(store, &follower) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    match relationships::authorize_request(store, viewer, follower.id, state.context.now_ms()) {
        Ok(_) => relationship_response(&state, viewer, follower.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/follow_requests/{id}/reject`.
pub async fn reject_request(
    State(state): State<ApiState>,
    auth: Auth,
    Path(follower): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(FOLLOW_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let follower = match require_account(store, &follower) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    match relationships::reject_request(store, viewer, follower.id, state.context.now_ms()) {
        Ok(_) => relationship_response(&state, viewer, follower.id),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/domain_blocks` — the viewer's blocked domains, newest first.
///
/// Paged like Mastodon's `paginate_by_max_id`: `max_id`/`since_id` are
/// exclusive row-id cursors, the limit defaults to 100 and is capped at 200,
/// `next` is present on a full page and `prev` on a non-empty page.
pub async fn domain_blocks_index(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    uri: Uri,
) -> Response {
    if let Err(err) = optional_scopes(&auth, &["follow", "read", "read:blocks"]) {
        return err.into_response();
    }
    let Some(viewer) = auth.0.as_ref().and_then(|a| a.token.user_id) else {
        return ApiError::requires_user().into_response();
    };
    let page = match parse_page(&uri, 100, 200) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let blocks = match relationships::list_domain_blocks(
        store,
        viewer,
        page.max_id,
        page.since_id,
        page.limit,
    ) {
        Ok(blocks) => blocks,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let full = blocks.len() == page.limit;
    let next = if full { blocks.last().map(|b| b.id) } else { None };
    let prev = blocks.first().map(|b| b.id);
    let domains: Vec<String> = blocks.into_iter().map(|b| b.domain).collect();
    let mut response = axum::Json(domains).into_response();
    if let Some(value) = pagination::link_header_desc(&uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&value) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

/// `POST /api/v1/domain_blocks` — block a domain (`200 {}`). The `domain`
/// parameter comes from the form body or the query string, as Rails merges
/// them. A missing or blank domain is a 422 `Validation failed: Domain can't
/// be blank`.
pub async fn domain_blocks_create(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["follow", "write", "write:blocks"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let query = forms::parse(uri.query().unwrap_or("").as_bytes());
    let domain = forms::get(&body, "domain")
        .or_else(|| forms::get(&query, "domain"))
        .unwrap_or("")
        .to_string();
    let store: &dyn Store = &*state.context.store;
    match relationships::domain_block(store, viewer, &domain, state.context.now_ms()) {
        Ok(_) => {
            publish_policy(&state, vec![viewer], None);
            axum::Json(serde_json::json!({})).into_response()
        }
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `DELETE /api/v1/domain_blocks` — unblock a domain (`200 {}`, idempotent).
/// The `domain` parameter comes from the query string or the form body.
pub async fn domain_blocks_delete(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(&["follow", "write", "write:blocks"]) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let query = forms::parse(uri.query().unwrap_or("").as_bytes());
    let domain = forms::get(&body, "domain")
        .or_else(|| forms::get(&query, "domain"))
        .unwrap_or("");
    let store: &dyn Store = &*state.context.store;
    match relationships::unblock_domain(store, viewer, domain) {
        Ok(_) => axum::Json(serde_json::json!({})).into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `GET /api/v1/accounts/{id}/followers`.
pub async fn followers(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(target): Path<String>,
    uri: Uri,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    account_edge_list(&state, &auth, target, uri, Edge::Followers).await
}

/// `GET /api/v1/accounts/{id}/following`.
pub async fn following(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(target): Path<String>,
    uri: Uri,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    account_edge_list(&state, &auth, target, uri, Edge::Following).await
}

#[derive(Clone, Copy)]
enum Edge {
    Followers,
    Following,
}

async fn account_edge_list(
    state: &ApiState,
    _auth: &OptionalAuth,
    target: String,
    uri: Uri,
    edge: Edge,
) -> Response {
    let page = match parse_page(&uri, 40, 80) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let target = match require_account(store, &target) {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    let ids = match edge {
        Edge::Followers => relationships::followers_of(store, target.id, page.max_id, page.limit),
        Edge::Following => relationships::following_of(store, target.id, page.max_id, page.limit),
    };
    let ids = match ids {
        Ok(ids) => ids,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut accounts = Vec::with_capacity(ids.len());
    for id in &ids {
        match store.account_by_id(*id) {
            Ok(Some(account)) => accounts.push(account),
            Ok(None) => continue,
            Err(err) => return ApiError::from(err).into_response(),
        }
    }
    accounts_page(&uri, &accounts, page.limit, store, state)
}

/// `GET /api/v1/accounts/{id}/endorsements` — accounts this account endorses
/// (empty array when the account is unknown).
pub async fn account_endorsements(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(target): Path<String>,
    uri: Uri,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let page = match parse_page(&uri, 40, 80) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let Ok(target_id) = target.parse::<Id>() else {
        // Unknown account: Mastodon answers an empty array.
        return axum::Json(Vec::<Value>::new()).into_response();
    };
    if store.account_by_id(target_id).ok().flatten().is_none() {
        return axum::Json(Vec::<Value>::new()).into_response();
    }
    let ids = match relationships::list_endorsements(store, target_id, page.max_id, page.limit) {
        Ok(ids) => ids,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let accounts = load_accounts(store, &ids);
    accounts_page(&uri, &accounts, page.limit, store, &state)
}

/// `GET /api/v1/endorsements` — the viewer's endorsed accounts.
pub async fn endorsements_index(
    State(state): State<ApiState>,
    auth: Auth,
    uri: Uri,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_ACCOUNT_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let page = match parse_page(&uri, 40, 80) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    let ids = match relationships::list_endorsements(store, viewer, page.max_id, page.limit) {
        Ok(ids) => ids,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let accounts = load_accounts(store, &ids);
    accounts_page(&uri, &accounts, page.limit, store, &state)
}

/// Load accounts for ids, silently skipping ids that no longer resolve.
fn load_accounts(store: &dyn Store, ids: &[Id]) -> Vec<Account> {
    ids.iter()
        .filter_map(|id| store.account_by_id(*id).ok().flatten())
        .collect()
}

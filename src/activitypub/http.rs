//! Federation HTTP routes: WebFinger, NodeInfo, the actor document and the
//! inboxes. Each route is a thin wrapper over the crate: `receive_activity` for
//! the inboxes, `extract_webfinger_name` and `build_webfinger_response` for
//! WebFinger, `FederationJson` for the actor document.
//!
//! Outbound fetches and deliveries are the crate's too (see `federation::delivery`).

use activitypub_federation::{
    axum::{
        inbox::{receive_activity, ActivityData},
        json::FederationJson,
    },
    config::{Data, FederationConfig, FederationMiddleware},
    fetch::webfinger::{build_webfinger_response, extract_webfinger_name, Webfinger},
    protocol::context::WithContext,
    traits::Object,
};
use axum::{
    extract::{Path, Query},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::activitypub::actor::{ActorRecord, Person};
use crate::activitypub::activities::Activity;
use crate::activitypub::error::FedError;
use crate::activitypub::FedContext;
use crate::application::accounts::host_from_url;
use crate::domain::account::normalize_username;
use crate::domain::Error as DomainError;

/// The federation routes, with the crate's middleware that makes `Data<FedContext>` available.
pub fn routes<S>(config: FederationConfig<FedContext>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/.well-known/webfinger", get(webfinger))
        .route("/.well-known/nodeinfo", get(nodeinfo_links))
        .route("/nodeinfo/2.0", get(nodeinfo))
        .route("/actor", get(instance_actor))
        .route("/users/:username", get(actor))
        .route("/users/:username/statuses/:id", get(note))
        .route("/users/:username/quote_authorizations/:id", get(quote_authorization))
        .route("/users/:username/followers", get(followers))
        .route("/users/:username/following", get(following))
        .route("/users/:username/outbox", get(outbox))
        .route("/users/:username/inbox", post(personal_inbox))
        .route("/inbox", post(shared_inbox))
        .layer(FederationMiddleware::new(config))
}

/// Under authorized fetch, a read of one of our documents needs a valid HTTP
/// signature from an actor this server can resolve. Without it: 401.
async fn require_signed(
    data: &Data<FedContext>,
    headers: &axum::http::HeaderMap,
    method: &axum::http::Method,
    uri: &axum::http::Uri,
) -> Result<(), FedError> {
    if !data.authorized_fetch {
        return Ok(());
    }
    let unauthorized = || FedError::Protocol(activitypub_federation::error::Error::ActivitySignatureInvalid);
    let signature = headers
        .get("signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    // keyId="https://host/users/name#main-key": the key's owner is the actor.
    let key_id = signature
        .split(',')
        .find_map(|part| part.trim().strip_prefix("keyId=").map(|v| v.trim_matches('"').to_string()))
        .ok_or_else(unauthorized)?;
    let actor_url = key_id.split('#').next().unwrap_or_default();
    let actor_url = url::Url::parse(actor_url).map_err(|_| unauthorized())?;
    let actor = activitypub_federation::fetch::object_id::ObjectId::<ActorRecord>::from(actor_url)
        .dereference(data)
        .await?;
    activitypub_federation::http_signatures::verify_signature(
        headers.iter(),
        method,
        uri,
        &actor.keys.public_key_pem,
    )
    .map_err(|_| unauthorized())?;
    Ok(())
}

fn not_found(what: &str) -> FedError {
    FedError::Domain(DomainError::NotFound(what.to_string()))
}

#[derive(Deserialize)]
pub struct WebfingerQuery {
    resource: String,
}

/// `GET /.well-known/webfinger?resource=acct:name@domain`.
pub async fn webfinger(
    Query(query): Query<WebfingerQuery>,
    data: Data<FedContext>,
) -> Result<impl IntoResponse, FedError> {
    let username = extract_webfinger_name(&query.resource, &data)?.to_string();
    let actor = ActorRecord::local_by_username(&data, &username)
        .await?
        .ok_or_else(|| not_found("account"))?;
    // Upstream answers 410 for a suspended account (`check_account_suspension`
    // → `gone if @account.permanently_unavailable?`).
    if actor.account.suspended {
        return Err(FedError::Gone(format!("acct:{}@{}", actor.account.username, data.domain())));
    }
    let subject = format!("acct:{}@{}", actor.account.username, data.domain());
    let body: Webfinger = build_webfinger_response(subject, actor.url().clone());
    Ok(([(header::CONTENT_TYPE, "application/jrd+json")], Json(body)))
}

/// `GET /users/{username}`: the `Person` document, as `application/activity+json`.
pub async fn actor(
    Path(username): Path<String>,
    data: Data<FedContext>,
) -> Result<FederationJson<WithContext<Person>>, FedError> {
    let record = ActorRecord::local_by_username(&data, &username)
        .await?
        .ok_or_else(|| not_found("account"))?;
    let person = record.into_json(&data).await?;
    Ok(FederationJson(WithContext::new_default(person)))
}

/// `GET /.well-known/nodeinfo`: the link to the 2.0 document.
pub async fn nodeinfo_links(data: Data<FedContext>) -> Json<serde_json::Value> {
    Json(json!({
        "links": [{
            "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
            "href": format!("{}/nodeinfo/2.0", data.public_url),
        }]
    }))
}

/// `GET /nodeinfo/2.0`. The `usage` block carries real local-only counts
/// (round 3, D31): local accounts, unique password sign-ins over the trailing
/// 4 and 24 weeks (upstream `active_user_count(4/24)` over
/// `activity:logins`), and non-deleted statuses by local authors (upstream
/// `local_status_count`). A failed scan degrades that counter to zero rather
/// than failing the document; every counter is measured, never fabricated.
pub async fn nodeinfo(data: Data<FedContext>) -> Json<serde_json::Value> {
    let (users_total, active_month, active_halfyear, local_posts) = nodeinfo_usage(&data);
    Json(json!({
        "version": "2.0",
        "software": {"name": "birchtree", "version": env!("CARGO_PKG_VERSION")},
        "protocols": ["activitypub"],
        "services": {"inbound": [], "outbound": []},
        "usage": {
            "users": {
                "total": users_total,
                "activeMonth": active_month,
                "activeHalfyear": active_halfyear,
            },
            "localPosts": local_posts,
        },
        "openRegistrations": data.registrations_open,
        "metadata": {"nodeName": &*data.site_title, "nodeDescription": &*data.site_description}
    }))
}

/// `(local users, active 4w, active 24w, local posts)` for NodeInfo.
fn nodeinfo_usage(data: &Data<FedContext>) -> (u64, u64, u64, u64) {
    use std::collections::HashSet;
    const SCAN_CAP: usize = 200_000;
    const WEEK_MS: u64 = 7 * 86_400_000;
    let store = &data.app.store;
    let now = data.app.now_ms();
    let accounts = store.admin_accounts_overview(SCAN_CAP).unwrap_or_default();
    let local_ids: HashSet<crate::domain::Id> = accounts
        .iter()
        .filter(|account| account.local)
        .map(|account| account.id)
        .collect();
    let sign_ins = store.admin_sign_ins(SCAN_CAP).unwrap_or_default();
    let unique_since = |since_ms: u64| {
        sign_ins
            .iter()
            .filter(|(_, at)| *at >= since_ms && *at <= now)
            .map(|(who, _)| *who)
            .collect::<HashSet<_>>()
            .len() as u64
    };
    let posts = store
        .admin_status_metrics(SCAN_CAP)
        .map(|rows| {
            rows.iter()
                .filter(|row| local_ids.contains(&row.account_id))
                .count() as u64
        })
        .unwrap_or(0);
    (
        local_ids.len() as u64,
        unique_since(now.saturating_sub(4 * WEEK_MS)),
        unique_since(now.saturating_sub(24 * WEEK_MS)),
        posts,
    )
}

/// `POST /inbox`: the shared inbox. Any local recipient is served by the same handler.
pub async fn shared_inbox(data: Data<FedContext>, activity: ActivityData) -> Result<StatusCode, FedError> {
    deliver_to_inbox(&data, activity).await
}

/// `POST /users/{username}/inbox`: the personal inbox. The account must exist.
pub async fn personal_inbox(
    Path(username): Path<String>,
    data: Data<FedContext>,
    activity: ActivityData,
) -> Result<StatusCode, FedError> {
    let exists = data
        .app
        .store
        .account_by_handle(&normalize_username(&username), &host_from_url(&data.public_url))?
        .is_some_and(|account| account.local);
    if !exists {
        return Err(not_found("account"));
    }
    deliver_to_inbox(&data, activity).await
}

async fn deliver_to_inbox(data: &Data<FedContext>, activity: ActivityData) -> Result<StatusCode, FedError> {
    match receive_activity::<WithContext<Activity>, ActorRecord, FedContext>(activity, data).await {
        Ok(()) => Ok(StatusCode::ACCEPTED),
        Err(err) => {
            tracing::warn!(
                target: "birchtree::federation",
                direction = "inbound",
                status = err.status().as_u16(),
                error = %err,
                "inbound activity refused"
            );
            Err(err)
        }
    }
}

/// The local account for a username in a path, or 404.
async fn local_account(data: &Data<FedContext>, username: &str) -> Result<crate::domain::Account, FedError> {
    let account = data
        .app
        .store
        .account_by_handle(&normalize_username(username), &host_from_url(&data.public_url))?
        .filter(|a| a.local)
        .ok_or_else(|| not_found("account"))?;
    Ok(account)
}

/// `GET /users/{username}/statuses/{id}`: a local status as a Note. A deleted status is
/// a `Tombstone` (410); one that is not public or unlisted is not shown (404).
pub async fn note(
    Path((username, id)): Path<(String, String)>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<axum::response::Response, FedError> {
    use axum::response::IntoResponse;
    require_signed(&data, &headers, &method, &uri).await?;
    let account = local_account(&data, &username).await?;
    let status_id = crate::domain::Id::new(id.parse().map_err(|_| not_found("status"))?);
    let status = data
        .app
        .store
        .status_by_id(status_id)?
        .filter(|s| s.account_id == account.id)
        .ok_or_else(|| not_found("status"))?;
    if status.is_deleted() {
        return Ok((StatusCode::GONE, FederationJson(json!({
            "@context": crate::activitypub::outgoing::context(),
            "id": status.uri, "type": "Tombstone", "formerType": "Note"
        }))).into_response());
    }
    if !matches!(status.visibility, crate::domain::Visibility::Public | crate::domain::Visibility::Unlisted) {
        return Err(not_found("status"));
    }
    let mut note = crate::activitypub::outgoing::note_for_status(&*data.app.store, &data.public_url, &status, data.app.now_ms())?;
    note["@context"] = crate::activitypub::outgoing::context();
    Ok(FederationJson(note).into_response())
}

/// `GET /users/{username}/quote_authorizations/{id}`: the authorization of an accepted quote.
pub async fn quote_authorization(
    Path((username, id)): Path<(String, String)>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    require_signed(&data, &headers, &method, &uri).await?;
    let account = local_account(&data, &username).await?;
    let quote_id = crate::domain::Id::new(id.parse().map_err(|_| not_found("quote"))?);
    let quote = data
        .app
        .store
        .quote_by_id(quote_id)?
        .filter(|q| q.quoted_account_id == account.id && q.state == crate::domain::QuoteState::Accepted)
        .ok_or_else(|| not_found("quote authorization"))?;
    let quoting = data.app.store.status_by_id(quote.quoting_status_id)?.ok_or_else(|| not_found("status"))?;
    let quoted = data.app.store.status_by_id(quote.quoted_status_id)?.ok_or_else(|| not_found("status"))?;
    Ok(FederationJson(json!({
        "@context": crate::activitypub::outgoing::context(),
        "id": format!("{}/quote_authorizations/{}", account.actor_uri, quote.id.value()),
        "type": "QuoteAuthorization",
        "attributedTo": account.actor_uri,
        "interactingObject": quoting.uri,
        "interactionTarget": quoted.uri,
    })))
}

/// `GET /users/{username}/followers`: the followers collection, paged like
/// upstream (`FollowerAccountsController`, `FOLLOW_PER_PAGE = 12`):
/// the base document carries `{id, type: OrderedCollection, totalItems,
/// first: <uri>?page=1}` and each `?page=N` carries an
/// `OrderedCollectionPage` (`partOf`, `totalItems`, actor-URI `orderedItems`,
/// `next`/`prev`).
///
/// Divergence note: our account rows do not persist `hide_collections`, so
/// the collection is always listed (upstream hides the member list but keeps
/// the counts when the flag is set).
pub async fn followers(
    Path(username): Path<String>,
    Query(query): Query<CollectionQuery>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    require_signed(&data, &headers, &method, &uri).await?;
    federation_collection(&username, &query, &data, true).await
}

/// `GET /users/{username}/following`: the following collection, same paging
/// shape as followers (upstream `FollowingAccountsController`).
pub async fn following(
    Path(username): Path<String>,
    Query(query): Query<CollectionQuery>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    require_signed(&data, &headers, &method, &uri).await?;
    federation_collection(&username, &query, &data, false).await
}

/// Query parameters of the collection routes: numbered `page` for the
/// followers/following collections, `page=true` with `min_id`/`max_id`
/// cursors for the outbox (upstream's two paging shapes).
#[derive(Debug, Default, Deserialize)]
pub struct CollectionQuery {
    #[serde(default)]
    pub page: Option<String>,
    #[serde(default)]
    pub min_id: Option<String>,
    #[serde(default)]
    pub max_id: Option<String>,
}

/// Members per followers/following page (upstream
/// `AccountControllerConcern::FOLLOW_PER_PAGE`).
pub const COLLECTION_PER_PAGE: usize = 12;
/// Highest followers/following page served: bounds the cursor walk
/// (`page * PER_PAGE` rows) so a far-future page cannot force an unbounded
/// scan. Past it the answer is 404.
pub const COLLECTION_MAX_PAGE: u64 = 1000;

/// Parse a decimal id query parameter.
fn parse_collection_id(raw: &str, what: &str) -> Result<crate::domain::Id, FedError> {
    raw.parse().map_err(|_| not_found(what))
}

async fn federation_collection(
    username: &str,
    query: &CollectionQuery,
    data: &Data<FedContext>,
    followers: bool,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    let account = local_account(data, username).await?;
    let stats = data.app.store.account_stats(account.id)?;
    let (suffix, total) = if followers {
        ("followers", stats.followers_count)
    } else {
        ("following", stats.following_count)
    };
    let base = format!("{}/{suffix}", account.actor_uri);
    let Some(page_raw) = query.page.as_deref() else {
        return Ok(FederationJson(json!({
            "@context": crate::activitypub::outgoing::context(),
            "id": base,
            "type": "OrderedCollection",
            "totalItems": total,
            "first": format!("{base}?page=1"),
        })));
    };
    let page: u64 = page_raw.parse().map_err(|_| not_found("page"))?;
    if page == 0 || page > COLLECTION_MAX_PAGE {
        return Err(not_found("page"));
    }
    let store = &*data.app.store;
    // Newest-first cursor walk: rows up to the end of the wanted page, then
    // keep its last `PER_PAGE`. Bounded by `COLLECTION_MAX_PAGE`.
    let rows = if followers {
        store.followers(account.id, None, (page as usize).saturating_mul(COLLECTION_PER_PAGE))?
    } else {
        store.following(account.id, None, (page as usize).saturating_mul(COLLECTION_PER_PAGE))?
    };
    let start = (page as usize - 1) * COLLECTION_PER_PAGE;
    let ids: Vec<crate::domain::Id> = rows.into_iter().skip(start).take(COLLECTION_PER_PAGE).collect();
    let accounts = store.accounts_by_ids(&ids)?;
    let mut items = Vec::with_capacity(ids.len());
    for id in &ids {
        if let Some(account) = accounts.get(id) {
            items.push(account.actor_uri.clone());
        }
    }
    let mut page_doc = json!({
        "@context": crate::activitypub::outgoing::context(),
        "id": format!("{base}?page={page}"),
        "type": "OrderedCollectionPage",
        "partOf": base,
        "totalItems": total,
        "orderedItems": items,
    });
    if (page as usize) * COLLECTION_PER_PAGE < total as usize {
        page_doc["next"] = json!(format!("{base}?page={}", page + 1));
    }
    if page > 1 {
        page_doc["prev"] = json!(format!("{base}?page={}", page - 1));
    }
    Ok(FederationJson(page_doc))
}

/// `GET /users/{username}/outbox`: the public statuses as Create activities,
/// newest first, paged like upstream (`OutboxesController`, `LIMIT = 20`):
/// the base document carries `{id, type: OrderedCollection, totalItems:
/// statuses_count, first: <uri>?page=true, last: <uri>?page=true&min_id=0}`
/// and each `?page=true` (with `min_id`/`max_id` cursors) carries an
/// `OrderedCollectionPage` (`partOf`, `prev`/`next`, `orderedItems`).
pub async fn outbox(
    Path(username): Path<String>,
    Query(query): Query<CollectionQuery>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    require_signed(&data, &headers, &method, &uri).await?;
    let account = local_account(&data, &username).await?;
    let store = &*data.app.store;
    let base = format!("{}/outbox", account.actor_uri);
    let is_page = query.page.as_deref() == Some("true");
    if !is_page {
        let total = store.account_stats(account.id)?.statuses_count;
        return Ok(FederationJson(json!({
            "@context": crate::activitypub::outgoing::context(),
            "id": base,
            "type": "OrderedCollection",
            "totalItems": total,
            "first": format!("{base}?page=true"),
            "last": format!("{base}?page=true&min_id=0"),
        })));
    }
    // Cursor page: walk the author's newest-first id list past the cursor,
    // keeping public statuses until the limit fills (bounded: the scan stops
    // after `OUTBOX_PAGE_LIMIT * OUTBOX_SCAN_FACTOR` rows so a media-heavy
    // tail cannot stall the request).
    let max_id = query.max_id.as_deref().map(|raw| parse_collection_id(raw, "max_id")).transpose()?;
    let min_id = query.min_id.as_deref().map(|raw| parse_collection_id(raw, "min_id")).transpose()?;
    const OUTBOX_PAGE_LIMIT: usize = 20;
    const OUTBOX_SCAN_FACTOR: usize = 10;
    let mut items = Vec::new();
    let mut item_ids: Vec<crate::domain::Id> = Vec::new();
    let mut scanned = 0usize;
    let mut last_seen: Option<crate::domain::Id> = None;
    for id in store.account_status_ids(account.id)? {
        if let Some(max_id) = max_id
            && id >= max_id
        {
            continue;
        }
        if let Some(min_id) = min_id
            && id <= min_id
        {
            break;
        }
        last_seen = Some(id);
        scanned += 1;
        if scanned > OUTBOX_PAGE_LIMIT * OUTBOX_SCAN_FACTOR {
            break;
        }
        if items.len() >= OUTBOX_PAGE_LIMIT {
            continue;
        }
        if let Some(status) = store.status_by_id(id)? {
            if status.visibility == crate::domain::Visibility::Public {
                item_ids.push(status.id);
                items.push(crate::activitypub::outgoing::create(store, &data.public_url, &status, data.app.now_ms())?);
            }
        }
    }
    let full = items.len() >= OUTBOX_PAGE_LIMIT;
    let mut params = String::from("page=true");
    if let Some(min_id) = query.min_id.as_deref() {
        params.push_str(&format!("&min_id={min_id}"));
    }
    if let Some(max_id) = query.max_id.as_deref() {
        params.push_str(&format!("&max_id={max_id}"));
    }
    let mut page_doc = json!({
        "@context": crate::activitypub::outgoing::context(),
        "id": format!("{base}?{params}"),
        "type": "OrderedCollectionPage",
        "partOf": base,
        "orderedItems": items,
    });
    // A full page means older rows may remain: `next` continues below the
    // oldest row the scan touched. `prev` climbs above the page's newest id
    // (absent on an empty page, like upstream's empty-`@statuses` page).
    if full && let Some(last) = last_seen {
        page_doc["next"] = json!(format!("{base}?page=true&max_id={}", last.value()));
    }
    if let Some(newest) = item_ids.first() {
        page_doc["prev"] = json!(format!("{base}?page=true&min_id={}", newest.value()));
    }
    Ok(FederationJson(page_doc))
}

/// `GET /actor`: the instance actor's document (its key verifies signed fetches).
pub async fn instance_actor(data: Data<FedContext>) -> Result<Json<serde_json::Value>, FedError> {
    use crate::activitypub::instance::{document, InstanceActor, INSTANCE_KEY_ID};
    let keys = data
        .app
        .store
        .actor_keys(crate::domain::Id::new(INSTANCE_KEY_ID))?
        .filter(|k| k.private_key_pem.is_some())
        .ok_or_else(|| not_found("instance actor"))?;
    let id = url::Url::parse(&format!("{}/actor", data.public_url)).map_err(|e| FedError::Refused(e.to_string()))?;
    let actor = InstanceActor {
        id,
        public_key_pem: keys.public_key_pem,
        private_key_pem: String::new(),
    };
    Ok(Json(document(&actor, &host_from_url(&data.public_url))))
}


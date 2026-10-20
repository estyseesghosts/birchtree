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

/// `GET /nodeinfo/2.0`. The `usage` block is not published: birchtree has no
/// count of local accounts or posts separate from remote ones yet.
pub async fn nodeinfo(data: Data<FedContext>) -> Json<serde_json::Value> {
    Json(json!({
        "version": "2.0",
        "software": {"name": "birchtree", "version": env!("CARGO_PKG_VERSION")},
        "protocols": ["activitypub"],
        "services": {"inbound": [], "outbound": []},
        "openRegistrations": data.registrations_open,
        "metadata": {}
    }))
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

/// `GET /users/{username}/followers`: the followers collection. Its count is exact; the
/// members are not listed (the collection is a count, as far as remote servers read it).
pub async fn followers(
    Path(username): Path<String>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    require_signed(&data, &headers, &method, &uri).await?;
    collection(&username, &data, true).await
}

/// `GET /users/{username}/following`: the following collection (a count, as above).
pub async fn following(
    Path(username): Path<String>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    require_signed(&data, &headers, &method, &uri).await?;
    collection(&username, &data, false).await
}

async fn collection(username: &str, data: &Data<FedContext>, followers: bool) -> Result<FederationJson<serde_json::Value>, FedError> {
    let account = local_account(data, username).await?;
    let stats = data.app.store.account_stats(account.id)?;
    let (suffix, total) = if followers {
        ("followers", stats.followers_count)
    } else {
        ("following", stats.following_count)
    };
    Ok(FederationJson(json!({
        "@context": crate::activitypub::outgoing::context(),
        "id": format!("{}/{suffix}", account.actor_uri),
        "type": "OrderedCollection",
        "totalItems": total,
    })))
}

/// `GET /users/{username}/outbox`: the public statuses as Create activities, newest first.
pub async fn outbox(
    Path(username): Path<String>,
    data: Data<FedContext>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> Result<FederationJson<serde_json::Value>, FedError> {
    require_signed(&data, &headers, &method, &uri).await?;
    let account = local_account(&data, &username).await?;
    let store = &*data.app.store;
    let mut items = Vec::new();
    for id in store.account_status_ids(account.id)? {
        if items.len() >= 20 {
            break;
        }
        if let Some(status) = store.status_by_id(id)? {
            if status.visibility == crate::domain::Visibility::Public {
                items.push(crate::activitypub::outgoing::create(store, &data.public_url, &status, data.app.now_ms())?);
            }
        }
    }
    Ok(FederationJson(json!({
        "@context": crate::activitypub::outgoing::context(),
        "id": format!("{}/outbox", account.actor_uri),
        "type": "OrderedCollection",
        "totalItems": items.len(),
        "orderedItems": items,
    })))
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


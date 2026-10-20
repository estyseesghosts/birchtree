//! Remote thread completion (Phase 6, `docs/federation-contract.md` A–E).
//!
//! - **Parent resolution (A):** a remote reply whose parent is unknown is stored
//!   with the parent's URI, and a `ResolveParent` job fetches the parent. One hop
//!   per job; the chain stops at [`MAX_PARENT_HOPS`]. A parent that is not found,
//!   refused, or not a valid document is a terminal state: the reply stays
//!   unresolved and is never promoted to a root.
//! - **Descendants (B):** `CrawlReplies` reads a status's `replies` collection,
//!   breadth first, under the caps below.
//!
//! Every fetch goes through the crate (`ObjectId::dereference`, `fetch_object_http`),
//! so each fetch is signed when the instance actor is configured, and passes the
//! same URL policy, size and time limits as every other remote fetch.

use activitypub_federation::{
    fetch::{fetch_object_http, object_id::ObjectId},
    error::Error as ProtocolError,
};
use std::collections::{HashSet, VecDeque};
use url::Url;

use crate::activitypub::error::FedError;
use crate::activitypub::objects::NoteRef;
use crate::activitypub::wire::{CollectionPage, Note};
use crate::activitypub::FedContext;
use crate::application::ports::Store;
use crate::domain::{Id, JobKind, Result};
use crate::federation::delivery::DeliveryError;

/// Parent hops per chain (contract D: a proposal; upstream has no hop cap).
pub const MAX_PARENT_HOPS: u64 = 40;
/// Items read per `replies` collection (upstream `FetchAllRepliesService::MAX_REPLIES`).
pub const MAX_COLLECTION_ITEMS: usize = 500;
/// Replies fetched in total per crawl (upstream `FetchAllRepliesWorker::MAX_REPLIES`).
pub const MAX_CRAWL_REPLIES: usize = 1000;
/// Collection pages read per crawl (upstream `FetchAllRepliesWorker::MAX_PAGES`).
pub const MAX_CRAWL_PAGES: usize = 500;

/// Record that a stored reply's parent is `parent_uri`, and queue its resolution.
pub fn schedule_parent(store: &dyn Store, child_id: Id, parent_uri: &str, now_ms: u64) -> Result<()> {
    store.set_unresolved_parent(child_id, Some(parent_uri))?;
    store.enqueue(
        JobKind::ResolveParent,
        serde_json::json!({ "child_status_id": child_id.value(), "parent_uri": parent_uri, "hop": 0 }),
        now_ms,
    )?;
    Ok(())
}

/// The outcome of a thread job that failed.
fn settle(err: FedError) -> DeliveryError {
    match err {
        // Refused, gone, or not a valid document: retrying gives the same answer.
        FedError::Refused(msg) | FedError::Gone(msg) | FedError::Unsupported(msg) => {
            DeliveryError::Quarantine(msg)
        }
        FedError::Domain(e) => DeliveryError::Quarantine(e.to_string()),
        FedError::Protocol(e) => match e {
            ProtocolError::NotFound
            | ProtocolError::ObjectDeleted(_)
            | ProtocolError::UrlVerificationError(_)
            | ProtocolError::ParseFetchedObject(..)
            | ProtocolError::FetchWrongId(_)
            | ProtocolError::FetchInvalidContentType(_) => DeliveryError::Quarantine(e.to_string()),
            other => DeliveryError::Retry(other.to_string()),
        },
    }
}

/// Fetch and link one missing parent. Terminal results are logged and return `Ok`.
pub async fn resolve_parent(config: &activitypub_federation::config::FederationConfig<FedContext>, job: &crate::domain::Job) -> std::result::Result<(), DeliveryError> {
    let payload = &job.payload;
    let child = payload
        .get("child_status_id")
        .and_then(serde_json::Value::as_u64)
        .map(Id::new)
        .ok_or_else(|| DeliveryError::Quarantine("resolve job has no child".into()))?;
    let parent_uri = payload
        .get("parent_uri")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| DeliveryError::Quarantine("resolve job has no parent uri".into()))?
        .to_string();
    let hop = payload.get("hop").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let now = ctx.app.now_ms();
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());

    if store.unresolved_parent(child).map_err(retry)?.is_none() {
        return Ok(());
    }
    if let Some(parent) = store.status_by_uri(&parent_uri).map_err(retry)? {
        store.link_reply(child, parent.id).map_err(retry)?;
        tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "ResolveParent", parent = %parent_uri, outcome = "linked (parent already stored)", "thread");
        if let Some(next) = store.unresolved_parent(parent.id).map_err(retry)? {
            enqueue_hop(store, parent.id, &next, hop + 1, now).map_err(retry)?;
        }
        return Ok(());
    }
    if hop >= MAX_PARENT_HOPS {
        tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "ResolveParent", parent = %parent_uri, hop, outcome = "stopped: hop cap, reply stays unresolved", "thread");
        return Ok(());
    }
    let url = Url::parse(&parent_uri).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
    let data = config.to_request_data();
    match ObjectId::<NoteRef>::from(url).dereference(&data).await {
        Ok(fetched) => {
            store.link_reply(child, fetched.status.id).map_err(retry)?;
            tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "ResolveParent", parent = %parent_uri, hop, outcome = "linked", "thread");
            if let Some(next) = store.unresolved_parent(fetched.status.id).map_err(retry)? {
                enqueue_hop(store, fetched.status.id, &next, hop + 1, now).map_err(retry)?;
            }
            Ok(())
        }
        Err(err) => {
            let settled = settle(err);
            match &settled {
                DeliveryError::Quarantine(msg) => tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "ResolveParent", parent = %parent_uri, outcome = %format!("terminal, reply stays unresolved: {msg}"), "thread"),
                DeliveryError::Retry(_) => {}
            }
            match settled {
                DeliveryError::Quarantine(_) => Ok(()),
                retry_err => Err(retry_err),
            }
        }
    }
}

fn enqueue_hop(store: &dyn Store, child: Id, parent_uri: &str, hop: u64, now: u64) -> Result<()> {
    if hop >= MAX_PARENT_HOPS {
        return Ok(());
    }
    store.enqueue(
        JobKind::ResolveParent,
        serde_json::json!({ "child_status_id": child.value(), "parent_uri": parent_uri, "hop": hop }),
        now,
    )?;
    Ok(())
}

/// Crawl a remote status's replies, breadth first, under the caps.
pub async fn crawl_replies(config: &activitypub_federation::config::FederationConfig<FedContext>, job: &crate::domain::Job) -> std::result::Result<(), DeliveryError> {
    let root = job
        .payload
        .get("status_id")
        .and_then(serde_json::Value::as_u64)
        .map(Id::new)
        .ok_or_else(|| DeliveryError::Quarantine("crawl job has no status".into()))?;
    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let status = store
        .status_by_id(root)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine("crawl of a missing status".into()))?;
    if !status.uri.starts_with("http") {
        return Ok(());
    }
    let data = config.to_request_data();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut seen: HashSet<String> = HashSet::new();
    queue.push_back(status.uri.clone());
    let mut fetched_replies = 0usize;
    let mut pages = 0usize;
    while let Some(uri) = queue.pop_front() {
        if fetched_replies >= MAX_CRAWL_REPLIES || pages >= MAX_CRAWL_PAGES {
            break;
        }
        let url = Url::parse(&uri).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
        // The Note carries the URI of its replies collection.
        let note: Note = match fetch_object_http::<_, Note>(&url, &data).await {
            Ok(response) => response.object,
            Err(err) => {
                let settled = settle(FedError::Protocol(err));
                if matches!(settled, DeliveryError::Retry(_)) {
                    return Err(settled);
                }
                continue;
            }
        };
        let Some(collection) = replies_url(note.replies.as_ref()) else {
            continue;
        };
        let mut next_page = Some(collection);
        let mut items_here = 0usize;
        while let Some(page_url) = next_page.take() {
            if pages >= MAX_CRAWL_PAGES || items_here >= MAX_COLLECTION_ITEMS {
                break;
            }
            pages += 1;
            let page: CollectionPage = match fetch_object_http::<_, CollectionPage>(&page_url, &data).await {
                Ok(response) => response.object,
                Err(err) => {
                    let settled = settle(FedError::Protocol(err));
                    if matches!(settled, DeliveryError::Retry(_)) {
                        return Err(settled);
                    }
                    break;
                }
            };
            let items: Vec<Url> = if page.items.is_empty() { page.ordered_items.clone() } else { page.items.clone() };
            for item in items {
                if items_here >= MAX_COLLECTION_ITEMS || fetched_replies >= MAX_CRAWL_REPLIES {
                    break;
                }
                items_here += 1;
                let reply = item.to_string();
                if !seen.insert(reply.clone()) {
                    continue;
                }
                fetched_replies += 1;
                // Stored through the crate's dereference (signed, verified, ingested).
                match ObjectId::<NoteRef>::from(item.clone()).dereference(&data).await {
                    Ok(_) => queue.push_back(reply),
                    Err(err) => {
                        let settled = settle(err);
                        if matches!(settled, DeliveryError::Retry(_)) {
                            return Err(settled);
                        }
                    }
                }
            }
            next_page = page.next_uri();
        }
    }
    tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "CrawlReplies", status = %status.uri, fetched = fetched_replies, pages, outcome = "done", "thread");
    Ok(())
}

/// The first page of a `replies` collection: its `first` (Mastodon's shape), or
/// the collection URI itself when it names a page.
fn replies_url(replies: Option<&serde_json::Value>) -> Option<Url> {
    let value = replies?;
    match value {
        serde_json::Value::String(s) => Url::parse(s).ok(),
        serde_json::Value::Object(o) => {
            let from = |v: &serde_json::Value| match v {
                serde_json::Value::String(s) => Url::parse(s).ok(),
                serde_json::Value::Object(f) => f.get("id").and_then(|i| i.as_str()).and_then(|s| Url::parse(s).ok()),
                _ => None,
            };
            o.get("first").and_then(from).or_else(|| o.get("id").and_then(from))
        }
        _ => None,
    }
}

/// Upstream's `FETCH_REPLIES_INITIAL_WAIT_MINUTES`: a remote status is crawled once it is this old.
pub const INITIAL_WAIT_MS: u64 = 5 * 60 * 1000;
/// Upstream's `FETCH_REPLIES_COOLDOWN_MINUTES`: a status is refreshed at most this often.
pub const COOLDOWN_MS: u64 = 15 * 60 * 1000;
/// How far up an unresolved ancestor chain one read looks (the hop cap).
const ANCESTOR_WALK: usize = 40;

/// Read-time completion for an authenticated `GET /api/v1/statuses/{id}/context`
/// (contract B and C). Returns the refresh ID for the `Mastodon-Async-Refresh`
/// header, or `None` when there is nothing to complete. Anonymous reads never reach
/// this (the caller checks the token).
///
/// A refresh inside its cooldown is reported with its ID and enqueues nothing. Otherwise
/// the work is enqueued: a `CrawlReplies` for a remote distributable root old enough to
/// crawl, and a `ResolveParent` for the nearest unresolved ancestor link.
pub fn read_time_completion(store: &dyn Store, root: &crate::domain::Status, now_ms: u64) -> Result<Option<String>> {
    if let Some(record) = store.remote_refresh(root.id)? {
        if now_ms.saturating_sub(record.refreshed_at_ms) < COOLDOWN_MS {
            return Ok(Some(record.refresh_id));
        }
    }
    let remote = store.account_by_id(root.account_id)?.is_some_and(|a| !a.local);
    if !remote {
        return Ok(None);
    }
    let distributable = matches!(
        root.visibility,
        crate::domain::Visibility::Public | crate::domain::Visibility::Unlisted
    );
    let old_enough = now_ms.saturating_sub(root.created_at_ms) >= INITIAL_WAIT_MS;
    let mut gap: Option<(Id, String)> = None;
    let mut cursor = Some(root.clone());
    let mut steps = 0usize;
    while let Some(status) = cursor.take() {
        if let Some(uri) = store.unresolved_parent(status.id)? {
            gap = Some((status.id, uri));
            break;
        }
        steps += 1;
        cursor = match status.in_reply_to_id {
            Some(parent) if steps < ANCESTOR_WALK => store.status_by_id(parent)?,
            _ => None,
        };
    }
    let crawl = distributable && old_enough;
    if !crawl && gap.is_none() {
        return Ok(None);
    }
    if crawl {
        store.enqueue(
            JobKind::CrawlReplies,
            serde_json::json!({ "status_id": root.id.value() }),
            now_ms,
        )?;
    }
    if let Some((child, parent_uri)) = gap {
        schedule_parent(store, child, &parent_uri, now_ms)?;
    }
    let refresh_id = format!("{}-{}", root.id.value(), now_ms);
    store.set_remote_refresh(
        root.id,
        crate::domain::federation::RefreshRecord {
            refreshed_at_ms: now_ms,
            refresh_id: refresh_id.clone(),
        },
    )?;
    Ok(Some(refresh_id))
}

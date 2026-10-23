//! Builders for the [`crate::federation::events`] job: each event family maps
//! to one function returning the fixed activity bodies plus their destination
//! inboxes. Behavior is verbatim from `events::run`; the parent keeps the
//! actor lookup, the relay fan-out and the enqueue, and only matches each
//! event name to the builder below.
//!
//! Builders return `None` when the event plans nothing (a vote on a local
//! poll, hidden poll totals): the parent then succeeds without enqueueing.
//! Any other event name is a caller bug and quarantines.

use activitypub_federation::config::FederationConfig;
use activitypub_federation::fetch::object_id::ObjectId;
use serde_json::Value;

use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::activitypub::outgoing;
use crate::application::federation_ports::DeliverySpec;
use crate::application::ports::Store;
use crate::domain::{Account, Id, Job, JobKind, Result, Status};
use crate::federation::delivery::DeliveryError;

/// Parsed head of a `FederationEvent` job: the event name, the resolved
/// local author, and the id/uri selectors the builders read. Rejection
/// strings are verbatim from the `events::run` prelude.
pub struct EventHead {
    /// Event name (`payload["event"]`, empty when absent).
    pub event: String,
    /// Local author (`payload["actor_id"]`); remote/missing actors reject.
    pub author: Account,
    /// Subject status (`payload["status_id"]`).
    pub status_id: Option<Id>,
    /// Block/flag target (`payload["target_id"]`).
    pub target_id: Option<Id>,
    /// Flag report (`payload["report_id"]`).
    pub report_id: Option<Id>,
    /// Quote (`payload["quote_id"]`).
    pub quote_id: Option<Id>,
    /// Announce URI override (`payload["announce_uri"]`).
    pub announce_uri: Option<String>,
}

/// Parse a job into its [`EventHead`]: event name, local author and arm
/// selectors. Verbatim from the `events::run` prelude (same order: event,
/// actor, author; the selectors are pure payload reads).
pub fn load_head(store: &dyn Store, job: &Job) -> std::result::Result<EventHead, DeliveryError> {
    let quarantine = |msg: String| DeliveryError::Quarantine(msg);
    let uint = |key: &str| job.payload.get(key).and_then(Value::as_u64).map(Id::new);
    let text = |key: &str| {
        job.payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let event = job
        .payload
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let actor_id = job
        .payload
        .get("actor_id")
        .and_then(Value::as_u64)
        .map(Id::new)
        .ok_or_else(|| quarantine("event has no actor".into()))?;
    let author = store
        .account_by_id(actor_id)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .filter(|a| a.local)
        .ok_or_else(|| quarantine("event actor is not local".into()))?;
    Ok(EventHead {
        event,
        author,
        status_id: uint("status_id"),
        target_id: uint("target_id"),
        report_id: uint("report_id"),
        quote_id: uint("quote_id"),
        announce_uri: text("announce_uri"),
    })
}

/// Build the full-document `Update` and `Delete` activities: status
/// `create`/`update`/`delete`, actor `profile_update` and `delete_actor`.
/// (S7 slice group B: covers `events.rs` create/update/delete,
/// question-update-free profile/deletion arms verbatim.)
pub async fn build_create_update_delete(
    config: &FederationConfig<FedContext>,
    store: &dyn Store,
    public: &str,
    author: &Account,
    event: &str,
    status_id: Option<Id>,
    now: u64,
    license_policy: &str,
    place_policy: &str,
) -> std::result::Result<Option<(Vec<Value>, Vec<String>)>, DeliveryError> {
    use activitypub_federation::traits::Object;
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    match event {
        "create" | "update" | "delete" => {
            let status = status_of(store, status_id)?;
            let body = match event {
                "create" => {
                    outgoing::create(store, public, &status, now, license_policy, place_policy)
                        .map_err(retry)?
                }
                "update" => {
                    outgoing::update(store, public, &status, now, license_policy, place_policy)
                        .map_err(retry)?
                }
                _ => outgoing::delete(author, &status),
            };
            let inboxes = status_audience(store, author, &status).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        "profile_update" => {
            let actor_url = url::Url::parse(&author.actor_uri).map_err(|e| {
                DeliveryError::Quarantine(format!("profile update of an actor without a URL: {e}"))
            })?;
            let data = config.to_request_data();
            let record = ObjectId::<ActorRecord>::from(actor_url)
                .dereference(&data)
                .await
                .map_err(|e| DeliveryError::Retry(e.to_string()))?;
            let person = record
                .into_json(&data)
                .await
                .map_err(|e| DeliveryError::Retry(e.to_string()))?;
            let person = serde_json::to_value(&person)
                .map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
            let body = outgoing::profile_update(author, person, now);
            let inboxes = outgoing::follower_inboxes(store, author.id).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        "delete_actor" => {
            let body = outgoing::delete_actor(author);
            let inboxes = outgoing::follower_inboxes(store, author.id).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        _ => Err(DeliveryError::Quarantine(format!(
            "unexpected event for create_update_delete: {event}"
        ))),
    }
}

/// Build `like` / `unlike`, addressed to the liked status's author.
pub fn build_like(
    store: &dyn Store,
    author: &Account,
    event: &str,
    status_id: Option<Id>,
) -> std::result::Result<Option<(Vec<Value>, Vec<String>)>, DeliveryError> {
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let target = status_of(store, status_id)?;
    let body = if event == "like" {
        outgoing::like(author, &target)
    } else if event == "unlike" {
        outgoing::undo_like(author, &target)
    } else {
        return Err(DeliveryError::Quarantine(format!(
            "unexpected event for like: {event}"
        )));
    };
    let inboxes = outgoing::inboxes_of(store, &[target.account_id]).map_err(retry)?;
    Ok(Some((vec![body], inboxes)))
}

/// Build poll activities: `poll_vote` (one `Create` per chosen option, to
/// the poll owner) and `question_update` (the refreshed `Question`).
pub fn build_poll_vote(
    store: &dyn Store,
    public: &str,
    author: &Account,
    event: &str,
    status_id: Option<Id>,
    now: u64,
    license_policy: &str,
    place_policy: &str,
) -> std::result::Result<Option<(Vec<Value>, Vec<String>)>, DeliveryError> {
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let quarantine = |msg: String| DeliveryError::Quarantine(msg);
    let status = status_of(store, status_id)?;
    if event == "question_update" {
        if status.account_id != author.id {
            return Err(quarantine(
                "question update for a status of another account".into(),
            ));
        }
        let poll = store
            .poll_by_status(status.id)
            .map_err(retry)?
            .ok_or_else(|| quarantine("question update for a status without a poll".into()))?;
        if poll.hide_totals {
            return Ok(None);
        }
        let body =
            outgoing::question_update(store, public, &status, now, license_policy, place_policy)
                .map_err(retry)?;
        let inboxes = status_audience(store, author, &status).map_err(retry)?;
        return Ok(Some((vec![body], inboxes)));
    }
    if event != "poll_vote" {
        return Err(DeliveryError::Quarantine(format!(
            "unexpected event for poll_vote: {event}"
        )));
    }
    let owner = store
        .account_by_id(status.account_id)
        .map_err(retry)?
        .ok_or_else(|| quarantine("vote on a status whose author is missing".into()))?;
    if owner.local {
        return Ok(None);
    }
    let poll = store
        .poll_by_status(status.id)
        .map_err(retry)?
        .ok_or_else(|| quarantine("vote on a status without a poll".into()))?;
    let ballot = store
        .ballot(poll.id, author.id)
        .map_err(retry)?
        .ok_or_else(|| quarantine("vote without a ballot".into()))?;
    let mut bodies = Vec::new();
    for (n, choice) in ballot.iter().enumerate() {
        let Some(title) = poll.options.get(*choice as usize).map(|o| o.title.clone()) else {
            continue;
        };
        let vote_id = format!(
            "{}#votes/{}/{}/{}",
            author.actor_uri,
            poll.id.value(),
            n,
            now
        );
        bodies.push(outgoing::vote(author, &owner, &status, &vote_id, &title));
    }
    if bodies.is_empty() {
        return Err(quarantine("vote with no deliverable choices".into()));
    }
    let inboxes = outgoing::inboxes_of(store, &[owner.id]).map_err(retry)?;
    Ok(Some((bodies, inboxes)))
}

/// Build targeted activities: `announce` / `undo_announce`, `block` /
/// `unblock`, and `flag`. Each goes only to the target's inbox.
pub fn build_announce_block_flag(
    store: &dyn Store,
    author: &Account,
    event: &str,
    status_id: Option<Id>,
    target_id: Option<Id>,
    announce_uri: Option<&str>,
    report_id: Option<Id>,
) -> std::result::Result<Option<(Vec<Value>, Vec<String>)>, DeliveryError> {
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let quarantine = |msg: String| DeliveryError::Quarantine(msg);
    match event {
        "announce" => {
            let target = status_of(store, status_id)?;
            let boost = store
                .own_reblog(author.id, target.id)
                .map_err(retry)?
                .ok_or_else(|| quarantine("announce without a boost row".into()))?;
            let body = outgoing::announce(store, author, &boost.uri, &target, boost.visibility)
                .map_err(retry)?;
            let inboxes =
                boost_audience(store, author, &target, boost.visibility).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        "undo_announce" => {
            let target = status_of(store, status_id)?;
            let uri =
                announce_uri.ok_or_else(|| quarantine("undo without an announce uri".into()))?;
            let body = outgoing::undo_announce(author, uri, &target);
            let inboxes = outgoing::inboxes_of(store, &[target.account_id]).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        "block" | "unblock" => {
            let target = store
                .account_by_id(
                    target_id.ok_or_else(|| quarantine("block without a target".into()))?,
                )
                .map_err(retry)?
                .ok_or_else(|| quarantine("block of a missing account".into()))?;
            let body = if event == "block" {
                outgoing::block(author, &target)
            } else {
                outgoing::undo_block(author, &target)
            };
            let inboxes = outgoing::inboxes_of(store, &[target.id]).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        "flag" => {
            let report = store
                .report_by_id(report_id.ok_or_else(|| quarantine("flag without a report".into()))?)
                .map_err(retry)?
                .ok_or_else(|| quarantine("flag of a missing report".into()))?;
            let target = store
                .account_by_id(report.target_account_id)
                .map_err(retry)?
                .ok_or_else(|| quarantine("flag of a missing account".into()))?;
            let mut uris = Vec::new();
            for id in &report.status_ids {
                if let Some(status) = store.status_by_id(*id).map_err(retry)? {
                    uris.push(status.uri);
                }
            }
            let body = outgoing::flag(author, &target, &uris, &report.comment, report.id);
            let inboxes = outgoing::inboxes_of(store, &[target.id]).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        _ => Err(DeliveryError::Quarantine(format!(
            "unexpected event for announce_block_flag: {event}"
        ))),
    }
}

/// Build quote activities: `quote_request` plus its `Accept` / `Reject`,
/// and `quote_revoke` (deleting the `QuoteAuthorization`).
pub fn build_quote(
    store: &dyn Store,
    public: &str,
    author: &Account,
    event: &str,
    quote_id: Option<Id>,
    now: u64,
    license_policy: &str,
    place_policy: &str,
) -> std::result::Result<Option<(Vec<Value>, Vec<String>)>, DeliveryError> {
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let quarantine = |msg: String| DeliveryError::Quarantine(msg);
    if event == "quote_revoke" {
        let quote = quote_of(store, quote_id)?;
        let approval = quote.approval_uri.clone();
        let approval =
            approval.ok_or_else(|| quarantine("revoke without an authorization".into()))?;
        let quoting = status_of(store, Some(quote.quoting_status_id))?;
        let quoted = status_of(store, Some(quote.quoted_status_id))?;
        let body = outgoing::revoke_quote(author, &approval, &quoting.uri, &quoted.uri);
        let inboxes = outgoing::inboxes_of(store, &[quote.quoting_account_id]).map_err(retry)?;
        return Ok(Some((vec![body], inboxes)));
    }
    if !matches!(event, "quote_request" | "quote_accept" | "quote_reject") {
        return Err(DeliveryError::Quarantine(format!(
            "unexpected event for quote: {event}"
        )));
    }
    let quote = quote_of(store, quote_id)?;
    let quoting = status_of(store, Some(quote.quoting_status_id))?;
    let quoted = status_of(store, Some(quote.quoted_status_id))?;
    let request_uri = match &quote.quote_activity_uri {
        Some(uri) => uri.clone(),
        None => {
            let uri = format!(
                "{}/quote_requests/{}",
                author_uri_of(store, quote.quoting_account_id)?,
                quote.id.value()
            );
            store
                .set_quote_state(quote.id, quote.state, None, Some(uri.clone()))
                .map_err(retry)?;
            uri
        }
    };
    let request = outgoing::quote_request(
        store,
        public,
        &request_uri,
        &quoting,
        &quoted,
        now,
        license_policy,
        place_policy,
    )
    .map_err(retry)?;
    match event {
        "quote_request" => {
            let inboxes = outgoing::inboxes_of(store, &[quote.quoted_account_id]).map_err(retry)?;
            Ok(Some((vec![request], inboxes)))
        }
        "quote_accept" => {
            let approval = quote.approval_uri.clone();
            let approval =
                approval.ok_or_else(|| quarantine("accept without an authorization".into()))?;
            let body = outgoing::accept_quote(author, &request, quote.id, &approval);
            let inboxes =
                outgoing::inboxes_of(store, &[quote.quoting_account_id]).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
        _ => {
            let body = outgoing::reject_quote(author, &request, quote.id);
            let inboxes =
                outgoing::inboxes_of(store, &[quote.quoting_account_id]).map_err(retry)?;
            Ok(Some((vec![body], inboxes)))
        }
    }
}

/// Load the status for an event, or quarantine when it is missing.
pub(crate) fn status_of(
    store: &dyn Store,
    id: Option<Id>,
) -> std::result::Result<Status, DeliveryError> {
    let id = id.ok_or_else(|| DeliveryError::Quarantine("event has no status".into()))?;
    store
        .status_by_id(id)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine(format!("event status {id} is missing")))
}

/// Load the quote for an event, or quarantine when it is missing.
pub(crate) fn quote_of(
    store: &dyn Store,
    id: Option<Id>,
) -> std::result::Result<crate::domain::QuoteRecord, DeliveryError> {
    let id = id.ok_or_else(|| DeliveryError::Quarantine("event has no quote".into()))?;
    store
        .quote_by_id(id)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine(format!("event quote {id} is missing")))
}

/// Load the actor URI of an account, or quarantine when it is missing.
pub(crate) fn author_uri_of(
    store: &dyn Store,
    id: Id,
) -> std::result::Result<String, DeliveryError> {
    store
        .account_by_id(id)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .map(|a| a.actor_uri)
        .ok_or_else(|| DeliveryError::Quarantine("quote author is missing".into()))
}

/// Inboxes for a status: followers (unless direct), its mentions, and its reply's parent author.
pub(crate) fn status_audience(
    store: &dyn Store,
    author: &Account,
    status: &Status,
) -> Result<Vec<String>> {
    let mut ids: Vec<Id> = status.mentions.clone();
    if let Some(parent) = status.in_reply_to_account_id {
        ids.push(parent);
    }
    let mut inboxes = outgoing::inboxes_of(store, &ids)?;
    if status.visibility != crate::domain::Visibility::Direct {
        for inbox in outgoing::follower_inboxes(store, author.id)? {
            if !inboxes.contains(&inbox) {
                inboxes.push(inbox);
            }
        }
    }
    Ok(inboxes)
}

/// Inboxes for a boost: the followers (when the boost is visible to them) and the boosted author.
pub(crate) fn boost_audience(
    store: &dyn Store,
    author: &Account,
    target: &Status,
    visibility: crate::domain::Visibility,
) -> Result<Vec<String>> {
    let mut inboxes = outgoing::inboxes_of(store, &[target.account_id])?;
    if visibility != crate::domain::Visibility::Direct {
        for inbox in outgoing::follower_inboxes(store, author.id)? {
            if !inboxes.contains(&inbox) {
                inboxes.push(inbox);
            }
        }
    }
    Ok(inboxes)
}

/// Fan out public content activities to configured relay peers (G-D
/// relays): same fixed bytes, per-host budget via `RelayForward`. Targeted
/// activities (likes, flags, quotes, blocks, profile/deletion bookkeeping)
/// never leave their addressed inboxes, and non-public statuses never reach
/// a relay. Verbatim from `events::run`.
pub(crate) fn enqueue_relays(
    store: &dyn Store,
    relays: &[String],
    head: &EventHead,
    bodies: &[Value],
    now: u64,
) -> Result<()> {
    if !matches!(
        head.event.as_str(),
        "create" | "update" | "delete" | "announce" | "question_update"
    ) {
        return Ok(());
    }
    let relay_public = head
        .status_id
        .and_then(|id| store.status_by_id(id).ok().flatten())
        .is_some_and(|status| status.visibility == crate::domain::Visibility::Public);
    if relay_public {
        for relay in crate::federation::delivery::relay_inboxes(relays) {
            for body in bodies {
                // The relay send carries the relay host's sequence, minted
                // in the same transaction as the insert (see the job
                // queue's delivery mint), so relay traffic joins the same
                // per-host FIFO without a stamp-then-enqueue race.
                let payload = serde_json::json!({
                    "sender_id": head.author.id.value(),
                    "relay_inbox": relay.as_str(),
                    "activity_uri": body.get("id").and_then(Value::as_str).unwrap_or_default(),
                    "body": body,
                });
                store.enqueue(JobKind::RelayForward, payload, now)?;
            }
        }
    }
    Ok(())
}

/// Enqueue one `DeliverActivity` job per destination inbox for each planned
/// body. Plan order is sequence order: the atomic enqueue mints the host
/// sequence and inserts the job in one transaction, so a `Follow` planned
/// before its `Undo` always carries the smaller number. Verbatim from
/// `events::run`.
pub(crate) fn enqueue_deliveries(
    store: &dyn Store,
    head: &EventHead,
    bodies: &[Value],
    inboxes: &[String],
    now: u64,
) -> Result<()> {
    for body in bodies {
        let activity_uri = body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        tracing::debug!(target: "birchtree::federation", direction = "outbound", event = %head.event, activity = %activity_uri, destinations = inboxes.len(), "event planned");
        for inbox in inboxes {
            // Plan order is sequence order: the atomic enqueue mints the host
            // sequence and inserts the job in one transaction, so a `Follow`
            // planned before its `Undo` always carries the smaller number.
            let delivery = DeliverySpec {
                sender_id: head.author.id,
                inbox_uri: inbox.clone(),
                activity_uri: activity_uri.clone(),
                body: body.clone(),
            };
            store.enqueue_delivery(delivery, now)?;
        }
    }
    Ok(())
}

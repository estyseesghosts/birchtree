//! The `FederationEvent` job: a local change becomes one `DeliverActivity` job per
//! destination inbox. The activity JSON is built here from the state at this time,
//! and is then fixed in each delivery job, so a retry sends the same bytes.
//!
//! Recipients follow the activity's audience: followers for public, unlisted and
//! private statuses (none for direct ones); mentioned accounts and the author of
//! the parent of a reply; the target of a like, boost, block, flag or quote request.

use activitypub_federation::config::FederationConfig;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::traits::Object;
use serde_json::Value;

use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::activitypub::outgoing;
use crate::application::ports::Store;
use crate::domain::{Account, Id, JobKind, QuoteState, Result, Status, Visibility};
use crate::federation::delivery::DeliveryError;

/// Run one event: build its activity and enqueue its deliveries.
pub async fn run(
    config: &FederationConfig<FedContext>,
    job: &crate::domain::Job,
) -> std::result::Result<(), DeliveryError> {
    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let now = ctx.app.now_ms();
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let quarantine = |msg: String| DeliveryError::Quarantine(msg);
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
        .map_err(retry)?
        .filter(|a| a.local)
        .ok_or_else(|| quarantine("event actor is not local".into()))?;
    let public = ctx.public_url.as_ref();
    let uint = |key: &str| job.payload.get(key).and_then(Value::as_u64).map(Id::new);
    let text = |key: &str| {
        job.payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    let (bodies, inboxes): (Vec<Value>, Vec<String>) = match event.as_str() {
        "create" | "update" | "delete" => {
            let status = status_of(store, uint("status_id"))?;
            let body = match event.as_str() {
                "create" => outgoing::create(
                    store,
                    public,
                    &status,
                    now,
                    &ctx.license_policy,
                    &ctx.place_policy,
                )
                .map_err(retry)?,
                "update" => outgoing::update(
                    store,
                    public,
                    &status,
                    now,
                    &ctx.license_policy,
                    &ctx.place_policy,
                )
                .map_err(retry)?,
                _ => outgoing::delete(&author, &status),
            };
            (
                vec![body],
                status_audience(store, &author, &status).map_err(retry)?,
            )
        }
        "like" | "unlike" => {
            let target = status_of(store, uint("status_id"))?;
            let body = if event == "like" {
                outgoing::like(&author, &target)
            } else {
                outgoing::undo_like(&author, &target)
            };
            (
                vec![body],
                outgoing::inboxes_of(store, &[target.account_id]).map_err(retry)?,
            )
        }
        "poll_vote" => {
            let status = status_of(store, uint("status_id"))?;
            let owner = store
                .account_by_id(status.account_id)
                .map_err(retry)?
                .ok_or_else(|| quarantine("vote on a status whose author is missing".into()))?;
            if owner.local {
                // A vote on a local poll is already tallied; nothing to deliver.
                return Ok(());
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
                let Some(title) = poll.options.get(*choice as usize).map(|o| o.title.clone())
                else {
                    continue;
                };
                let vote_id = format!(
                    "{}#votes/{}/{}/{}",
                    author.actor_uri,
                    poll.id.value(),
                    n,
                    now
                );
                bodies.push(outgoing::vote(&author, &owner, &status, &vote_id, &title));
            }
            if bodies.is_empty() {
                return Err(quarantine("vote with no deliverable choices".into()));
            }
            (
                bodies,
                outgoing::inboxes_of(store, &[owner.id]).map_err(retry)?,
            )
        }
        "question_update" => {
            let status = status_of(store, uint("status_id"))?;
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
                // Hidden totals are never distributed (Mastodon parity).
                return Ok(());
            }
            let body = outgoing::question_update(
                store,
                public,
                &status,
                now,
                &ctx.license_policy,
                &ctx.place_policy,
            )
            .map_err(retry)?;
            (
                vec![body],
                status_audience(store, &author, &status).map_err(retry)?,
            )
        }
        "announce" => {
            let target = status_of(store, uint("status_id"))?;
            let boost = store
                .own_reblog(author.id, target.id)
                .map_err(retry)?
                .ok_or_else(|| quarantine("announce without a boost row".into()))?;
            let body = outgoing::announce(store, &author, &boost.uri, &target, boost.visibility)
                .map_err(retry)?;
            (
                vec![body],
                boost_audience(store, &author, &target, boost.visibility).map_err(retry)?,
            )
        }
        "undo_announce" => {
            let target = status_of(store, uint("status_id"))?;
            let announce_uri = text("announce_uri")
                .ok_or_else(|| quarantine("undo without an announce uri".into()))?;
            let body = outgoing::undo_announce(&author, &announce_uri, &target);
            (
                vec![body],
                outgoing::inboxes_of(store, &[target.account_id]).map_err(retry)?,
            )
        }
        "block" | "unblock" => {
            let target =
                uint("target_id").ok_or_else(|| quarantine("block without a target".into()))?;
            let target = store
                .account_by_id(target)
                .map_err(retry)?
                .ok_or_else(|| quarantine("block of a missing account".into()))?;
            let body = if event == "block" {
                outgoing::block(&author, &target)
            } else {
                outgoing::undo_block(&author, &target)
            };
            (
                vec![body],
                outgoing::inboxes_of(store, &[target.id]).map_err(retry)?,
            )
        }
        // Profile write (display name, avatar/header, fields, ...): the
        // actor's own `Update`, carrying the full actor document the crate
        // serves (same `into_json` as the `Person` endpoint, so refetch and
        // fan-out can never disagree). Recipients are the account's remote
        // followers (upstream `AccountReachFinder` is broader — reporters,
        // recent mentions/follows — documented in the federation contract).
        "profile_update" => {
            let actor_url = url::Url::parse(&author.actor_uri).map_err(|e| {
                quarantine(format!("profile update of an actor without a URL: {e}"))
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
            let person = serde_json::to_value(&person).map_err(|e| quarantine(e.to_string()))?;
            let body = outgoing::profile_update(&author, person, now);
            (
                vec![body],
                outgoing::follower_inboxes(store, author.id).map_err(retry)?,
            )
        }
        // Account deletion (D38, upstream `DeleteAccountService#delete_actor!`):
        // the actor becomes a `Tombstone` for every follower inbox. Only
        // local actors reach here (`run` quarantines remote actors above).
        "delete_actor" => {
            let body = outgoing::delete_actor(&author);
            (
                vec![body],
                outgoing::follower_inboxes(store, author.id).map_err(retry)?,
            )
        }
        "quote_request" | "quote_accept" | "quote_reject" => {
            let quote = quote_of(store, uint("quote_id"))?;
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
                &ctx.license_policy,
                &ctx.place_policy,
            )
            .map_err(retry)?;
            match event.as_str() {
                "quote_request" => (
                    vec![request],
                    outgoing::inboxes_of(store, &[quote.quoted_account_id]).map_err(retry)?,
                ),
                "quote_accept" => {
                    let approval = quote
                        .approval_uri
                        .clone()
                        .ok_or_else(|| quarantine("accept without an authorization".into()))?;
                    let body = outgoing::accept_quote(&author, &request, quote.id, &approval);
                    (
                        vec![body],
                        outgoing::inboxes_of(store, &[quote.quoting_account_id]).map_err(retry)?,
                    )
                }
                _ => {
                    let body = outgoing::reject_quote(&author, &request, quote.id);
                    (
                        vec![body],
                        outgoing::inboxes_of(store, &[quote.quoting_account_id]).map_err(retry)?,
                    )
                }
            }
        }
        "quote_revoke" => {
            let quote = quote_of(store, uint("quote_id"))?;
            let approval = quote
                .approval_uri
                .clone()
                .ok_or_else(|| quarantine("revoke without an authorization".into()))?;
            let quoting = status_of(store, Some(quote.quoting_status_id))?;
            let quoted = status_of(store, Some(quote.quoted_status_id))?;
            let body = outgoing::revoke_quote(&author, &approval, &quoting.uri, &quoted.uri);
            (
                vec![body],
                outgoing::inboxes_of(store, &[quote.quoting_account_id]).map_err(retry)?,
            )
        }
        "flag" => {
            let report = store
                .report_by_id(
                    uint("report_id").ok_or_else(|| quarantine("flag without a report".into()))?,
                )
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
            let body = outgoing::flag(&author, &target, &uris, &report.comment, report.id);
            (
                vec![body],
                outgoing::inboxes_of(store, &[target.id]).map_err(retry)?,
            )
        }
        other => return Err(quarantine(format!("unknown federation event {other}"))),
    };

    // G-D relays: public content activities also forward to each configured
    // relay peer (same fixed bytes; per-host budget via `RelayForward`).
    // Targeted activities (likes, flags, quotes, blocks, profile/deletion
    // bookkeeping) never leave their addressed inboxes, and non-public
    // statuses never reach a relay — this allowlist is deliberately narrower
    // than "everything but deletes/blocks", so moderation traffic (Flag) and
    // direct/private content cannot leak to relays.
    if matches!(
        event.as_str(),
        "create" | "update" | "delete" | "announce" | "question_update"
    ) {
        let relay_public = uint("status_id")
            .and_then(|id| store.status_by_id(id).ok().flatten())
            .is_some_and(|status| status.visibility == Visibility::Public);
        if relay_public {
            for relay in crate::federation::delivery::relay_inboxes(ctx.relays.as_slice()) {
                for body in &bodies {
                    // The relay send carries the relay host's sequence: the
                    // in-memory delivery built from this job inherits it, so
                    // relay traffic joins the same per-host FIFO.
                    let payload = crate::federation::delivery::stamp_delivery_seq(
                        store,
                        relay.as_str(),
                        serde_json::json!({
                            "sender_id": author.id.value(),
                            "relay_inbox": relay.as_str(),
                            "activity_uri": body.get("id").and_then(Value::as_str).unwrap_or_default(),
                            "body": body,
                        }),
                    )
                    .map_err(quarantine)?;
                    store
                        .enqueue(JobKind::RelayForward, payload, now)
                        .map_err(retry)?;
                }
            }
        }
    }

    for body in &bodies {
        let activity_uri = body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        tracing::debug!(target: "birchtree::federation", direction = "outbound", event = %event, activity = %activity_uri, destinations = inboxes.len(), "event planned");
        for inbox in &inboxes {
            // Plan order is sequence order: the mint and the insert are one
            // call apart per inbox, so a `Follow` planned before its `Undo`
            // always carries the smaller number.
            let payload = crate::federation::delivery::stamp_delivery_seq(
                store,
                inbox,
                serde_json::json!({
                    "sender_id": author.id.value(),
                    "inbox": inbox,
                    "activity_uri": activity_uri,
                    "body": body,
                }),
            )
            .map_err(quarantine)?;
            store
                .enqueue(JobKind::DeliverActivity, payload, now)
                .map_err(retry)?;
        }
    }
    Ok(())
}

fn status_of(store: &dyn Store, id: Option<Id>) -> std::result::Result<Status, DeliveryError> {
    let id = id.ok_or_else(|| DeliveryError::Quarantine("event has no status".into()))?;
    store
        .status_by_id(id)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine(format!("event status {id} is missing")))
}

fn quote_of(
    store: &dyn Store,
    id: Option<Id>,
) -> std::result::Result<crate::domain::QuoteRecord, DeliveryError> {
    let id = id.ok_or_else(|| DeliveryError::Quarantine("event has no quote".into()))?;
    store
        .quote_by_id(id)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine(format!("event quote {id} is missing")))
}

fn author_uri_of(store: &dyn Store, id: Id) -> std::result::Result<String, DeliveryError> {
    store
        .account_by_id(id)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .map(|a| a.actor_uri)
        .ok_or_else(|| DeliveryError::Quarantine("quote author is missing".into()))
}

/// Inboxes for a status: followers (unless direct), its mentions, and its reply's parent author.
fn status_audience(store: &dyn Store, author: &Account, status: &Status) -> Result<Vec<String>> {
    let mut ids: Vec<Id> = status.mentions.clone();
    if let Some(parent) = status.in_reply_to_account_id {
        ids.push(parent);
    }
    let mut inboxes = outgoing::inboxes_of(store, &ids)?;
    if status.visibility != Visibility::Direct {
        for inbox in outgoing::follower_inboxes(store, author.id)? {
            if !inboxes.contains(&inbox) {
                inboxes.push(inbox);
            }
        }
    }
    Ok(inboxes)
}

/// Inboxes for a boost: the followers (when the boost is visible to them) and the boosted author.
fn boost_audience(
    store: &dyn Store,
    author: &Account,
    target: &Status,
    visibility: Visibility,
) -> Result<Vec<String>> {
    let mut inboxes = outgoing::inboxes_of(store, &[target.account_id])?;
    if visibility != Visibility::Direct {
        for inbox in outgoing::follower_inboxes(store, author.id)? {
            if !inboxes.contains(&inbox) {
                inboxes.push(inbox);
            }
        }
    }
    Ok(inboxes)
}

/// Whether a quote state is one a remote should still see an activity for.
pub fn quote_is_live(state: QuoteState) -> bool {
    !matches!(
        state,
        QuoteState::Rejected | QuoteState::Revoked | QuoteState::Deleted
    )
}

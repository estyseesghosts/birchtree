//! Outbound delivery (Phase 6): the `DeliverActivity` job.
//!
//! The job's payload holds the activity JSON, fixed when the event was planned.
//! Each attempt signs and sends those exact bytes (the crate's `SendActivityTask`),
//! so a retry sends the same activity. The outcome settles the job:
//! - a 2xx: done; the destination's failure count resets.
//! - a 4xx, a 5xx, a timeout or a transport error: retry with the queue's backoff.
//!   The 4xx rule is birchtree's (D28): the middleware in `federation::net` turns
//!   a 4xx delivery into an error, so the crate does not treat it as delivered.
//! - a refused URL, a missing key, or a payload that is not an activity: permanent.
//!   The job is quarantined for an operator.
//!
//! Each destination is its own job, so one failing inbox does not hold up others.
//! On top of the per-job backoff, destinations carry a failure budget and a
//! cooldown ([`DEST_FAILURE_BUDGET`], [`dest_cooldown_ms`]): consecutive
//! failures to one host move its cooldown out exponentially, and a job aimed
//! at a host inside its cooldown is parked — completed now, re-enqueued as a
//! deferred copy due when the cooldown ends — instead of hitting the down host
//! again. Past the budget, or past [`DEST_MAX_DEFERRALS`] parks, the job is
//! quarantined. A success resets the host. Permanent (quarantine-class)
//! failures never touch the host's budget: one bad inbox must not cool down
//! its host.

use activitypub_federation::{
    activity_sending::SendActivityTask,
    config::FederationConfig,
    error::Error as ProtocolError,
    traits::ActivityHandler,
};
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::activitypub::actor::ActorRecord;
use crate::activitypub::error::FedError;
use crate::activitypub::FedContext;
use crate::domain::{DeliveryDestState, Id, Job, JobKind};

/// First cooldown after one failed attempt to a host.
pub const DEST_COOLDOWN_BASE_MS: u64 = 60_000;
/// The cooldown never moves past this.
pub const DEST_COOLDOWN_CAP_MS: u64 = 3_600_000;
/// Consecutive failed attempts to a host before its jobs quarantine fast.
pub const DEST_FAILURE_BUDGET: u32 = 10;
/// Times a job may be parked for a cooling destination before quarantine.
pub const DEST_MAX_DEFERRALS: u32 = 25;

/// The cooldown after `failures` consecutive failures: the base, doubled per
/// failure past the first, capped.
pub fn dest_cooldown_ms(failures: u32) -> u64 {
    let shift = failures.saturating_sub(1).min(16);
    DEST_COOLDOWN_BASE_MS.saturating_mul(1 << shift).min(DEST_COOLDOWN_CAP_MS)
}

/// The failure domain of an inbox URL: its lowercased host, with the explicit
/// port when one is named. Inboxes on one host share a budget and a cooldown.
pub fn dest_host(inbox: &Url) -> String {
    match (inbox.host_str(), inbox.port()) {
        (Some(host), Some(port)) => format!("{}:{port}", host.to_lowercase()),
        (Some(host), None) => host.to_lowercase(),
        (None, _) => inbox.as_str().to_lowercase(),
    }
}

/// How a failed delivery settles its job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    /// Try again later, with the queue's backoff.
    Retry(String),
    /// Stop: quarantine the job for an operator.
    Quarantine(String),
}

/// An outbound activity: the stored JSON, as sent. The crate needs its ID and
/// actor to sign it; the body is serialized as it is stored.
#[derive(Debug, Clone)]
pub struct OutboundActivity {
    id: Url,
    actor: Url,
    body: Value,
}

impl OutboundActivity {
    /// Read the ID and actor of a stored activity.
    pub fn from_body(body: Value) -> Result<Self, String> {
        let id = body
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "activity has no id".to_string())?;
        let actor = body
            .get("actor")
            .and_then(Value::as_str)
            .ok_or_else(|| "activity has no actor".to_string())?;
        Ok(Self {
            id: Url::parse(id).map_err(|e| e.to_string())?,
            actor: Url::parse(actor).map_err(|e| e.to_string())?,
            body,
        })
    }
}

impl Serialize for OutboundActivity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.body.serialize(serializer)
    }
}

#[async_trait::async_trait]
impl ActivityHandler for OutboundActivity {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        &self.actor
    }

    async fn verify(&self, _data: &activitypub_federation::config::Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, _data: &activitypub_federation::config::Data<FedContext>) -> Result<(), FedError> {
        Err(FedError::Refused("an outbound activity is not received here".into()))
    }
}

/// Attempt one delivery. `job` must be a `DeliverActivity` job.
pub async fn run(config: &FederationConfig<FedContext>, job: &Job) -> Result<(), DeliveryError> {
    if job.kind != JobKind::DeliverActivity {
        return Err(DeliveryError::Quarantine("not a delivery job".into()));
    }
    let payload = &job.payload;
    let sender = payload
        .get("sender_id")
        .and_then(Value::as_u64)
        .map(Id::new)
        .ok_or_else(|| DeliveryError::Quarantine("delivery has no sender".into()))?;
    let inbox = payload
        .get("inbox")
        .and_then(Value::as_str)
        .ok_or_else(|| DeliveryError::Quarantine("delivery has no inbox".into()))?;
    let inbox_url = Url::parse(inbox).map_err(|_| DeliveryError::Quarantine("delivery inbox is not a URL".into()))?;
    let body = payload
        .get("body")
        .cloned()
        .ok_or_else(|| DeliveryError::Quarantine("delivery has no body".into()))?;
    let activity = OutboundActivity::from_body(body).map_err(|e| DeliveryError::Quarantine(format!("delivery body is not an activity: {e}")))?;

    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let now = ctx.app.now_ms();
    let host = dest_host(&inbox_url);
    // A host inside its cooldown is not hit again: park this job as a
    // deferred copy due when the cooldown ends. The park does not consume
    // the job's retry budget; past the deferral cap the job quarantines.
    if let Some(state) = store
        .delivery_dest(&host)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        && now < state.cooldown_until_ms
    {
        let deferrals = payload.get("deferrals").and_then(Value::as_u64).unwrap_or(0);
        if deferrals >= u64::from(DEST_MAX_DEFERRALS) {
            return Err(DeliveryError::Quarantine(format!(
                "destination {host} still cooling down after {deferrals} deferrals"
            )));
        }
        let mut parked = payload.clone();
        if let Some(object) = parked.as_object_mut() {
            object.insert("deferrals".into(), Value::from(deferrals + 1));
        }
        store
            .enqueue_at(JobKind::DeliverActivity, parked, state.cooldown_until_ms, now)
            .map_err(|e| DeliveryError::Retry(e.to_string()))?;
        tracing::info!(
            target: "birchtree::federation",
            direction = "outbound",
            inbox,
            host = %host,
            deferrals = deferrals + 1,
            outcome = "parked: destination cooling down",
            "delivery deferred"
        );
        return Ok(());
    }

    let account = store
        .account_by_id(sender)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine("sender account is gone".into()))?;
    let keys = store
        .actor_keys(sender)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .filter(|keys| keys.private_key_pem.is_some())
        .ok_or_else(|| DeliveryError::Quarantine("sender has no local signing key".into()))?;
    let sender_actor = ActorRecord::new(account, keys).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;

    let data = config.to_request_data();
    let tasks = SendActivityTask::prepare(&activity, &sender_actor, vec![inbox_url], &data)
        .await
        .map_err(classify)?;
    if tasks.is_empty() {
        return Err(DeliveryError::Quarantine(format!("inbox {inbox} refused by federation policy")));
    }
    for task in tasks {
        match task.sign_and_send(&data).await {
            Ok(()) => {
                // A success resets the host: the outage, if any, is over.
                if store.delivery_dest(&host).map_err(|e| DeliveryError::Retry(e.to_string()))?.is_some() {
                    store
                        .set_delivery_dest(&host, DeliveryDestState { failures: 0, cooldown_until_ms: 0 })
                        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
                }
                tracing::info!(
                    target: "birchtree::federation",
                    direction = "outbound",
                    inbox,
                    activity = %activity.id(),
                    outcome = "delivered (2xx)",
                    "activity sent"
                );
            }
            Err(err) => {
                let settled = classify(err);
                // Only a retryable failure charges the host's budget: a
                // permanent refusal is one bad inbox, not a sick host.
                if let DeliveryError::Retry(msg) = &settled {
                    let failures = store
                        .delivery_dest(&host)
                        .map_err(|e| DeliveryError::Retry(e.to_string()))?
                        .map(|s| s.failures)
                        .unwrap_or(0)
                        .saturating_add(1);
                    let state = DeliveryDestState {
                        failures,
                        cooldown_until_ms: now.saturating_add(dest_cooldown_ms(failures)),
                    };
                    store
                        .set_delivery_dest(&host, state)
                        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
                    if failures > DEST_FAILURE_BUDGET {
                        return Err(DeliveryError::Quarantine(format!(
                            "destination {host} failed {failures} times in a row; budget exhausted: {msg}"
                        )));
                    }
                }
                return Err(settled);
            }
        }
    }
    Ok(())
}

fn classify(err: ProtocolError) -> DeliveryError {
    match err {
        ProtocolError::UrlVerificationError(_) | ProtocolError::SerializeOutgoingActivity(..) => {
            DeliveryError::Quarantine(err.to_string())
        }
        other => DeliveryError::Retry(other.to_string()),
    }
}

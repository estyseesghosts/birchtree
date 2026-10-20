//! Outbound delivery (Phase 6): the `DeliverActivity` job.
//!
//! The job's payload holds the activity JSON, fixed when the event was planned.
//! Each attempt signs and sends those exact bytes (the crate's `SendActivityTask`),
//! so a retry sends the same activity. The outcome settles the job:
//! - a 2xx: done.
//! - a 4xx, a 5xx, a timeout or a transport error: retry with the queue's backoff.
//!   The 4xx rule is birchtree's (D28): the middleware in `federation::net` turns
//!   a 4xx into an error, so the crate does not treat it as delivered.
//! - a refused URL, a missing key, or a payload that is not an activity: permanent.
//!   The job is quarantined for an operator.
//!
//! Each destination is its own job, so one failing inbox does not hold up others.

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
use crate::domain::{Id, Job, JobKind};

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
    let account = ctx
        .app
        .store
        .account_by_id(sender)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine("sender account is gone".into()))?;
    let keys = ctx
        .app
        .store
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
        task.sign_and_send(&data).await.map_err(classify)?;
        tracing::info!(
            target: "birchtree::federation",
            direction = "outbound",
            inbox,
            activity = %activity.id(),
            outcome = "delivered (2xx)",
            "activity sent"
        );
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

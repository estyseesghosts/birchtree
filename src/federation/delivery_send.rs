//! Deferred-send helpers extracted from [`crate::federation::delivery::run`].
//!
//! The parent keeps the sequence gate, the send, and the tracing; these
//! helpers own the failure taxonomy and the per-host budget ledger, verbatim:
//! [`classify`] maps a send error to its settlement, [`cooldown_host`]
//! reports whether a host is cooling, [`charge_budget`] records one
//! retryable failure, and [`enqueue_deferred`] parks a copy due later.
//! Budget constants stay canonical in `delivery` and are referenced, never
//! redefined (see `DEST_FAILURE_BUDGET`).

use activitypub_federation::activity_sending::SendActivityTask;
use activitypub_federation::config::FederationConfig;
use activitypub_federation::error::Error as ProtocolError;
use activitypub_federation::traits::ActivityHandler;
use serde_json::Value;
use url::Url;

use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::application::ports::Store;
use crate::domain::DeliveryDestState;
use crate::domain::Id;
use crate::domain::Job;
use crate::domain::JobKind;
use crate::federation::delivery;
use crate::federation::delivery::DeliveryError;

/// Settle a failed send: refused URLs, missing keys and non-activity
/// payloads quarantine; anything else retries with the queue's backoff.
pub fn classify(err: ProtocolError) -> DeliveryError {
    match err {
        ProtocolError::UrlVerificationError(_) | ProtocolError::SerializeOutgoingActivity(..) => {
            DeliveryError::Quarantine(err.to_string())
        }
        other => DeliveryError::Retry(other.to_string()),
    }
}

/// Whether `host` is inside its cooldown at `now_ms`: returns the stored
/// destination state while cooling, `None` once the cooldown has elapsed.
pub fn cooldown_host(
    store: &dyn Store,
    host: &str,
    now_ms: u64,
) -> Result<Option<DeliveryDestState>, DeliveryError> {
    if let Some(state) = store
        .delivery_dest(host)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        && now_ms < state.cooldown_until_ms
    {
        return Ok(Some(state));
    }
    Ok(None)
}

/// Charge one retryable failure to `host`: the consecutive-failure count
/// grows and the cooldown moves out exponentially. Past
/// [`delivery::DEST_FAILURE_BUDGET`] the settlement becomes a quarantine;
/// a store failure settles as a retry. Permanent (quarantine-class)
/// failures must never call this: one bad inbox must not cool down its host.
pub fn charge_budget(store: &dyn Store, host: &str, now_ms: u64, message: String) -> DeliveryError {
    let failures = match store.delivery_dest(host) {
        Ok(found) => found
            .map(|state| state.failures)
            .unwrap_or(0)
            .saturating_add(1),
        Err(error) => return DeliveryError::Retry(error.to_string()),
    };
    let state = DeliveryDestState {
        failures,
        cooldown_until_ms: now_ms.saturating_add(delivery::dest_cooldown_ms(failures)),
    };
    if let Err(error) = store.set_delivery_dest(host, state) {
        return DeliveryError::Retry(error.to_string());
    }
    if failures > delivery::DEST_FAILURE_BUDGET {
        return DeliveryError::Quarantine(format!(
            "destination {host} failed {failures} times in a row; budget exhausted: {message}"
        ));
    }
    DeliveryError::Retry(message)
}

/// Park `payload` as a deferred copy of `kind` due at `due_ms`, stamped at
/// `now_ms`. Parking never consumes the job's retry budget.
pub fn enqueue_deferred(
    store: &dyn Store,
    kind: JobKind,
    payload: Value,
    due_ms: u64,
    now_ms: u64,
) -> Result<(), DeliveryError> {
    store
        .enqueue_at(kind, payload, due_ms, now_ms)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    Ok(())
}

/// A validated `DeliverActivity` payload: everything
/// [`crate::federation::delivery::run`] needs after the per-host lock is
/// taken. All strings are owned so the struct can outlive the job borrow.
pub struct ParsedDelivery {
    /// Local sender account id (`sender_id`).
    pub sender: Id,
    /// Destination inbox URL as stored in the payload.
    pub inbox: String,
    /// Parsed destination inbox URL.
    pub inbox_url: Url,
    /// The fixed activity bytes, as sent.
    pub activity: super::delivery::OutboundActivity,
    /// Lowercased failure domain of the inbox (see
    /// [`super::delivery::dest_host`]).
    pub host: String,
    /// Destination-host monotonic sequence (`seq`, 0 when unstamped).
    pub seq: u64,
}

/// Validate a `DeliverActivity` job into its send inputs: kind, sender,
/// inbox, body-as-activity, host and sequence. Every rejection string is
/// verbatim from [`crate::federation::delivery::run`].
pub fn parse_delivery_job(job: &Job) -> Result<ParsedDelivery, DeliveryError> {
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
    let inbox_url = Url::parse(inbox)
        .map_err(|_| DeliveryError::Quarantine("delivery inbox is not a URL".into()))?;
    let body = payload
        .get("body")
        .cloned()
        .ok_or_else(|| DeliveryError::Quarantine("delivery has no body".into()))?;
    let activity = super::delivery::OutboundActivity::from_body(body)
        .map_err(|e| DeliveryError::Quarantine(format!("delivery body is not an activity: {e}")))?;
    let host = super::delivery::dest_host(&inbox_url);
    let seq = payload.get("seq").and_then(Value::as_u64).unwrap_or(0);
    Ok(ParsedDelivery {
        sender,
        inbox: inbox.to_string(),
        inbox_url,
        activity,
        host,
        seq,
    })
}

/// Per-destination FIFO gate (verbatim from `delivery::run`): while an
/// earlier sequence for `parsed.host` is still unfinished, park this job as
/// a deferred copy and report `true` (the caller then returns `Ok(())`).
/// Past [`super::delivery::SEQ_PARK_MAX`] parks (or
/// [`super::delivery::SEQ_PARK_MAX_AGE_MS`] since the first park) the
/// ordering relaxes and this reports `false`. The park goes through
/// [`enqueue_deferred`]; constants are referenced from `delivery`, never
/// redefined.
pub fn park_if_sequence_blocked(
    store: &dyn Store,
    parsed: &ParsedDelivery,
    job: &Job,
    now_ms: u64,
) -> Result<bool, DeliveryError> {
    let inbox = parsed.inbox.as_str();
    let host = parsed.host.as_str();
    let seq = parsed.seq;
    if store
        .delivery_blocked_by(host, seq)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
    {
        let payload = &job.payload;
        let parks = payload
            .get("seq_parks")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let first_at = payload
            .get("seq_park_first_at")
            .and_then(Value::as_u64)
            .unwrap_or(now_ms);
        let past_parks = parks >= u64::from(super::delivery::SEQ_PARK_MAX);
        let past_age = now_ms.saturating_sub(first_at) >= super::delivery::SEQ_PARK_MAX_AGE_MS;
        if past_parks || past_age {
            tracing::info!(
                target: "birchtree::federation",
                direction = "outbound",
                inbox,
                host = %host,
                seq,
                seq_parks = parks,
                outcome = "ordering-relaxed: earlier sequence still unfinished past cap",
                "delivery sent out of order"
            );
        } else {
            let mut parked = payload.clone();
            if let Some(object) = parked.as_object_mut() {
                object.insert("seq_parks".into(), Value::from(parks + 1));
                if payload.get("seq_park_first_at").is_none() {
                    object.insert("seq_park_first_at".into(), Value::from(now_ms));
                }
            }
            enqueue_deferred(
                store,
                JobKind::DeliverActivity,
                parked,
                now_ms.saturating_add(super::delivery::SEQ_PARK_DELAY_MS),
                now_ms,
            )?;
            tracing::info!(
                target: "birchtree::federation",
                direction = "outbound",
                inbox,
                host = %host,
                seq,
                seq_parks = parks + 1,
                outcome = "parked: earlier sequence unfinished",
                "delivery deferred"
            );
            return Ok(true);
        }
    }
    Ok(false)
}

/// Cooldown park (verbatim from `delivery::run`): while `parsed.host` is
/// inside its cooldown, park this job as a deferred copy due when the
/// cooldown ends and report `true`. Past
/// [`super::delivery::DEST_MAX_DEFERRALS`] parks the job quarantines. The
/// store read delegates to [`cooldown_host`] and the park to
/// [`enqueue_deferred`].
pub fn park_if_cooling(
    store: &dyn Store,
    parsed: &ParsedDelivery,
    job: &Job,
    now_ms: u64,
) -> Result<bool, DeliveryError> {
    let inbox = parsed.inbox.as_str();
    let host = parsed.host.as_str();
    if let Some(state) = cooldown_host(store, host, now_ms)? {
        let deferrals = job
            .payload
            .get("deferrals")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if deferrals >= u64::from(super::delivery::DEST_MAX_DEFERRALS) {
            return Err(DeliveryError::Quarantine(format!(
                "destination {host} still cooling down after {deferrals} deferrals"
            )));
        }
        let mut parked = job.payload.clone();
        if let Some(object) = parked.as_object_mut() {
            object.insert("deferrals".into(), Value::from(deferrals + 1));
        }
        enqueue_deferred(
            store,
            JobKind::DeliverActivity,
            parked,
            state.cooldown_until_ms,
            now_ms,
        )?;
        tracing::info!(
            target: "birchtree::federation",
            direction = "outbound",
            inbox,
            host = %host,
            deferrals = deferrals + 1,
            outcome = "parked: destination cooling down",
            "delivery deferred"
        );
        return Ok(true);
    }
    Ok(false)
}

/// Load the sender's local signing identity (verbatim from `delivery::run`):
/// a present account plus actor keys with a private key, combined into an
/// [`ActorRecord`]. Store failures retry; missing account/keys quarantine.
pub fn load_sender_keys(store: &dyn Store, sender: Id) -> Result<ActorRecord, DeliveryError> {
    let account = store
        .account_by_id(sender)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .ok_or_else(|| DeliveryError::Quarantine("sender account is gone".into()))?;
    let keys = store
        .actor_keys(sender)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        .filter(|keys| keys.private_key_pem.is_some())
        .ok_or_else(|| DeliveryError::Quarantine("sender has no local signing key".into()))?;
    ActorRecord::new(account, keys).map_err(|e| DeliveryError::Quarantine(e.to_string()))
}

/// Sign and send one parsed delivery, then settle the job (verbatim from
/// `delivery::run`): a 2xx resets the host, a policy refusal quarantines,
/// and any other failure goes through [`charge_budget`] so only retryable
/// failures touch the host budget.
pub async fn send_and_settle(
    config: &FederationConfig<FedContext>,
    store: &dyn Store,
    parsed: &ParsedDelivery,
    sender_actor: &ActorRecord,
    now_ms: u64,
) -> Result<(), DeliveryError> {
    let inbox = parsed.inbox.as_str();
    let host = parsed.host.as_str();
    let data = config.to_request_data();
    let tasks = SendActivityTask::prepare(
        &parsed.activity,
        sender_actor,
        vec![parsed.inbox_url.clone()],
        &data,
    )
    .await
    .map_err(classify)?;
    if tasks.is_empty() {
        return Err(DeliveryError::Quarantine(format!(
            "inbox {inbox} refused by federation policy"
        )));
    }
    for task in tasks {
        match task.sign_and_send(&data).await {
            Ok(()) => {
                // A success resets the host: the outage, if any, is over.
                if store
                    .delivery_dest(host)
                    .map_err(|e| DeliveryError::Retry(e.to_string()))?
                    .is_some()
                {
                    store
                        .set_delivery_dest(
                            host,
                            DeliveryDestState {
                                failures: 0,
                                cooldown_until_ms: 0,
                            },
                        )
                        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
                }
                tracing::info!(
                    target: "birchtree::federation",
                    direction = "outbound",
                    inbox,
                    activity = %parsed.activity.id(),
                    outcome = "delivered (2xx)",
                    "activity sent"
                );
            }
            Err(err) => {
                let settled = classify(err);
                // Only a retryable failure charges the host's budget: a
                // permanent refusal is one bad inbox, not a sick host.
                if let DeliveryError::Retry(message) = &settled {
                    return Err(charge_budget(store, host, now_ms, message.clone()));
                }
                return Err(settled);
            }
        }
    }
    Ok(())
}

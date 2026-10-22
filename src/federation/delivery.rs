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
//!
//! Ordering note (Mastodon baseline): a remote server applies `Follow`/`Undo`,
//! `Create`/`Delete` and `Announce`/`Undo Announce` in arrival order per
//! sender. Each destination is its own job and the worker executes a claimed
//! batch concurrently, so two jobs for the same inbox would race on the wire
//! without per-destination sequencing. Every delivery payload carries its
//! destination host's monotonic `seq` (minted at enqueue), and [`run`]
//! parks sequence N+1 while N is still unfinished — so same-host deliveries
//! go out in plan order while different hosts proceed in parallel. The
//! per-host lock serializes the check-and-send; the sequence makes it FIFO,
//! across restarts too (the counter is durable).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use activitypub_federation::{
    activity_sending::SendActivityTask, config::FederationConfig, error::Error as ProtocolError,
    traits::ActivityHandler,
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::activitypub::error::FedError;
use crate::domain::{DeliveryDestState, Id, Job, JobKind};

/// First cooldown after one failed attempt to a host.
pub const DEST_COOLDOWN_BASE_MS: u64 = 60_000;
/// The cooldown never moves past this.
pub const DEST_COOLDOWN_CAP_MS: u64 = 3_600_000;
/// Consecutive failed attempts to a host before its jobs quarantine fast.
pub const DEST_FAILURE_BUDGET: u32 = 10;
/// Times a job may be parked for a cooling destination before quarantine.
pub const DEST_MAX_DEFERRALS: u32 = 25;
/// Delay before a sequence-parked delivery is retried: long enough to let a
/// just-finished earlier send settle, short enough that a genuinely slow
/// earlier send only costs a few park cycles. Parking never consumes the
/// job's retry budget and never quarantines: the blocker always settles
/// terminally (success, quarantine, or retry-budget exhaustion), so a parked
/// job always makes progress.
pub const SEQ_PARK_DELAY_MS: u64 = 5_000;

/// The cooldown after `failures` consecutive failures: the base, doubled per
/// failure past the first, capped.
pub fn dest_cooldown_ms(failures: u32) -> u64 {
    let shift = failures.saturating_sub(1).min(16);
    DEST_COOLDOWN_BASE_MS
        .saturating_mul(1 << shift)
        .min(DEST_COOLDOWN_CAP_MS)
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

// ---------------------------------------------------------------------------
// Per-destination ordering: the worker fans a claimed batch out concurrently,
// so without serialization two jobs for the same inbox (a `Follow` and its
// `Undo`, a `Create` and its `Delete`) could interleave on the wire and the
// remote would apply them out of order. Ordering is a durable per-host
// sequence, not the lock:
//
// - every `DeliverActivity` (and `RelayForward`) enqueue mints the next
//   monotonic number for its destination host into the payload as `seq`
//   (see [`stamp_delivery_seq`]; the `DELIVERY_SEQ` table survives restarts,
//   so order survives them too);
// - [`run`] holds the per-host lock ([`destination_lock`]) and then refuses
//   to send sequence N+1 while any earlier sequence for the same host is
//   still `Pending` or `Leased`: the job parks (a deferred copy, no budget
//   consumed) instead of jumping the queue. Terminal rows (`Succeeded`,
//   `Quarantined`) never block — a permanently failed earlier send must not
//   wedge its host (there is no operator-resend path that could revive it).
//
// The per-host mutex remains as the concurrency control (one send per host
// at a time); the sequence is what makes the order FIFO.
// ---------------------------------------------------------------------------

/// Registry of per-destination-host serialization locks, keyed by
/// [`dest_host`]. Entries are `Weak`: a host with no in-flight or waiting
/// delivery releases its lock instead of growing the map without bound.
static DESTINATION_LOCKS: OnceLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> = OnceLock::new();

fn destination_locks() -> &'static Mutex<HashMap<String, Weak<AsyncMutex<()>>>> {
    DESTINATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The serialization lock for one destination host.
///
/// Same-host deliveries hold this across the cooldown check and the send (see
/// [`run`]), so they never interleave; different hosts hold different locks.
/// The registry evicts dead entries on lookup, so idle hosts leave no trace.
pub fn destination_lock(host: &str) -> Arc<AsyncMutex<()>> {
    let registry = destination_locks();
    let mut locks = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(live) = locks.get(host).and_then(Weak::upgrade) {
        return live;
    }
    // Evict dead entries while a write lock is already held: keeps the map
    // proportional to the live destination set.
    locks.retain(|_, weak| weak.upgrade().is_some());
    let fresh = Arc::new(AsyncMutex::new(()));
    locks.insert(host.to_string(), Arc::downgrade(&fresh));
    fresh
}

/// Ordering key for one delivery job: the destination host plus the activity
/// id being sent. A `Follow` and its `Undo` (or a `Create` and its `Delete`)
/// to the same inbox share the host half and differ in the activity half, so
/// an operator (or the ordering harness in `tests/fix_worker_sched.rs`) can
/// audit that the pair stayed serialized. Returns `None` when the job has no
/// parseable inbox or no activity id.
pub fn delivery_order_key(job: &Job) -> Option<(String, String)> {
    let inbox = job.payload.get("inbox")?.as_str()?;
    let host = Url::parse(inbox).ok().map(|url| dest_host(&url))?;
    let activity = job.payload.get("body")?.get("id")?.as_str()?.to_string();
    Some((host, activity))
}

/// Stamp a delivery payload with its destination's next monotonic sequence
/// number (`seq`). Every `DeliverActivity` (and `RelayForward`) enqueue goes
/// through here, so plan order and sequence order are identical: the counter
/// mint and the job insert are one call apart at the plan site (or one
/// transaction apart for in-transaction planning), and the worker never
/// reorders on the wire (see the per-destination ordering note above).
pub fn stamp_delivery_seq(
    store: &dyn crate::application::ports::Store,
    inbox: &str,
    payload: serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let host = Url::parse(inbox)
        .map(|url| dest_host(&url))
        .unwrap_or_else(|_| inbox.to_lowercase());
    let seq = store.delivery_seq_next(&host).map_err(|e| e.to_string())?;
    let mut payload = payload;
    if let Some(object) = payload.as_object_mut() {
        object.insert("seq".into(), Value::from(seq));
    }
    Ok(payload)
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

    async fn verify(
        &self,
        _data: &activitypub_federation::config::Data<FedContext>,
    ) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(
        self,
        _data: &activitypub_federation::config::Data<FedContext>,
    ) -> Result<(), FedError> {
        Err(FedError::Refused(
            "an outbound activity is not received here".into(),
        ))
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
    let inbox_url = Url::parse(inbox)
        .map_err(|_| DeliveryError::Quarantine("delivery inbox is not a URL".into()))?;
    let body = payload
        .get("body")
        .cloned()
        .ok_or_else(|| DeliveryError::Quarantine("delivery has no body".into()))?;
    let activity = OutboundActivity::from_body(body)
        .map_err(|e| DeliveryError::Quarantine(format!("delivery body is not an activity: {e}")))?;

    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let now = ctx.app.now_ms();
    let host = dest_host(&inbox_url);
    // Serialize with the other in-flight deliveries to this host (see the
    // per-destination ordering note above): the guard spans the cooldown
    // check and the send, so a `Follow`/`Undo` pair to one inbox cannot
    // interleave. Other hosts take other locks and proceed in parallel.
    let destination_guard_lock = destination_lock(&host);
    let _destination_guard = destination_guard_lock.lock().await;
    // Per-destination FIFO gate: sequence N+1 for this host must not send
    // while N is still unfinished (`Pending` or `Leased`). Park instead — a
    // deferred copy due shortly, no retry budget consumed — so a `Follow`
    // and its `Undo` can never go out on the wire even when the worker runs
    // them concurrently or a restart reorders the claims. Terminal rows
    // never block (see the ordering note above).
    let seq = payload.get("seq").and_then(Value::as_u64).unwrap_or(0);
    if store
        .delivery_blocked_by(&host, seq)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
    {
        let parks = payload.get("seq_parks").and_then(Value::as_u64).unwrap_or(0);
        let mut parked = payload.clone();
        if let Some(object) = parked.as_object_mut() {
            object.insert("seq_parks".into(), Value::from(parks + 1));
        }
        store
            .enqueue_at(
                JobKind::DeliverActivity,
                parked,
                now.saturating_add(SEQ_PARK_DELAY_MS),
                now,
            )
            .map_err(|e| DeliveryError::Retry(e.to_string()))?;
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
        return Ok(());
    }
    // A host inside its cooldown is not hit again: park this job as a
    // deferred copy due when the cooldown ends. The park does not consume
    // the job's retry budget; past the deferral cap the job quarantines.
    if let Some(state) = store
        .delivery_dest(&host)
        .map_err(|e| DeliveryError::Retry(e.to_string()))?
        && now < state.cooldown_until_ms
    {
        let deferrals = payload
            .get("deferrals")
            .and_then(Value::as_u64)
            .unwrap_or(0);
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
            .enqueue_at(
                JobKind::DeliverActivity,
                parked,
                state.cooldown_until_ms,
                now,
            )
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
    let sender_actor =
        ActorRecord::new(account, keys).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;

    let data = config.to_request_data();
    let tasks = SendActivityTask::prepare(&activity, &sender_actor, vec![inbox_url], &data)
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
                    .delivery_dest(&host)
                    .map_err(|e| DeliveryError::Retry(e.to_string()))?
                    .is_some()
                {
                    store
                        .set_delivery_dest(
                            &host,
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

// ---------------------------------------------------------------------------
// Relays + inbox forwarding (federation tail, G-D).
// ---------------------------------------------------------------------------

/// Largest relayed/inbound-forwarded activity body (matches the 1 MB inbound
/// activity bound, D27): federation input is untrusted on every path.
pub const RELAY_MAX_BODY_BYTES: u64 = 1_048_576;
/// Destinations cap per inbox-forwarding plan (bounded fan-out per job).
pub const FORWARD_MAX_DESTINATIONS: usize = 1_000;

/// Parse operator-configured relay inbox URLs (proposed `relays` config keys,
/// parent-merged): only `http(s)` URLs without credentials survive. Anything
/// else is dropped, never quarantined (operator config, not remote input).
pub fn relay_inboxes(configured: &[String]) -> Vec<Url> {
    configured
        .iter()
        .filter_map(|raw| Url::parse(raw).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .filter(|url| url.username().is_empty() && url.password().is_none())
        .filter(|url| url.host_str().is_some())
        .collect()
}

/// Whether an inbound activity type is forwarded to local followers' remote
/// followers (Mastodon inbox-forwarding shape): public boosts and public
/// creations travel on; anything else (likes, flags, private addressing)
/// never forwards. Audience checks stay with the caller, which knows the
/// activity's `to`/`cc`.
pub fn should_forward_inbound(activity_type: &str) -> bool {
    matches!(activity_type, "Announce" | "Create")
}

/// Plan inbox forwarding for an activity authored by `author_id`: one
/// `DeliverActivity` job per distinct remote follower inbox (shared inboxes
/// deduped by [`crate::activitypub::outgoing::follower_inboxes`]), excluding
/// `exclude_inbox` (the inbox the activity arrived from, so a relay loop
/// cannot bounce it straight back). Returns the planned destination count.
///
/// The payload is fixed at plan time, so retries send the same bytes; each
/// destination settles under the same per-host budget/cooldown as ordinary
/// delivery (reachability claim D49 stands for forwarded traffic too).
pub fn plan_inbox_forward(
    store: &dyn crate::application::ports::Store,
    author_id: Id,
    activity_uri: &str,
    body: &serde_json::Value,
    exclude_inbox: Option<&str>,
    now_ms: u64,
) -> Result<usize, String> {
    let body_bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    if body_bytes.len() as u64 > RELAY_MAX_BODY_BYTES {
        return Err("forwarded activity is larger than the limit".into());
    }
    let author = store
        .account_by_id(author_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "forwarding author is missing".to_string())?;
    let mut inboxes = crate::activitypub::outgoing::follower_inboxes(store, author.id)
        .map_err(|e| e.to_string())?;
    if let Some(exclude) = exclude_inbox {
        inboxes.retain(|inbox| inbox != exclude);
    }
    inboxes.truncate(FORWARD_MAX_DESTINATIONS);
    for inbox in &inboxes {
        // Plan order is sequence order (see [`stamp_delivery_seq`]).
        let payload = stamp_delivery_seq(
            store,
            inbox,
            serde_json::json!({
                "sender_id": author.id.value(),
                "inbox": inbox,
                "activity_uri": activity_uri,
                "body": body,
            }),
        )
        .map_err(|e| e.to_string())?;
        store
            .enqueue(JobKind::DeliverActivity, payload, now_ms)
            .map_err(|e| e.to_string())?;
    }
    Ok(inboxes.len())
}

/// Run one `RelayForward` job (payload `{sender_id, relay_inbox|inbox, body,
/// activity_uri?}`): forward a fixed activity to one relay peer. The send
/// reuses [`run`], so relay traffic carries the same signatures, per-host
/// failure budget/cooldown and quarantine behavior as ordinary delivery —
/// that is the reachability claim. Oversize bodies and unparseable payloads
/// quarantine without touching any host budget.
pub async fn run_relay_forward(
    config: &FederationConfig<FedContext>,
    job: &Job,
) -> Result<(), DeliveryError> {
    let payload = &job.payload;
    let sender = payload
        .get("sender_id")
        .and_then(Value::as_u64)
        .map(Id::new)
        .ok_or_else(|| DeliveryError::Quarantine("relay forward has no sender".into()))?;
    let relay = payload
        .get("relay_inbox")
        .or_else(|| payload.get("inbox"))
        .and_then(Value::as_str)
        .ok_or_else(|| DeliveryError::Quarantine("relay forward has no relay inbox".into()))?;
    let relay_url = Url::parse(relay)
        .map_err(|_| DeliveryError::Quarantine("relay inbox is not a URL".into()))?;
    if !matches!(relay_url.scheme(), "http" | "https") || relay_url.host_str().is_none() {
        return Err(DeliveryError::Quarantine(
            "relay inbox is not an http(s) URL".into(),
        ));
    }
    let body = payload
        .get("body")
        .cloned()
        .ok_or_else(|| DeliveryError::Quarantine("relay forward has no body".into()))?;
    let body_bytes = serde_json::to_vec(&body)
        .map_err(|e| DeliveryError::Quarantine(format!("relay body is not JSON: {e}")))?;
    if body_bytes.len() as u64 > RELAY_MAX_BODY_BYTES {
        return Err(DeliveryError::Quarantine(
            "relay body is larger than the limit".into(),
        ));
    }
    // The activity must be sendable (an id + actor to sign with); anything
    // else quarantines before any host state is touched.
    OutboundActivity::from_body(body.clone())
        .map_err(|e| DeliveryError::Quarantine(format!("relay body is not an activity: {e}")))?;
    let activity_uri = payload
        .get("activity_uri")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let forwarded = Job {
        id: job.id,
        kind: JobKind::DeliverActivity,
        payload: serde_json::json!({
            "sender_id": sender.value(),
            "inbox": relay,
            "activity_uri": activity_uri,
            "body": body,
            // The relay host's sequence rides along from the `RelayForward`
            // enqueue, so the send joins the same per-host FIFO.
            "seq": job.payload.get("seq").cloned().unwrap_or(serde_json::Value::Null),
        }),
        state: job.state,
        due_at_ms: job.due_at_ms,
        lease_until_ms: job.lease_until_ms,
        attempts: job.attempts,
        max_attempts: job.max_attempts,
        last_error: job.last_error.clone(),
        created_at_ms: job.created_at_ms,
    };
    run(config, &forwarded).await
}

//! Deferred-send helpers extracted from [`crate::federation::delivery::run`].
//!
//! The parent keeps the sequence gate, the send, and the tracing; these
//! helpers own the failure taxonomy and the per-host budget ledger, verbatim:
//! [`classify`] maps a send error to its settlement, [`cooldown_host`]
//! reports whether a host is cooling, [`charge_budget`] records one
//! retryable failure, and [`enqueue_deferred`] parks a copy due later.
//! Budget constants stay canonical in `delivery` and are referenced, never
//! redefined (see `DEST_FAILURE_BUDGET`).

use activitypub_federation::error::Error as ProtocolError;
use serde_json::Value;

use crate::application::ports::Store;
use crate::domain::DeliveryDestState;
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

//! Single-flight generation of local actor keys.
//!
//! Generating a 2048-bit RSA key is expensive, so concurrent first requests for
//! one account must not each generate a pair. Callers for the same account
//! queue on a per-account gate; the first one generates, and later ones find the
//! stored pair when their turn comes. The store's atomic insert still decides the
//! winner: if another process won, its pair is returned and the generated one is
//! discarded.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use activitypub_federation::error::Error as ProtocolError;
use activitypub_federation::http_signatures::Keypair;

use crate::activitypub::error::FedError;
use crate::application::federation_ports::FederationStore;
use crate::domain::{ActorKeys, Id};

type AccountGate = Arc<tokio::sync::Mutex<()>>;

// Shared across all callers in the process; entries are removed once nobody waits on them.
static GATES: LazyLock<Mutex<HashMap<u64, AccountGate>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn gate_for(account_id: Id) -> Result<AccountGate, FedError> {
    let mut gates = GATES
        .lock()
        .map_err(|_| FedError::Refused("key generation gate poisoned".into()))?;
    Ok(gates.entry(account_id.value()).or_default().clone())
}

fn release_gate(account_id: Id, gate: AccountGate) {
    let Ok(mut gates) = GATES.lock() else {
        return;
    };
    // Two handles remain when nobody else waits: the map's and ours.
    if Arc::strong_count(&gate) <= 2 {
        gates.remove(&account_id.value());
    }
}

/// The account's keys, generating and storing a pair on first need.
///
/// `generate` runs on a blocking thread, at most once per account at a time in this process.
pub async fn ensure_keys<F>(
    store: &dyn FederationStore,
    account_id: Id,
    key_id: &str,
    generate: F,
) -> Result<ActorKeys, FedError>
where
    F: FnOnce() -> Result<Keypair, ProtocolError> + Send + 'static,
{
    if let Some(keys) = stored_private_keys(store, account_id)? {
        return Ok(keys);
    }
    let gate = gate_for(account_id)?;
    let outcome = generate_behind_gate(store, account_id, key_id, generate, &gate).await;
    release_gate(account_id, gate);
    outcome
}

fn stored_private_keys(
    store: &dyn FederationStore,
    account_id: Id,
) -> Result<Option<ActorKeys>, FedError> {
    Ok(store
        .actor_keys(account_id)?
        .filter(|keys| keys.private_key_pem.is_some()))
}

async fn generate_behind_gate<F>(
    store: &dyn FederationStore,
    account_id: Id,
    key_id: &str,
    generate: F,
    gate: &AccountGate,
) -> Result<ActorKeys, FedError>
where
    F: FnOnce() -> Result<Keypair, ProtocolError> + Send + 'static,
{
    let _turn = gate.lock().await;
    // The previous holder of the gate may have stored the pair while this caller waited.
    if let Some(keys) = stored_private_keys(store, account_id)? {
        return Ok(keys);
    }
    let pair = tokio::task::spawn_blocking(generate)
        .await
        .map_err(|e| FedError::Refused(format!("key generation task failed: {e}")))??;
    // Atomic insert-if-absent: returns the winning stored pair.
    Ok(store.store_local_keys(account_id, key_id, &pair.public_key, &pair.private_key)?)
}

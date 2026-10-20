//! Federation use cases (Phase 6).
//!
//! Each inbound activity becomes exactly one store command, and that command
//! commits the activity's dedup record, its state change and its durable
//! follow-up jobs together. The adapters verify signatures and parse the wire
//! shape before calling in. The adapters also build the wire JSON of any
//! outbound activity, so the bytes a retried job sends are fixed here.

use crate::application::federation_ports::{
    DeliverySpec, FederationStore, InboundDisposition, InboundEffect, RemoteAccountUpsert,
};
use crate::application::ports::Store;
use crate::domain::{Follow, Id, Result};

/// An inbound Follow, already signature-verified and parsed by the adapter.
#[derive(Debug, Clone)]
pub struct InboundFollow {
    /// The Follow activity's canonical ID. It is the dedup key and the follow's URI.
    pub activity_uri: String,
    pub actor: RemoteAccountUpsert,
    /// The local account being followed.
    pub target_id: Id,
    /// The Accept activity's ID and wire body, built by the adapter. Sent only if the target accepts at once.
    pub accept_uri: String,
    pub accept_body: serde_json::Value,
}

/// Apply an inbound Follow. An already-seen activity ID changes nothing.
pub fn receive_follow(store: &dyn Store, input: InboundFollow, now_ms: u64) -> Result<InboundDisposition> {
    // A target that blocks the actor (or its domain) takes no follow: the
    // activity is recorded and ignored, and nothing is sent back. The check
    // runs only when the actor is already known; a new actor cannot be blocked yet.
    if let Some(known) = store.account_by_actor_uri(&input.actor.actor_uri)? {
        let pair = crate::application::ports::RelationshipStore::pair_state(store, input.target_id, known.id, now_ms)?;
        if pair.blocking || pair.blocked_by {
            return Ok(InboundDisposition::Ignored("follow between blocked accounts".into()));
        }
    }
    let accept = DeliverySpec {
        sender_id: input.target_id,
        inbox_uri: input.actor.inbox_uri.clone(),
        activity_uri: input.accept_uri,
        body: input.accept_body,
    };
    store.commit_inbound(
        &input.activity_uri,
        input.actor,
        InboundEffect::Follow {
            target_id: input.target_id,
            follow_uri: input.activity_uri.clone(),
            accept,
        },
        now_ms,
    )
}

/// An inbound Accept of one of our outbound follows.
#[derive(Debug, Clone)]
pub struct InboundAccept {
    /// The Accept activity's canonical ID (the dedup key).
    pub activity_uri: String,
    pub actor: RemoteAccountUpsert,
    /// The URI of the Follow activity being accepted (our outbound follow).
    pub follow_uri: String,
}

/// Apply an inbound Accept. The accepting actor must be the account we followed.
pub fn receive_accept(store: &dyn Store, input: InboundAccept, now_ms: u64) -> Result<InboundDisposition> {
    store.commit_inbound(
        &input.activity_uri,
        input.actor,
        InboundEffect::Accept {
            follow_uri: input.follow_uri,
        },
        now_ms,
    )
}

/// An outbound Undo of a follow the local account sent earlier.
#[derive(Debug, Clone)]
pub struct OutboundUndo {
    pub follower_id: Id,
    pub followed_id: Id,
    pub inbox_uri: String,
    /// The Undo activity's ID (the dedup key of nothing here, but the delivery's ID).
    pub undo_uri: String,
    pub body: serde_json::Value,
}

/// Remove the follow and enqueue its Undo, in one transaction. `Ok(None)` when there was no follow.
pub fn undo_remote_follow(store: &dyn FederationStore, input: OutboundUndo, now_ms: u64) -> Result<Option<Follow>> {
    let delivery = DeliverySpec {
        sender_id: input.follower_id,
        inbox_uri: input.inbox_uri,
        activity_uri: input.undo_uri,
        body: input.body,
    };
    store.undo_follow(input.follower_id, input.followed_id, delivery, now_ms)
}

/// An outbound follow the local account is sending to a remote account.
#[derive(Debug, Clone)]
pub struct OutboundFollow {
    pub follower_id: Id,
    pub followed_id: Id,
    /// The Follow activity's ID (also the follow's URI).
    pub follow_uri: String,
    pub inbox_uri: String,
    pub body: serde_json::Value,
}

/// Record an outbound follow request and enqueue its delivery, in one transaction.
pub fn request_remote_follow(store: &dyn FederationStore, input: OutboundFollow, now_ms: u64) -> Result<Follow> {
    let delivery = DeliverySpec {
        sender_id: input.follower_id,
        inbox_uri: input.inbox_uri,
        activity_uri: input.follow_uri.clone(),
        body: input.body,
    };
    store.request_follow(input.follower_id, input.followed_id, &input.follow_uri, delivery, now_ms)
}

/// An inbound Reject of one of our outbound follows.
#[derive(Debug, Clone)]
pub struct InboundReject {
    /// The Reject activity's canonical ID (the dedup key).
    pub activity_uri: String,
    pub actor: RemoteAccountUpsert,
    /// The URI of the Follow activity being rejected (our outbound follow).
    pub follow_uri: String,
}

/// Apply an inbound Reject. The rejecting actor must be the account we followed.
pub fn receive_reject(store: &dyn Store, input: InboundReject, now_ms: u64) -> Result<InboundDisposition> {
    store.commit_inbound(
        &input.activity_uri,
        input.actor,
        InboundEffect::Reject {
            follow_uri: input.follow_uri,
        },
        now_ms,
    )
}

/// An inbound Undo of a follow the remote actor sent to the local `target_id`.
#[derive(Debug, Clone)]
pub struct InboundUndoFollow {
    /// The Undo activity's canonical ID (the dedup key).
    pub activity_uri: String,
    pub actor: RemoteAccountUpsert,
    pub target_id: Id,
}

/// Apply an inbound Undo of a Follow. A follow that is not there is recorded and ignored.
pub fn receive_undo_follow(store: &dyn Store, input: InboundUndoFollow, now_ms: u64) -> Result<InboundDisposition> {
    store.commit_inbound(
        &input.activity_uri,
        input.actor,
        InboundEffect::UndoFollow {
            target_id: input.target_id,
        },
        now_ms,
    )
}

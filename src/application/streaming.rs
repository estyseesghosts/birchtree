//! Committed changes that streaming publishes (Phase 5).
//!
//! Adapters call [`StreamingSink::publish`](crate::application::ports::StreamingSink)
//! after a use case has committed, never inside a storage transaction. Events
//! are notifications, not an event log: a subscriber receives an event only if
//! it is connected when the hub processes the event, and nothing is replayed
//! or persisted for an offline client (`birchtree.md` §7 Phase 5).
//!
//! Events carry ids where the hub can re-read durable state at delivery time.
//! A deletion carries snapshots, because the tombstoned status can no longer
//! answer policy questions.

use crate::domain::{Id, Status};

/// One committed change.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A status was created. Delivered as `update` to the streams whose gate
    /// admits it. The hub re-reads the status, so a status deleted before
    /// delivery is not sent.
    StatusCreated { status_id: Id },
    /// A status was edited. Delivered as `status.update`, and to the mentioned
    /// viewers' notification channels as upstream does.
    StatusEdited { status_id: Id },
    /// A status and the reblog wrappers its deletion cascaded to. Delivered as
    /// `delete` with the id to every stream that would have carried the status.
    StatusDeleted { status: Status, reblogs: Vec<Status> },
    /// A notification row was created. Delivered as `notification` to the
    /// recipient's streams.
    Notification { notification_id: Id },
    /// Live policy removed visibility for these viewers (a block, mute,
    /// unfollow or domain block). Their connections drop every frame queued
    /// before this event is processed.
    PolicyChanged { viewers: Vec<Id> },
    /// A block or mute removes these statuses from `viewer`'s open home and
    /// list streams (upstream's `clear_from_home` and `clear_from_lists`).
    /// Publish after the matching [`StreamEvent::PolicyChanged`].
    Retract { viewer: Id, statuses: Vec<Status> },
    /// An access token was revoked. Its connections close (upstream `kill`).
    TokenRevoked { token_id: Id },
}

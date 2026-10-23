//! The activities birchtree accepts (inbound) and sends (outbound) in Phase 6.
//!
//! Each type implements the crate's `ActivityHandler`. `verify` checks the
//! activity's own shape (its actor against the object it names); `receive`
//! applies the effect through `application::federation_inbound` or
//! `application::federation`, then records the activity URI if it applied (D33).
//!
//! The inbox parses the body as [`Activity`]. A type outside this set is refused
//! at parse time (422, D32), so nothing is accepted and dropped.

pub mod envelope;
pub mod follow;
pub mod inputs;
pub mod move_;
pub mod social;
pub mod statuses;

pub use envelope::Activity;
pub use follow::{Accept, AcceptObject, Follow, Reject, Undo, UndoObject};
pub use inputs::{
    RemoteVote, audience_visibility, note_input, note_input_for, question_input,
    question_input_for, vote_of,
};
pub use move_::{
    MIGRATE_FOLLOWERS_CAP, Move, MoveOutcome, MoveType, apply_move, apply_move_counted,
};
pub use social::{Announce, Block, Flag, Like, QuoteRequestActivity};
pub use statuses::{Create, CreateObject, Delete, Update};

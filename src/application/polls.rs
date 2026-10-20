//! Poll use cases: reads and voting.

use crate::application::ports::{PairState, Store, VoteRejection};
use crate::application::policy;
use crate::domain::{Error, Id, InteractionEvent, InteractionEventKind, Poll, Result};

/// Why a vote was refused, in the shape the REST layer maps onto Mastodon
/// responses: `NotFound` -> 404, `NotPermitted` -> 403 "This action is not
/// allowed", `Rejected` -> 422 "Validation failed: …".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollVoteError {
    /// The poll (or the status it hangs off) is missing or not visible.
    NotFound,
    /// Blocks in either direction between the voter and the poll author.
    NotPermitted,
    /// Store-side validation refusal (expired poll, unknown choice, self
    /// vote, duplicate vote).
    Rejected(VoteRejection),
}

/// Map a persistence error onto the vote error surface. The vote API has no
/// internal-error variant, so authorization-shaped failures become
/// `NotPermitted` and everything else is reported as `NotFound` rather than
/// fabricated as a rejection.
fn store_error(err: Error) -> PollVoteError {
    match err {
        Error::Forbidden(_) | Error::Unauthorized(_) => PollVoteError::NotPermitted,
        _ => PollVoteError::NotFound,
    }
}

/// `GET /api/v1/polls/:id` — the poll for this viewer.
///
/// `Err(NotFound("poll"))` when the poll is missing, its status is missing or
/// tombstoned, or the status is not visible to the viewer.
pub fn get_poll(
    store: &dyn Store,
    viewer_id: Option<Id>,
    poll_id: Id,
    now_ms: u64,
) -> Result<Poll> {
    let poll = store
        .poll_by_id(poll_id)?
        .ok_or_else(|| Error::NotFound("poll".into()))?;
    let status = store
        .status_by_id(poll.status_id)?
        .ok_or_else(|| Error::NotFound("poll".into()))?;
    let pair = match viewer_id {
        Some(viewer) => store.pair_state(status.account_id, viewer, now_ms)?,
        None => PairState::default(),
    };
    if !policy::can_fetch(viewer_id, &status, pair) {
        return Err(Error::NotFound("poll".into()));
    }
    Ok(poll)
}

/// `POST /api/v1/polls/:id/votes` — cast (or replace, on multiple-choice
/// polls) the viewer's ballot.
///
/// Gates, in order: poll exists -> its status is showable -> no block in
/// either direction -> at least one choice -> the store's in-transaction
/// validation (expiry/choice/self/duplicate), mapped through unchanged.
pub fn vote(
    store: &dyn Store,
    viewer_id: Id,
    poll_id: Id,
    choices: &[u64],
    now_ms: u64,
) -> std::result::Result<Poll, PollVoteError> {
    let poll = store
        .poll_by_id(poll_id)
        .map_err(store_error)?
        .ok_or(PollVoteError::NotFound)?;
    let status = store
        .status_by_id(poll.status_id)
        .map_err(store_error)?
        .ok_or(PollVoteError::NotFound)?;
    // The show gate: a poll whose status the viewer may not fetch is a 404.
    let status_pair = store
        .pair_state(status.account_id, viewer_id, now_ms)
        .map_err(store_error)?;
    if !policy::can_fetch(Some(viewer_id), &status, status_pair) {
        return Err(PollVoteError::NotFound);
    }
    // Blocks either direction between the poll author and the voter -> 403.
    let author_pair = store
        .pair_state(poll.account_id, viewer_id, now_ms)
        .map_err(store_error)?;
    if author_pair.blocking || author_pair.blocked_by {
        return Err(PollVoteError::NotPermitted);
    }
    if choices.is_empty() {
        return Err(PollVoteError::Rejected(VoteRejection::InvalidChoice));
    }
    let poll = store
        .vote_poll(poll_id, viewer_id, choices, now_ms)
        .map_err(PollVoteError::Rejected)?;
    // D39 `interactions` journal: upstream increments on every vote call that
    // reaches the tracker (`VoteService#call`, `app/services/vote_service.rb:33`;
    // duplicate/expiry/self rejections never reach it because the store errs
    // first — same shape here: only `Ok` journals). Federation inbound tallies
    // remote votes via `sync_remote_poll`, never this path, so remote votes
    // stay out. Journal failures never fail the vote (fire-and-forget, like
    // `record_sign_in`).
    let _ = store.record_interaction(InteractionEvent::for_journal(
        viewer_id,
        InteractionEventKind::Vote,
        now_ms,
    ));
    Ok(poll)
}

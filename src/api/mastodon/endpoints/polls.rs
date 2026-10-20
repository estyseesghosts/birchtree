//! Poll endpoints: read a poll and cast a vote.
//!
//! Both answer `REST::PollSerializer`; the read is optional-auth (the show
//! gate of the poll's status applies), the vote needs a user token with
//! `write`/`write:statuses`.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::{Auth, OptionalAuth};
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::polls::{self, PollVoteError};
use crate::application::ports::Store;
use crate::application::ports::VoteRejection;
use crate::domain::Id;

/// Read scopes for poll reads.
const READ_SCOPES: &[&str] = &["read", "read:statuses"];
/// Write scopes for votes.
const WRITE_SCOPES: &[&str] = &["write", "write:statuses"];

fn optional_scopes(auth: &OptionalAuth, required: &[&str]) -> Result<(), ApiError> {
    match &auth.0 {
        Some(authenticated) => authenticated.require_scopes(required),
        None => Ok(()),
    }
}

/// Map a vote rejection to Mastodon's 422 bodies.
fn vote_error(rejection: VoteRejection) -> Response {
    let message = match rejection {
        VoteRejection::Expired => "Validation failed: The poll has already ended",
        VoteRejection::InvalidChoice => "Validation failed: The chosen vote option does not exist",
        VoteRejection::SelfVote => "Validation failed: You cannot vote in your own polls",
        VoteRejection::AlreadyVoted => "Validation failed: You have already voted on this poll",
    };
    ApiError::unprocessable(message).into_response()
}

/// `GET /api/v1/polls/{id}`.
pub async fn show(
    State(state): State<ApiState>,
    auth: OptionalAuth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = optional_scopes(&auth, READ_SCOPES) {
        return err.into_response();
    }
    let viewer = auth.0.as_ref().and_then(|a| a.token.user_id);
    let Ok(poll_id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match polls::get_poll(store, viewer, poll_id, state.context.now_ms()) {
        Ok(poll) => {
            // The viewer's own ballot for `voted`/`own_votes`.
            let ballot = match viewer {
                Some(viewer) => store.ballot(poll.id, viewer).ok().flatten(),
                None => None,
            };
            axum::Json(dto::poll_json_with_ballot(
                &poll,
                viewer,
                ballot.as_deref(),
                state.context.now_ms(),
            ))
            .into_response()
        }
        // Missing or invisible polls answer Mastodon's explicit not_found.
        Err(crate::domain::Error::NotFound(_)) => ApiError::not_found().into_response(),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// `POST /api/v1/polls/{id}/votes` — `choices[]` required.
pub async fn vote(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(poll_id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let pairs = body;
    // `params.require(:choices)` -> 400 when the key is missing entirely.
    // A present-but-empty list (`choices[]=`) is upstream's silent no-op
    // (`VoteService` returns before the vote gate): 200 with the unchanged
    // poll. A present list with no parseable choice 422s like upstream's
    // `VoteValidator#invalid_choice?` (`Integer(choice)` fails there; a
    // bare empty string from form encoding is ignored as a form artifact).
    if !forms::has(&pairs, "choices") {
        return ApiError::bad_request("param is missing or the value is empty: choices")
            .into_response();
    }
    let raw = forms::all(&pairs, "choices");
    if raw.iter().any(|item| !item.is_empty() && item.parse::<u64>().is_err()) {
        return ApiError::unprocessable("Validation failed: The chosen vote option does not exist")
            .into_response();
    }
    let choices: Vec<u64> = raw.iter().filter_map(|raw| raw.parse::<u64>().ok()).collect();
    let store: &dyn Store = &*state.context.store;
    match polls::vote(store, viewer, poll_id, &choices, state.context.now_ms()) {
        Ok(poll) => {
            let ballot = store.ballot(poll.id, viewer).ok().flatten();
            axum::Json(dto::poll_json_with_ballot(
                &poll,
                Some(viewer),
                ballot.as_deref(),
                state.context.now_ms(),
            ))
            .into_response()
        }
        Err(PollVoteError::NotFound) => ApiError::not_found().into_response(),
        Err(PollVoteError::NotPermitted) => {
            ApiError::forbidden("This action is not allowed").into_response()
        }
        Err(PollVoteError::Rejected(rejection)) => vote_error(rejection),
    }
}

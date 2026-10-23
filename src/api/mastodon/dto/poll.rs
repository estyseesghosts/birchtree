//! Mastodon poll DTO packers (S10 split of `api::mastodon::dto`).

use serde_json::{Value, json};

use crate::application::ports::filter_json::rfc3339_ms;
use crate::domain::{Id, Poll};

/// `REST::PollSerializer` without viewer ballot context.
/// Poll-level counts are serialized unmasked even under `hide_totals`; only
/// per-option `votes_count` is nulled pre-expiry.
pub fn poll_json(poll: &Poll, viewer: Option<Id>, now_ms: u64) -> Value {
    poll_json_with_ballot(poll, viewer, None, now_ms)
}

/// `REST::PollSerializer` with the viewer's ballot applied
/// (`voted`/`own_votes`; the poll author is considered a voter without a
/// ballot, matching Mastodon).
pub fn poll_json_with_ballot(
    poll: &Poll,
    viewer: Option<Id>,
    ballot: Option<&[u64]>,
    now_ms: u64,
) -> Value {
    let expired = poll.is_expired_at(now_ms);
    let options: Vec<Value> = poll
        .options
        .iter()
        .map(|option| {
            json!({
                "title": option.title,
                "votes_count": if poll.show_totals_at(now_ms) {
                    json!(option.votes_count)
                } else {
                    Value::Null
                },
            })
        })
        .collect();
    let is_author = viewer.is_some_and(|v| v == poll.account_id);
    let own: Vec<u64> = ballot.map(<[u64]>::to_vec).unwrap_or_default();
    let voted = is_author || !own.is_empty();
    let own_votes = if own.is_empty() && !is_author {
        Value::Null
    } else {
        json!(own)
    };
    json!({
        "id": poll.id.to_string(),
        "expires_at": poll
            .expires_at_ms
            .map(rfc3339_ms)
            .map(Value::String)
            .unwrap_or(Value::Null),
        "expired": expired,
        "multiple": poll.multiple,
        "votes_count": poll.votes_count,
        "voters_count": poll.voters_count,
        "options": options,
        "emojis": [],
        "voted": voted,
        "own_votes": own_votes,
    })
}

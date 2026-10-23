use activitypub_federation::protocol::verification::verify_domains_match;
use url::Url;

use crate::activitypub::error::FedError;
use crate::activitypub::wire::{Note, PUBLIC, Question};
use crate::application::federation_inbound::{self as inbound, Applied, RemoteNote};
use crate::application::federation_ports::InboundDisposition;
use crate::domain::{Id, Visibility};

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// The visibility of an activity or Note from its audience. `followers_uri`
/// is the author's own followers collection URI (see
/// [`crate::activitypub::wire::followers_uri_of`]): only an exact match
/// yields followers-only (upstream `StatusParser#visibility` compares
/// against the author's `followers_collection`). A bare
/// `ends_with("/followers")` would let any host's `…/followers` URI pass.
pub fn audience_visibility(to: &[Url], cc: &[Url], followers_uri: &str) -> Visibility {
    let public = |list: &[Url]| list.iter().any(|u| u.as_str() == PUBLIC);
    if public(to) {
        Visibility::Public
    } else if public(cc) {
        Visibility::Unlisted
    } else if to.iter().any(|u| u.as_str() == followers_uri) {
        Visibility::Private
    } else {
        Visibility::Direct
    }
}

/// Log an applied, duplicate or ignored activity.
pub(super) fn log_outcome(kind: &str, id: &Url, actor: &Url, applied: &Applied) {
    let outcome = match applied {
        Applied::Applied => "applied".to_string(),
        Applied::Duplicate => "duplicate (already applied)".to_string(),
        Applied::Ignored(reason) => format!("ignored: {reason}"),
    };
    tracing::info!(
        target: "birchtree::federation",
        direction = "inbound",
        kind,
        activity = %id,
        actor = %actor,
        outcome = %outcome,
        "activity handled"
    );
}

pub(super) fn log_disposition(kind: &str, id: &Url, actor: &Url, disposition: &InboundDisposition) {
    let applied = match disposition {
        InboundDisposition::Applied => Applied::Applied,
        InboundDisposition::Duplicate => Applied::Duplicate,
        InboundDisposition::Ignored(reason) => Applied::Ignored(reason.clone()),
    };
    log_outcome(kind, id, actor, &applied);
}

/// Record an applied activity, so a redelivery is a duplicate (never an ignored one).
pub(super) fn record(
    store: &dyn crate::application::ports::Store,
    id: &Url,
    applied: &Applied,
    now_ms: u64,
) -> Result<(), FedError> {
    if *applied == Applied::Applied || *applied == Applied::Duplicate {
        store.record_activity(id.as_str(), now_ms)?;
    }
    Ok(())
}

/// Turn a verified Note into the application's input, for the given author.
///
/// `author_sensitized` is the author's account flag: upstream forces
/// `sensitive` when the account is sensitized
/// (`Activity::Create#sensitive: @account.sensitized? || @status_parser.sensitive`).
pub fn note_input_for(note: &Note, author_id: Id, author_sensitized: bool) -> RemoteNote {
    let published_ms = note
        .published
        .as_deref()
        .and_then(|p| chrono::DateTime::parse_from_rfc3339(p).ok())
        .map(|d| d.timestamp_millis().max(0) as u64)
        .unwrap_or(0);
    RemoteNote {
        uri: note.id.to_string(),
        url: note.url.as_ref().unwrap_or(&note.id).to_string(),
        author_id,
        published_ms,
        visibility: note.visibility(),
        content_html: note.content.clone(),
        spoiler_text: note.summary.clone().filter(|s| !s.is_empty()),
        sensitive: author_sensitized || note.sensitive.unwrap_or(false),
        language: note.language(),
        in_reply_to_uri: note.in_reply_to.as_ref().map(|u| u.to_string()),
        mention_uris: note.mention_uris(),
        tags: note.hashtags(),
        location: note.location.as_ref().and_then(|wire| {
            // Core extension CORE-P: validate value-by-value; invalid
            // locations are dropped, never fatal.
            let place = crate::domain::Place {
                // Temporary id: the use case mints the stored row.
                id: crate::domain::Id::new(0),
                name: wire.name.clone(),
                lon: wire.longitude,
                lat: wire.latitude,
                country: wire.country.clone(),
            };
            place.validate().ok()?;
            Some(inbound::RemotePlace {
                name: place.name,
                lon: place.lon,
                lat: place.lat,
                country: place.country,
            })
        }),
        attachments: note
            .attachment
            .iter()
            .filter_map(|a| {
                Some(inbound::RemoteAttachment {
                    source_url: a.url.as_ref()?.to_string(),
                    media_type: a.media_type.clone().unwrap_or_default(),
                    description: a.name.clone().unwrap_or_default(),
                    // Core extension CORE-L: valid ids round-trip; anything
                    // else is untrusted input and lands as NULL.
                    license: a.license.and_then(|raw| {
                        (raw >= 1 && raw <= u8::MAX as i64)
                            .then(|| raw as u8)
                            .filter(|id| crate::domain::license::is_valid_license(*id))
                    }),
                })
            })
            .collect(),
        // Remote emoji refs from the Note's `Emoji` tags (federation tail,
        // G-D): fetched through `FetchRemoteEmoji` at ingest.
        emojis: note
            .emoji_refs()
            .into_iter()
            .map(|e| inbound::RemoteEmojiRef {
                shortcode: e.shortcode,
                url: e.url,
            })
            .collect(),
        quote_uri: note.quote_uri().map(|u| u.to_string()),
        poll: None,
    }
}

/// Turn a verified Note into the application's input, for the given author.
///
/// Kept for callers without the author's account row (existing tests):
/// production ingest uses [`note_input_for`], which also forces `sensitive`
/// for sensitized authors.
pub fn note_input(note: &Note, author_id: Id) -> RemoteNote {
    note_input_for(note, author_id, false)
}

/// A received vote: the chosen option's title and the poll's URI.
#[derive(Debug, Clone)]
pub struct RemoteVote {
    pub poll_uri: String,
    pub choice_name: String,
}

/// A vote-shaped Note (`name` set, empty content, `inReplyTo` set) carries a
/// poll vote, not a status. Every other Note is ingested as a status.
pub fn vote_of(note: &Note) -> Option<RemoteVote> {
    let choice_name = note.name.clone().filter(|n| !n.is_empty())?;
    if !note.content.is_empty() {
        return None;
    }
    let poll_uri = note.in_reply_to.as_ref()?.to_string();
    Some(RemoteVote {
        poll_uri,
        choice_name,
    })
}

/// Turn a verified Question into the application's input, for the given
/// author: the status fields plus the poll. The tallies ride along so the
/// ingest can seed them atomically with the status.
///
/// `author_sensitized` forces `sensitive`, like [`note_input_for`] (upstream
/// `Activity::Create` forces it for every status shape of a sensitized
/// account).
pub fn question_input_for(
    question: &Question,
    author_id: Id,
    author_sensitized: bool,
    now_ms: u64,
) -> (RemoteNote, inbound::RemotePoll) {
    let published_ms = question
        .published
        .as_deref()
        .and_then(|p| chrono::DateTime::parse_from_rfc3339(p).ok())
        .map(|d| d.timestamp_millis().max(0) as u64)
        .unwrap_or(0);
    // A `closed` time with no `endTime` means the poll already ended: fall
    // back to its publish time so the row reads as expired.
    let expires_at_ms = question
        .expiry_ms()
        .or_else(|| question.closed_flag(now_ms).then_some(published_ms));
    let options: Vec<(String, u64)> = question
        .options()
        .iter()
        .map(|o| {
            (
                o.name.clone(),
                o.replies.as_ref().map(|r| r.total_items).unwrap_or(0),
            )
        })
        .collect();
    let votes_count = options.iter().map(|(_, v)| v).sum();
    let note = RemoteNote {
        uri: question.id.to_string(),
        url: question.url.as_ref().unwrap_or(&question.id).to_string(),
        author_id,
        published_ms,
        visibility: audience_visibility(
            &question.to,
            &question.cc,
            &crate::activitypub::wire::followers_uri_of(&question.attributed_to),
        ),
        content_html: question.content.clone(),
        spoiler_text: None,
        // Upstream also honors the Question's own `sensitive` flag here;
        // the wire shape carries no such field (adding one breaks the
        // shared struct literals), so only the account flag forces it.
        sensitive: author_sensitized,
        language: None,
        in_reply_to_uri: question.in_reply_to.as_ref().map(|u| u.to_string()),
        mention_uris: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
        location: None,
        // Questions carry no emoji refs on this path (the `Emoji`-tag tail
        // arm reads Notes; a Question's tags stay unparsed).
        emojis: Vec::new(),
        quote_uri: None,
        poll: None,
    };
    let poll = inbound::RemotePoll {
        multiple: question.multiple(),
        options,
        voters_count: question.voters_count.unwrap_or(0),
        votes_count,
        expires_at_ms,
        expired: question.closed_flag(now_ms),
    };
    (note, poll)
}

/// Turn a verified Question into the application's input, for the given
/// author. Kept for callers without the author's account row (existing
/// tests): production ingest uses [`question_input_for`].
pub fn question_input(
    question: &Question,
    author_id: Id,
    now_ms: u64,
) -> (RemoteNote, inbound::RemotePoll) {
    question_input_for(question, author_id, false, now_ms)
}

/// An object's author must be the actor that sent it, and its ID must be on the actor's host.
pub(super) fn verify_object_author(
    id: &Url,
    attributed_to: &Url,
    actor: &Url,
) -> Result<(), FedError> {
    if attributed_to != actor {
        return Err(FedError::Refused(
            "object is attributed to another actor".into(),
        ));
    }
    verify_domains_match(id, actor)?;
    Ok(())
}

/// The Note's author must be the actor that sent it, and its ID must be on the actor's host.
pub(super) fn verify_note_author(note: &Note, actor: &Url) -> Result<(), FedError> {
    verify_object_author(&note.id, &note.attributed_to, actor)
}

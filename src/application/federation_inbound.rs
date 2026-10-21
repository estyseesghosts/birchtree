//! Inbound federation use cases (Phase 6): the local effect of each received
//! activity other than Follow (which `application::federation` owns).
//!
//! The effects are the local operations (`create_status`, `favourite_status`,
//! `reblog_status`, `block`, ...) called with the remote account as the actor,
//! so remote and local interactions share one implementation. Each effect is
//! idempotent, so a redelivered activity changes nothing. The activity URI is
//! recorded only after the effect applied (D33): an ignored activity is not.

use crate::application::federation_ports::{NewQuote, NewReport};
use crate::application::policy::{quote_permission, QuotePermission};
use crate::application::ports::{BlobStore, NewMedia, NewPollSpec, NewStatus, RemoteStatusIdentity, Store, VoteRejection};use crate::domain::{CustomEmoji, Error, Id, JobKind, QuoteState, Result, Status, Visibility};

/// What an inbound activity did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The effect applied.
    Applied,
    /// The effect was already in place.
    Duplicate,
    /// No effect, with the reason (logged by the caller).
    Ignored(String),
}

/// One attachment of a received Note: fetched later from `source_url`.
#[derive(Debug, Clone)]
pub struct RemoteAttachment {
    pub source_url: String,
    /// The ActivityStreams media type (`image/png`, `video/mp4`, ...).
    pub media_type: String,
    pub description: String,
    /// Core extension CORE-L: sender's license id, when present and valid.
    /// Out-of-enum values are dropped (stored NULL) at ingest; federation
    /// input is untrusted, so ingest never 422s on licenses.
    pub license: Option<u8>,
}

/// A wired place attached to a received Note (core extension CORE-P).
#[derive(Debug, Clone)]
pub struct RemotePlace {
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub country: Option<String>,
}

/// A received Note, already verified and parsed by the adapter.
#[derive(Debug, Clone)]
pub struct RemoteNote {
    pub uri: String,
    pub url: String,
    pub author_id: Id,
    pub published_ms: u64,
    pub visibility: Visibility,
    pub content_html: String,
    pub spoiler_text: Option<String>,
    pub sensitive: bool,
    pub language: Option<String>,
    pub in_reply_to_uri: Option<String>,
    /// Core extension CORE-P: the Note's `location`, when present and valid.
    /// Invalid values are dropped (stored NULL); ingest never 422s on places.
    pub location: Option<RemotePlace>,
    /// The `actor` URIs of the Note's mentions.
    pub mention_uris: Vec<String>,
    pub tags: Vec<String>,
    pub attachments: Vec<RemoteAttachment>,
    /// Remote custom emoji from the Note's `Emoji` tags (Phase 6 tail).
    /// Bytes are fetched post-commit by the `FetchRemoteEmoji` job; the
    /// ingest only enqueues the jobs.
    pub emojis: Vec<RemoteEmoji>,
    /// The quoted status URI (`quote`), if any.
    pub quote_uri: Option<String>,
    /// A poll to attach, when the received object is a `Question`.
    pub poll: Option<RemotePoll>,
}

/// A received `Question`'s poll: option titles with their tallies, the
/// expiry, and whether the origin already closed it.
#[derive(Debug, Clone)]
pub struct RemotePoll {
    pub multiple: bool,
    /// `(title, votes)` per option, in the origin's order.
    pub options: Vec<(String, u64)>,
    pub voters_count: u64,
    pub votes_count: u64,
    /// Absolute expiry in ms; `None` when the origin sent no usable time.
    pub expires_at_ms: Option<u64>,
    pub expired: bool,
}

impl RemotePoll {
    /// Titles in the origin's order (for shape comparison on update).
    fn titles(&self) -> Vec<String> {
        self.options.iter().map(|(t, _)| t.clone()).collect()
    }

    /// A poll needs 2–10 named options and a known expiry (Misskey allows 10,
    /// Mastodon truncates at 500; birchtree posts at most 4). Anything else is
    /// ingested as a poll-less status, never rejected.
    fn usable(&self) -> bool {
        self.expires_at_ms.is_some()
            && (2..=10).contains(&self.options.len())
            && self.options.iter().all(|(t, _)| !t.is_empty())
    }
}

/// The outcome of ingesting a Note.
#[derive(Debug, Clone)]
pub struct NoteIngest {
    pub status: Status,
    pub created: bool,
    /// The Note names a parent that is not known here: thread completion must fetch it.
    pub parent_unknown: Option<String>,
}

/// Store a received Note (or Question) as a status of its remote author.
/// Idempotent by URI. A usable attached poll is stored with the status and
/// Aperture gate (Stage 2 AP-E1): does this received Note drop under
/// aperture mode? DMs and anything with an `inReplyTo` (replies park and
/// resolve through the existing thread-completion path) pass; top-level
/// non-DM Notes need at least one image/video attachment. Polls never
/// satisfy the gate (a poll-only top-level drops — documented deviation);
/// audio does not satisfy it (photo-first).
pub fn aperture_drops_note(note: &RemoteNote) -> bool {
    if note.visibility == Visibility::Direct {
        return false;
    }
    if note.in_reply_to_uri.is_some() {
        return false;
    }
    if note
        .attachments
        .iter()
        .any(|a| matches!(media_kind(&a.media_type), "image" | "video"))
    {
        return false;
    }
    true
}

/// seeded with the origin's tallies; an unusable one is dropped (poll-less
/// status), never a refusal.
pub fn ingest_note(store: &dyn Store, note: RemoteNote, poll: Option<RemotePoll>, now_ms: u64) -> Result<NoteIngest> {
    if let Some(existing) = store.status_by_uri(&note.uri)? {
        return Ok(NoteIngest {
            status: existing,
            created: false,
            parent_unknown: None,
        });
    }
    let usable_poll = poll.filter(|p| p.usable());
    let parent = match &note.in_reply_to_uri {
        Some(uri) => store.status_by_uri(uri)?.filter(|p| !p.is_deleted()),
        None => None,
    };
    let parent_unknown = match (&note.in_reply_to_uri, &parent) {
        (Some(uri), None) => Some(uri.clone()),
        _ => None,
    };
    let mut mentions = Vec::new();
    for uri in &note.mention_uris {
        if let Some(account) = store.account_by_actor_uri(uri)? {
            if !mentions.contains(&account.id) {
                mentions.push(account.id);
            }
        }
    }
    // Attachments are rows now and bytes later: the fetch job stages them.
    let mut media_ids = Vec::new();
    let mut pending_fetches = Vec::new();
    for attachment in &note.attachments {
        let media = store.create_media(NewMedia {
            account_id: note.author_id,
            r#type: media_kind(&attachment.media_type).into(),
            description: attachment.description.clone(),
            // CORE-L: validated at the adapter boundary; `None` when the
            // sender omitted it or sent garbage.
            license: attachment.license,
        })?;
        store.set_remote_media_source(media.id, &attachment.source_url)?;
        media_ids.push(media.id);
        pending_fetches.push(media.id);
    }
    // Core extension CORE-P: a valid remote `location` mints a local place
    // row (sender ids are opaque, never reused). A same-name local row wins
    // (dedupe by name) so remote input can neither hijack nor duplicate
    // local names.
    let place_id = match &note.location {
        None => None,
        Some(remote) => {
            let candidate = crate::domain::Place {
                // Temporary id: `link_remote_place` mints the stored row.
                id: crate::domain::Id::new(0),
                name: remote.name.clone(),
                lon: remote.lon,
                lat: remote.lat,
                country: remote.country.clone(),
            };
            Some(store.link_remote_place(&candidate)?.id)
        }
    };
    let status = store.create_status(
        NewStatus {
            remote: Some(RemoteStatusIdentity {
                uri: note.uri.clone(),
                url: note.url.clone(),
            }),
            account_id: note.author_id,
            source_text: note.content_html.clone(),
            content_html: note.content_html.clone(),
            spoiler_text: note.spoiler_text.clone(),
            sensitive: note.sensitive,
            visibility: note.visibility,
            language: note.language.clone(),
            in_reply_to_id: parent.as_ref().map(|p| p.id),
            in_reply_to_account_id: parent.as_ref().map(|p| p.account_id),
            media_ids,
            place_id,
            mentions,
            // D25: inbound tags land normalized (same rule as local
            // writes), so remote `#café` and local `#cafe` are one tag.
            tags: crate::application::statuses::normalize_tag_list(&note.tags),
            poll: usable_poll.as_ref().map(|p| NewPollSpec {
                multiple: p.multiple,
                hide_totals: false,
                expires_at_ms: p.expires_at_ms.unwrap_or(now_ms),
                options: p.titles(),
            }),
            ..NewStatus::default()
        },
        note.published_ms,
    )?;
    if let Some(remote) = &usable_poll {
        // Seed the origin's tallies (insert starts them at zero).
        store.sync_remote_poll(
            status.status.poll_id.ok_or_else(|| Error::NotFound("poll of ingested status".into()))?,
            &remote.options,
            remote.voters_count,
            remote.expires_at_ms,
            remote.expired,
            now_ms,
        )?;
    }
    for media_id in pending_fetches {
        store.enqueue(
            JobKind::FetchRemoteMedia,
            serde_json::json!({ "media_id": media_id.value() }),
            now_ms,
        )?;
    }
    // Remote emoji bytes arrive post-commit: one idempotent job per tag
    // (bounded at parse; refusals never fail the Note).
    for emoji in note
        .emojis
        .iter()
        .take(crate::application::federation_inbound::MAX_REMOTE_EMOJI_PER_NOTE)
    {
        store.enqueue(
            JobKind::FetchRemoteEmoji,
            serde_json::json!({
                "shortcode": emoji.shortcode,
                "domain": emoji.domain,
                "url": emoji.url,
            }),
            now_ms,
        )?;
    }
    // A quote of a local status is pending until the QuoteRequest is decided.
    if let Some(quoted_uri) = &note.quote_uri {
        if let Some(quoted) = store.status_by_uri(quoted_uri)? {
            if store.quote_by_quoting(status.status.id)?.is_none() && quoted.account_id != note.author_id {
                let local = store
                    .account_by_id(quoted.account_id)?
                    .is_some_and(|a| a.local);
                if local {
                    store.create_quote(
                        NewQuote {
                            quoting_status_id: status.status.id,
                            quoted_status_id: quoted.id,
                            quoting_account_id: note.author_id,
                            quoted_account_id: quoted.account_id,
                            state: QuoteState::Pending,
                            approval_uri: None,
                            quote_activity_uri: None,
                        },
                        now_ms,
                    )?;
                }
            }
        }
    }
    if let Some(uri) = &parent_unknown {
        store.set_unresolved_parent(status.status.id, Some(uri))?;
    }
    Ok(NoteIngest {
        status: status.status,
        created: true,
        parent_unknown,
    })
}

/// The media kind for a `mediaType` (the same vocabulary as local uploads).
pub fn media_kind(media_type: &str) -> &'static str {
    match media_type.split('/').next().unwrap_or_default() {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        _ => "unknown",
    }
}

/// Apply an edit of a remote status by its author.
/// A remote account is deleted: its statuses are deleted with it. The account row
/// stays, so its URI still resolves (as a tombstone for the remote's own references).
pub fn delete_account(store: &dyn Store, account_id: Id, now_ms: u64) -> Result<Applied> {
    let ids = store.account_status_ids(account_id)?;
    if ids.is_empty() {
        return Ok(Applied::Duplicate);
    }
    for id in ids {
        store.delete_status(id, account_id, now_ms)?;
    }
    Ok(Applied::Applied)
}

pub fn update_note(store: &dyn Store, actor_id: Id, note: &RemoteNote, now_ms: u64) -> Result<Applied> {
    let Some(status) = store.status_by_uri(&note.uri)? else {
        return Ok(Applied::Ignored("update of an unknown status".into()));
    };
    if status.account_id != actor_id {
        return Ok(Applied::Ignored("update by someone other than the author".into()));
    }
    store.update_status(
        status.id,
        actor_id,
        crate::application::ports::StatusUpdate {
            source_text: Some(note.content_html.clone()),
            content_html: Some(note.content_html.clone()),
            spoiler_text: Some(note.spoiler_text.clone()),
            sensitive: Some(note.sensitive),
            language: note.language.clone(),
            // Remote tags ride the wire `Hashtag` array (already
            // normalized at parse); re-derive the stored list from them so
            // a tag edit moves the hashtag-index keys (bug 4).
            tags: Some(crate::application::statuses::normalize_tag_list(&note.tags)),
            ..Default::default()
        },
        now_ms,
    )?;
    Ok(Applied::Applied)
}

/// Delete a remote status by its author. A status already gone is a duplicate.
pub fn delete_note(store: &dyn Store, actor_id: Id, uri: &str, now_ms: u64) -> Result<Applied> {
    let Some(status) = store.status_by_uri(uri)? else {
        return Ok(Applied::Duplicate);
    };
    if status.account_id != actor_id {
        return Ok(Applied::Ignored("delete by someone other than the author".into()));
    }
    if status.is_deleted() {
        return Ok(Applied::Duplicate);
    }
    store.delete_status(status.id, actor_id, now_ms)?;
    Ok(Applied::Applied)
}

/// A remote account favourites a status. Ignored if the status is unknown or not visible to it.
pub fn like(store: &dyn Store, actor_id: Id, status_uri: &str, now_ms: u64) -> Result<Applied> {
    let Some(status) = store.status_by_uri(status_uri)? else {
        return Ok(Applied::Ignored("like of an unknown status".into()));
    };
    match store.favourite_status(actor_id, status.id, now_ms) {
        Ok(_) => Ok(Applied::Applied),
        Err(Error::NotFound(_)) => Ok(Applied::Ignored("like of a status the liker cannot see".into())),
        Err(err) => Err(err),
    }
}

/// A remote account removes its favourite.
pub fn unlike(store: &dyn Store, actor_id: Id, status_uri: &str, now_ms: u64) -> Result<Applied> {
    let Some(status) = store.status_by_uri(status_uri)? else {
        return Ok(Applied::Duplicate);
    };
    store.unfavourite_status(actor_id, status.id, now_ms)?;
    Ok(Applied::Applied)
}

/// A remote account boosts a status. The boost's visibility comes from the Announce's audience.
pub fn announce(
    store: &dyn Store,
    actor_id: Id,
    status_uri: &str,
    visibility: Visibility,
    now_ms: u64,
) -> Result<Applied> {
    let Some(status) = store.status_by_uri(status_uri)? else {
        return Ok(Applied::Ignored("announce of an unknown status".into()));
    };
    match store.reblog_status(actor_id, status.id, visibility, now_ms) {
        Ok(_) => Ok(Applied::Applied),
        Err(Error::NotFound(_)) => Ok(Applied::Ignored("announce of a status the announcer cannot see".into())),
        Err(err) => Err(err),
    }
}

/// A remote account removes its boost.
pub fn unannounce(store: &dyn Store, actor_id: Id, status_uri: &str, now_ms: u64) -> Result<Applied> {
    let Some(status) = store.status_by_uri(status_uri)? else {
        return Ok(Applied::Duplicate);
    };
    store.unreblog_status(actor_id, status.id, now_ms)?;
    Ok(Applied::Applied)
}

/// What a vote-shaped `Create` turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoteDisposition {
    /// The vote was tallied (or ignored with a logged reason).
    Tallied(Applied),
    /// Not a vote after all: the parent is unknown, has no poll, or has no
    /// such option. The caller ingests the Note as an ordinary status.
    NotAVote,
}

/// A remote account votes on a poll: tally the named choice.
///
/// Only votes on known local polls tally here (Mastodon requires the poll to
/// be local). Anything else is `NotAVote`, and the Note keeps its ordinary
/// meaning. Gates mirror the local vote path: unknown choice, expiry,
/// duplicate and blocks drop the vote without a refusal.
pub fn vote(
    store: &dyn Store,
    voter_id: Id,
    poll_uri: &str,
    choice_name: &str,
    now_ms: u64,
) -> Result<VoteDisposition> {
    let Some(status) = store.status_by_uri(poll_uri)? else {
        return Ok(VoteDisposition::NotAVote);
    };
    let Some(poll) = store.poll_by_status(status.id)? else {
        return Ok(VoteDisposition::NotAVote);
    };
    let owner = store
        .account_by_id(poll.account_id)?
        .ok_or_else(|| Error::NotFound("poll author".into()))?;
    if !owner.local {
        return Ok(VoteDisposition::Tallied(Applied::Ignored(
            "vote for a poll that is not local".into(),
        )));
    }
    if status.account_id != poll.account_id {
        return Ok(VoteDisposition::Tallied(Applied::Ignored(
            "vote for a poll detached from its status".into(),
        )));
    }
    let Some(choice) = poll.options.iter().position(|o| o.title == choice_name) else {
        return Ok(VoteDisposition::NotAVote);
    };
    let pair = store.pair_state(poll.account_id, voter_id, now_ms)?;
    if pair.blocking || pair.blocked_by {
        return Ok(VoteDisposition::Tallied(Applied::Ignored(
            "vote from an account the poll author blocks or is blocked by".into(),
        )));
    }
    match store.vote_poll(poll.id, voter_id, &[choice as u64], now_ms) {
        Ok(_) => Ok(VoteDisposition::Tallied(Applied::Applied)),
        Err(VoteRejection::AlreadyVoted) => Ok(VoteDisposition::Tallied(Applied::Duplicate)),
        Err(VoteRejection::Expired) => Ok(VoteDisposition::Tallied(Applied::Ignored(
            "vote on an expired poll".into(),
        ))),
        Err(VoteRejection::SelfVote) => Ok(VoteDisposition::Tallied(Applied::Ignored(
            "vote by the poll author".into(),
        ))),
        Err(VoteRejection::InvalidChoice) => Ok(VoteDisposition::Tallied(Applied::Ignored(
            "vote with an invalid choice".into(),
        ))),
    }
}

/// A remote poll's new tallies: overwrite the matched options' counts (by
/// title, unknown names ignored), the voters count and the expiry. A poll
/// whose options or multiplicity changed is left alone (Mastodon ignores a
/// significant change on the update path); the status must already be known
/// (the caller ingests unknown ones).
pub fn sync_question(
    store: &dyn Store,
    author_id: Id,
    status_uri: &str,
    remote: &RemotePoll,
    now_ms: u64,
) -> Result<Applied> {
    let Some(status) = store.status_by_uri(status_uri)? else {
        return Ok(Applied::Ignored("update of an unknown status".into()));
    };
    if status.account_id != author_id {
        return Ok(Applied::Ignored("update by someone other than the author".into()));
    }
    let Some(poll) = store.poll_by_status(status.id)? else {
        return Ok(Applied::Ignored("update of a status without a poll".into()));
    };
    if poll.multiple != remote.multiple || poll.options.iter().map(|o| o.title.clone()).collect::<Vec<_>>() != remote.titles() {
        return Ok(Applied::Ignored("question changed options; not applied".into()));
    }
    let synced = store.sync_remote_poll(
        poll.id,
        &remote.options,
        remote.voters_count,
        remote.expires_at_ms,
        remote.expired,
        now_ms,
    )?;
    if synced.is_none() {
        return Ok(Applied::Ignored("update of a poll that is gone".into()));
    }
    // Our followers learn the new totals (a no-op when the poll is not local).
    store.enqueue_event(
        poll.account_id,
        serde_json::json!({"event": "question_update", "status_id": status.id.value()}),
        now_ms,
    )?;
    Ok(Applied::Applied)
}

/// A remote account blocks a local account (`target_id`).
pub fn block(store: &dyn Store, actor_id: Id, target_id: Id, now_ms: u64) -> Result<Applied> {
    match store.block(actor_id, target_id, now_ms)? {
        Some(_) => Ok(Applied::Applied),
        None => Ok(Applied::Duplicate),
    }
}

/// A remote account lifts its block.
pub fn unblock(store: &dyn Store, actor_id: Id, target_id: Id, now_ms: u64) -> Result<Applied> {
    match store.unblock(actor_id, target_id, now_ms)? {
        Some(_) => Ok(Applied::Applied),
        None => Ok(Applied::Duplicate),
    }
}

/// A remote account reports local accounts and their statuses. One report per
/// local target account; remote targets are not stored (Mastodon forwards them).
pub fn flag(
    store: &dyn Store,
    reporter_id: Id,
    target_uris: &[String],
    comment: &str,
    activity_uri: &str,
    now_ms: u64,
) -> Result<Applied> {
    let mut stored = 0usize;
    let mut status_targets = Vec::new();
    for uri in target_uris {
        if let Some(status) = store.status_by_uri(uri)? {
            status_targets.push(status);
        }
    }
    let mut accounts: Vec<Id> = Vec::new();
    for uri in target_uris {
        if let Some(account) = store.account_by_actor_uri(uri)? {
            if account.local && !accounts.contains(&account.id) {
                accounts.push(account.id);
            }
        }
    }
    for status in &status_targets {
        let owner = status.account_id;
        if let Some(account) = store.account_by_id(owner)? {
            if account.local && !accounts.contains(&owner) {
                accounts.push(owner);
            }
        }
    }
    for target in accounts {
        let status_ids: Vec<Id> = status_targets
            .iter()
            .filter(|s| s.account_id == target)
            .map(|s| s.id)
            .collect();
        store.insert_report(
            NewReport {
                reporter_id,
                target_account_id: target,
                status_ids,
                comment: comment.chars().take(5000).collect(),
                forwarded: false,
                activity_uri: Some(activity_uri.to_string()),
                category: "other".to_string(),
                rule_ids: Vec::new(),
                forward_to_domains: Vec::new(),
                collection_ids: Vec::new(),
            },
            now_ms,
        )?;
        stored += 1;
    }
    if stored == 0 {
        return Ok(Applied::Ignored("flag names no local account".into()));
    }
    Ok(Applied::Applied)
}

/// A remote account asks to quote a local status (`QuoteRequest`). The decision
/// follows the quoted status's quote policy. An accepted quote gets an
/// authorization URI, and an Accept (or Reject) is queued for the remote.
pub fn quote_request(
    store: &dyn Store,
    quoting_status_id: Id,
    quoted_uri: &str,
    activity_uri: &str,
    now_ms: u64,
) -> Result<Applied> {
    let Some(quoted) = store.status_by_uri(quoted_uri)? else {
        return Ok(Applied::Ignored("quote of an unknown status".into()));
    };
    let quoted_account = store
        .account_by_id(quoted.account_id)?
        .ok_or_else(|| Error::NotFound("quoted account".into()))?;
    if !quoted_account.local {
        return Ok(Applied::Ignored("quote of a status that is not local".into()));
    }
    let quoting = store
        .status_by_id(quoting_status_id)?
        .ok_or_else(|| Error::NotFound("quoting status".into()))?;
    if let Some(existing) = store.quote_by_quoting(quoting.id)? {
        if existing.state == QuoteState::Accepted {
            return Ok(Applied::Duplicate);
        }
    }
    let pair = store.pair_state(quoted.account_id, quoting.account_id, now_ms)?;
    let permitted = quote_permission(Some(quoting.account_id), &quoted, pair) != QuotePermission::Denied;
    let quote = match store.quote_by_quoting(quoting.id)? {
        Some(existing) => store.set_quote_state(
            existing.id,
            if permitted { QuoteState::Accepted } else { QuoteState::Rejected },
            None,
            Some(activity_uri.to_string()),
        )?,
        None => store.create_quote(
            NewQuote {
                quoting_status_id: quoting.id,
                quoted_status_id: quoted.id,
                quoting_account_id: quoting.account_id,
                quoted_account_id: quoted.account_id,
                state: if permitted { QuoteState::Accepted } else { QuoteState::Rejected },
                approval_uri: None,
                quote_activity_uri: Some(activity_uri.to_string()),
            },
            now_ms,
        )?,
    };
    if permitted {
        // The authorization URI names the quote, so it is set after the quote exists.
        let approval = format!("{}/quote_authorizations/{}", quoted_account.actor_uri, quote.id.value());
        store.set_quote_state(quote.id, QuoteState::Accepted, Some(approval), None)?;
    }
    store.enqueue_event(
        quoted.account_id,
        serde_json::json!({
            "event": if permitted { "quote_accept" } else { "quote_reject" },
            "quote_id": quote.id.value(),
        }),
        now_ms,
    )?;
    Ok(Applied::Applied)
}

/// A remote account accepts our quote request. `result` is its authorization URI.
pub fn quote_accepted(store: &dyn Store, quoting_uri: &str, authorization_uri: &str) -> Result<Applied> {
    let Some(status) = store.status_by_uri(quoting_uri)? else {
        return Ok(Applied::Ignored("accept of a quote of an unknown status".into()));
    };
    let Some(quote) = store.quote_by_quoting(status.id)? else {
        return Ok(Applied::Ignored("accept of a quote we did not request".into()));
    };
    if quote.state == QuoteState::Accepted {
        return Ok(Applied::Duplicate);
    }
    if quote.state != QuoteState::Pending {
        return Ok(Applied::Ignored("accept of a quote that is not pending".into()));
    }
    store.set_quote_state(quote.id, QuoteState::Accepted, Some(authorization_uri.to_string()), None)?;
    Ok(Applied::Applied)
}

/// A remote account rejects our quote request.
pub fn quote_rejected(store: &dyn Store, quoting_uri: &str) -> Result<Applied> {
    let Some(status) = store.status_by_uri(quoting_uri)? else {
        return Ok(Applied::Ignored("reject of a quote of an unknown status".into()));
    };
    let Some(quote) = store.quote_by_quoting(status.id)? else {
        return Ok(Applied::Ignored("reject of a quote we did not request".into()));
    };
    if quote.state != QuoteState::Pending {
        return Ok(Applied::Duplicate);
    }
    store.set_quote_state(quote.id, QuoteState::Rejected, None, None)?;
    Ok(Applied::Applied)
}

/// The quoted author revokes an authorization. Only the quoted author's
/// authorization can be revoked, by the author.
pub fn quote_revoked(store: &dyn Store, actor_id: Id, authorization_uri: &str) -> Result<Applied> {
    let Some(quote) = store.quote_by_approval_uri(authorization_uri)? else {
        return Ok(Applied::Ignored("revoke of an unknown authorization".into()));
    };
    if quote.quoted_account_id != actor_id {
        return Ok(Applied::Ignored("revoke by someone other than the quoted author".into()));
    }
    if quote.state == QuoteState::Revoked {
        return Ok(Applied::Duplicate);
    }
    store.set_quote_state(quote.id, QuoteState::Revoked, None, None)?;
    Ok(Applied::Applied)
}

// ---------------------------------------------------------------------------
// Remote custom emoji (W3).
// ---------------------------------------------------------------------------

/// One remote custom emoji named by a federated Note's `Emoji` tag.
///
/// Mirrors upstream `ActivityPub::Parser::CustomEmojiParser` (Mastodon
/// v4.7.3, `app/lib/activitypub/parser/custom_emoji_parser.rb`): `name` is
/// `:shortcode:` (colons stripped), `icon.url` is the image URL, `id` is the
/// emoji URI.
#[derive(Debug, Clone)]
pub struct RemoteEmoji {
    /// Shortcode without colons (`party_parrot`).
    pub shortcode: String,
    /// The `icon.url` the origin advertised.
    pub url: String,
    /// A static (non-animated) variant URL, when the tag carried one.
    /// Stored for the adapter's reference only; birchtree stages the
    /// animated bytes under both blob keys like `add_emoji` does.
    pub static_url: Option<String>,
    /// The author's domain: remote emoji are namespaced per origin.
    pub domain: String,
}

impl RemoteEmoji {
    /// Build from one `Emoji` tag (`name` like `:party_parrot:`).
    /// Returns `None` when the tag carries no usable shortcode, image URL
    /// or domain.
    pub fn from_tag(name: &str, url: &str, static_url: Option<&str>, domain: &str) -> Option<Self> {
        // Upstream deletes every colon (`@json['name']&.delete(':')`).
        let shortcode: String = name.chars().filter(|c| *c != ':').collect();
        if !CustomEmoji::shortcode_valid(&shortcode) {
            return None;
        }
        if url.is_empty() {
            return None;
        }
        let domain = domain.trim().to_lowercase();
        if domain.is_empty() {
            return None;
        }
        Some(Self {
            shortcode,
            url: url.to_string(),
            static_url: static_url.map(str::to_string),
            domain,
        })
    }
}

/// Storage key for a remote emoji. `CUSTOM_EMOJI` is keyed by shortcode
/// alone, so the origin domain joins the stored shortcode:
/// `party_parrot@toot.example` never collides with another origin's row or
/// with a local `party_parrot`. A future schema keying rows by
/// `(shortcode, domain)` can drop the suffix.
pub fn remote_emoji_key(shortcode: &str, domain: &str) -> String {
    format!("{}@{}", shortcode, domain.to_lowercase())
}

/// How one Note's remote emoji settled.
#[derive(Debug, Clone, Default)]
pub struct RemoteEmojiIngest {
    /// Rows stored (new or replaced).
    pub stored: usize,
    /// Refused entries (`shortcode@domain`, or a synthetic entry for the
    /// per-note cap) with the reason, in tag order.
    pub skipped: Vec<(String, String)>,
}

/// Bound on emoji tags processed per call: each row costs at most one
/// 256 KiB image, and a hostile Note may carry thousands of tags.
pub const MAX_REMOTE_EMOJI_PER_NOTE: usize = 50;

/// Store a Note's remote emoji from already-fetched image bytes.
///
/// `fetch` maps an emoji's image URL to its bytes (production: the safe
/// emoji fetch in `federation::remote_emoji`; tests: fixtures). Fetch
/// failures, oversize images (over `CustomEmoji::IMAGE_LIMIT`, 256 KiB),
/// non-emoji bytes and bad shortcodes are skipped — ingest never fails the
/// Note. Re-ingest replaces the row and its bytes (idempotent).
pub fn ingest_remote_emojis(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    emojis: &[RemoteEmoji],
    fetch: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<RemoteEmojiIngest> {
    let mut out = RemoteEmojiIngest::default();
    for emoji in emojis.iter().take(MAX_REMOTE_EMOJI_PER_NOTE) {
        let domain = emoji.domain.trim().to_lowercase();
        if !CustomEmoji::shortcode_valid(&emoji.shortcode) {
            out.skipped.push((
                format!("{}@{domain}", emoji.shortcode),
                "invalid shortcode".to_string(),
            ));
            continue;
        }
        if domain.is_empty() {
            out.skipped
                .push((emoji.shortcode.clone(), "missing domain".to_string()));
            continue;
        }
        let key = remote_emoji_key(&emoji.shortcode, &domain);
        let Some(image) = fetch(&emoji.url) else {
            out.skipped.push((key, "fetch failed".to_string()));
            continue;
        };
        if image.len() as u64 > CustomEmoji::IMAGE_LIMIT {
            out.skipped
                .push((key, "image larger than 256 KiB".to_string()));
            continue;
        }
        if !crate::media::ingest::is_emoji_image(&image) {
            out.skipped.push((key, "not an emoji image".to_string()));
            continue;
        }
        let partition = crate::application::emoji::emoji_partition(&key);
        let image_key = format!("custom_emojis/images/{partition}/original/{key}.png");
        let static_key = format!("custom_emojis/images/{partition}/static/{key}.png");
        blobs.put(&image_key, &image)?;
        blobs.put(&static_key, &image)?;
        // Upstream `CustomEmoji` leaves `disabled` false for remote emoji
        // (`disabled` defaults to FALSE; only the `listed` scope is
        // local-only). Mirror that: the row stays enabled so status text
        // can render it, but picker-invisible so it never joins the public
        // index (`CustomEmoji::listed` needs `visible_in_picker`).
        store.upsert_emoji(CustomEmoji {
            shortcode: key,
            image_key,
            static_image_key: static_key,
            visible_in_picker: false,
            disabled: false,
            category: None,
        })?;
        out.stored += 1;
    }
    if emojis.len() > MAX_REMOTE_EMOJI_PER_NOTE {
        out.skipped.push((
            format!("{} further tag(s)", emojis.len() - MAX_REMOTE_EMOJI_PER_NOTE),
            "per-note emoji cap".to_string(),
        ));
    }
    Ok(out)
}

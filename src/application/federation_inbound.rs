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
use crate::application::ports::{NewMedia, NewPollSpec, NewStatus, RemoteStatusIdentity, Store, VoteRejection};use crate::domain::{Error, Id, JobKind, QuoteState, Result, Status, Visibility};

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
    /// The `actor` URIs of the Note's mentions.
    pub mention_uris: Vec<String>,
    pub tags: Vec<String>,
    pub attachments: Vec<RemoteAttachment>,
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
        })?;
        store.set_remote_media_source(media.id, &attachment.source_url)?;
        media_ids.push(media.id);
        pending_fetches.push((media.id, attachment.source_url.clone()));
    }
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
    for (media_id, source_url) in pending_fetches {
        // Metadata-first shell in the cache ledger (storage overhaul,
        // `docs/database.md` Agent B): no bytes, no accounting change. The
        // eager `FetchRemoteMedia` enqueue below is kept until the
        // fetch-on-first-request miss path lands (follow-up); warming stays
        // off by default behind `should_warm`. Empty sources skip the shell
        // (the job quarantines them as before).
        if !source_url.is_empty() {
            crate::federation::media::record_metadata_first(
                store as &dyn crate::application::federation_ports::FederationContentStore,
                media_id,
                &source_url,
                now_ms,
            )?;
        }
        store.enqueue(
            JobKind::FetchRemoteMedia,
            serde_json::json!({ "media_id": media_id.value() }),
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

//! Outbound activities: the JSON of each local change, and the inboxes it goes to.
//!
//! The JSON is built from local state at the time the federation event is
//! handled (`federation::events`). Its bytes are then fixed in the delivery job,
//! so a retry sends the same activity. Shapes follow Mastodon 4.7.3's
//! serializers (`app/serializers/activitypub/*`), which are the contract.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use url::Url;

use crate::activitypub::wire::PUBLIC;
use crate::application::ports::Store;
use crate::domain::{Account, Error, Id, Poll, QuoteState, Result, Status, Visibility};

/// The JSON-LD context of every outbound document: ActivityStreams plus the
/// security vocabulary (Mastodon reads `publicKey` through it).
pub fn context() -> Value {
    json!([
        "https://www.w3.org/ns/activitystreams",
        "https://w3id.org/security/v1"
    ])
}

fn iso(ms: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms as i64)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// The followers collection URL of a local account (advertised in its actor document).
pub fn followers_uri(account: &Account) -> String {
    format!("{}/followers", account.actor_uri)
}

/// The audience of a status: `(to, cc)`, as Mastodon's `TagManager#to`/`#cc` compute it.
pub fn audience(visibility: Visibility, author: &Account, mentions: &[String]) -> (Vec<String>, Vec<String>) {
    let followers = followers_uri(author);
    match visibility {
        Visibility::Public => (vec![PUBLIC.into()], [vec![followers], mentions.to_vec()].concat()),
        Visibility::Unlisted => (vec![followers], [vec![PUBLIC.into()], mentions.to_vec()].concat()),
        Visibility::Private => (vec![followers], mentions.to_vec()),
        Visibility::Direct => (mentions.to_vec(), Vec::new()),
    }
}

/// The actor URI of each mentioned account (local or remote).
fn mention_uris(store: &dyn Store, ids: &[Id]) -> Result<Vec<String>> {
    let mut uris = Vec::new();
    for id in ids {
        if let Some(account) = store.account_by_id(*id)? {
            uris.push(account.actor_uri);
        }
    }
    Ok(uris)
}

/// A local status as an ActivityStreams `Note`, with its audience, mentions,
/// hashtags, attachments, quote and language. A status with a poll is a
/// `Question` instead (Mastodon serves the poll in place of the Note).
pub fn note_for_status(store: &dyn Store, public_url: &str, status: &Status, now_ms: u64) -> Result<Value> {
    let mut note = note_inner(store, public_url, status)?;
    if let Some(poll) = store.poll_by_status(status.id)? {
        apply_question(&mut note, &poll, now_ms);
    }
    Ok(note)
}

/// A local poll as an ActivityStreams `Question` (Mastodon 4.7.3
/// `NoteSerializer`: `oneOf`/`anyOf` options, `endTime`/`closed`,
/// `votersCount`). The audience and content are the status's own.
pub fn question_for_status(
    store: &dyn Store,
    public_url: &str,
    status: &Status,
    poll: &Poll,
    now_ms: u64,
) -> Result<Value> {
    let mut note = note_inner(store, public_url, status)?;
    apply_question(&mut note, poll, now_ms);
    Ok(note)
}

/// Turn a Note value into its Question shape, in place.
fn apply_question(note: &mut Value, poll: &Poll, now_ms: u64) {
    let options: Vec<Value> = poll
        .options
        .iter()
        .map(|o| {
            json!({
                "type": "Note",
                "name": o.title,
                "replies": {"type": "Collection", "totalItems": o.votes_count},
            })
        })
        .collect();
    note["type"] = json!("Question");
    if poll.multiple {
        note["anyOf"] = json!(options);
    } else {
        note["oneOf"] = json!(options);
    }
    note["votersCount"] = json!(poll.voters_count);
    if let Some(expires) = poll.expires_at_ms {
        if poll.is_expired_at(now_ms) {
            note["closed"] = json!(iso(expires));
        } else {
            note["endTime"] = json!(iso(expires));
        }
    }
}

fn note_inner(store: &dyn Store, public_url: &str, status: &Status) -> Result<Value> {
    let author = store
        .account_by_id(status.account_id)?
        .ok_or_else(|| Error::NotFound("status author".into()))?;
    let mentions = mention_uris(store, &status.mentions)?;
    let (to, cc) = audience(status.visibility, &author, &mentions);

    let mut tags: Vec<Value> = Vec::new();
    for uri in &mentions {
        tags.push(json!({"type": "Mention", "href": uri, "name": mention_name(store, uri)?}));
    }
    for tag in &status.tags {
        tags.push(json!({"type": "Hashtag", "href": format!("{public_url}/tags/{tag}"), "name": format!("#{tag}")}));
    }

    let media = store.media_by_ids(&status.media_ids)?;
    let attachments: Vec<Value> = status
        .media_ids
        .iter()
        .filter_map(|id| media.get(id))
        .filter(|m| {
            m.state == crate::domain::MediaState::Ready && !m.file_key.is_empty()
                || m.url.is_some()
        })
        .map(|m| {
            let url = m.url.clone().unwrap_or_else(|| {
                format!("{public_url}/system/media_attachments/files/{}", m.file_key)
            });
            let mut doc = json!({
                "type": "Document",
                "mediaType": m.mime_type,
                "url": url,
                "name": m.description,
            });
            if let Some(blurhash) = &m.blurhash {
                doc["blurhash"] = json!(blurhash);
            }
            if let (Some(w), Some(h)) = (m.width, m.height) {
                doc["width"] = json!(w);
                doc["height"] = json!(h);
            }
            doc
        })
        .collect();

    let in_reply_to = match status.in_reply_to_id {
        Some(parent) => store.status_by_id(parent)?.map(|p| p.uri),
        None => None,
    };

    let mut note = json!({
        "id": status.uri,
        "type": "Note",
        "summary": status.spoiler_text.clone().filter(|s| !s.is_empty()),
        "inReplyTo": in_reply_to,
        "published": iso(status.created_at_ms),
        "url": status.url,
        "attributedTo": author.actor_uri,
        "to": to,
        "cc": cc,
        "sensitive": status.sensitive,
        "content": status.content_html,
        "tag": tags,
        "attachment": attachments,
    });
    if let Some(edited) = status.edited_at_ms {
        note["updated"] = json!(iso(edited));
    }
    if let Some(language) = &status.language {
        note["contentMap"] = json!({ language: status.content_html });
    }
    if let Some(quote) = store.quote_by_quoting(status.id)? {
        if let Some(quoted) = store.status_by_id(quote.quoted_status_id)? {
            if !matches!(quote.state, QuoteState::Rejected | QuoteState::Revoked | QuoteState::Deleted) {
                note["quote"] = json!(quoted.uri);
            }
        }
        if quote.state == QuoteState::Accepted {
            if let Some(approval) = &quote.approval_uri {
                note["quoteAuthorization"] = json!(approval);
            }
        }
    }
    Ok(note)
}

fn mention_name(store: &dyn Store, actor_uri: &str) -> Result<String> {
    Ok(match store.account_by_actor_uri(actor_uri)? {
        Some(a) if a.local => format!("@{}", a.username),
        Some(a) => format!("@{}@{}", a.username, a.domain),
        None => actor_uri.to_string(),
    })
}

/// `Create` of a local status (a `Question` when it has a poll).
pub fn create(store: &dyn Store, public_url: &str, status: &Status, now_ms: u64) -> Result<Value> {
    let note = note_for_status(store, public_url, status, now_ms)?;
    Ok(json!({
        "@context": context(),
        "id": format!("{}/activity", status.uri),
        "type": "Create",
        "actor": note["attributedTo"],
        "to": note["to"],
        "cc": note["cc"],
        "object": note,
    }))
}

/// `Update` of a local status's Note.
pub fn update(store: &dyn Store, public_url: &str, status: &Status, now_ms: u64) -> Result<Value> {
    let note = note_for_status(store, public_url, status, now_ms)?;
    let stamp = status.edited_at_ms.unwrap_or(status.created_at_ms);
    Ok(json!({
        "@context": context(),
        "id": format!("{}#updates/{}", status.uri, stamp),
        "type": "Update",
        "actor": note["attributedTo"],
        "to": note["to"],
        "cc": note["cc"],
        "object": note,
    }))
}

/// `Update` of a local poll's Question, sent as votes arrive and at expiry.
/// The id carries the send time (not the status edit time), so every new
/// tally is a distinct activity for the origin's dedup.
pub fn question_update(store: &dyn Store, public_url: &str, status: &Status, now_ms: u64) -> Result<Value> {
    let poll = store
        .poll_by_status(status.id)?
        .ok_or_else(|| Error::NotFound("poll of status".into()))?;
    let question = question_for_status(store, public_url, status, &poll, now_ms)?;
    Ok(json!({
        "@context": context(),
        "id": format!("{}#updates/{}", status.uri, now_ms),
        "type": "Update",
        "actor": question["attributedTo"],
        "to": question["to"],
        "cc": question["cc"],
        "object": question,
    }))
}

/// `Update` of a local actor's profile (upstream
/// `ActivityPub::UpdateActorSerializer` as sent by
/// `UpdateDistributionWorker`): the full actor document is the object, so a
/// remote refetch picks up display name, avatar (`icon`) and header
/// (`image`) alike. Debounce is upstream's (5 s `perform_in`); here the
/// event enqueues immediately (documented in the federation contract).
pub fn profile_update(author: &Account, person: serde_json::Value, now_ms: u64) -> Value {
    json!({
        "@context": context(),
        "id": format!("{}#updates/{}", author.actor_uri, now_ms),
        "type": "Update",
        "actor": author.actor_uri,
        "to": [PUBLIC],
        "object": person,
    })
}

/// `Delete` of a local status: the Note becomes a `Tombstone` with its URI.
pub fn delete(author: &Account, status: &Status) -> Value {
    json!({
        "@context": context(),
        "id": format!("{}#delete", status.uri),
        "type": "Delete",
        "actor": author.actor_uri,
        "to": [PUBLIC],
        "object": {"id": status.uri, "type": "Tombstone", "formerType": "Note"},
    })
}

/// `Delete` of a local actor (account deletion, upstream
/// `ActivityPub::DeleteActorSerializer` as sent by `DeleteAccountService`):
/// the actor becomes a `Tombstone`. Recipients are the account's followers
/// (relays do not exist here, so there is no low-priority fan-out).
pub fn delete_actor(author: &Account) -> Value {
    json!({
        "@context": context(),
        "id": format!("{}#delete", author.actor_uri),
        "type": "Delete",
        "actor": author.actor_uri.clone(),
        "to": [PUBLIC],
        "object": {"id": author.actor_uri, "type": "Tombstone", "formerType": "Person"},
    })
}

/// A local vote on a remote poll, as Mastodon sends it
/// (`VoteSerializer`): a `Create` of a `Note` naming the chosen option,
/// `inReplyTo` the poll, addressed to the poll's author.
pub fn vote(
    voter: &Account,
    poll_owner: &Account,
    poll_status: &Status,
    vote_id: &str,
    choice_title: &str,
) -> Value {
    let object = json!({
        "id": format!("{vote_id}"),
        "type": "Note",
        "name": choice_title,
        "attributedTo": voter.actor_uri,
        "inReplyTo": poll_status.uri,
        "to": [poll_owner.actor_uri],
    });
    json!({
        "@context": context(),
        "id": format!("{vote_id}/activity"),
        "type": "Create",
        "actor": voter.actor_uri,
        "to": [poll_owner.actor_uri],
        "object": object,
    })
}

/// `Like` of a remote status by a local account.
pub fn like(author: &Account, target: &Status) -> Value {
    json!({
        "@context": context(),
        "id": format!("{}#likes/{}", author.actor_uri, target.id.value()),
        "type": "Like",
        "actor": author.actor_uri,
        "object": target.uri,
    })
}

/// `Undo` of a `Like`.
pub fn undo_like(author: &Account, target: &Status) -> Value {
    let like = like(author, target);
    json!({
        "@context": context(),
        "id": format!("{}/undo", like["id"].as_str().unwrap_or_default()),
        "type": "Undo",
        "actor": author.actor_uri,
        "object": like,
    })
}

/// `Announce` of a status by a local account (the boost's own URI is its ID).
pub fn announce(store: &dyn Store, author: &Account, boost_uri: &str, target: &Status, visibility: Visibility) -> Result<Value> {
    let (to, cc) = audience(visibility, author, &[]);
    let _ = store;
    Ok(json!({
        "@context": context(),
        "id": boost_uri,
        "type": "Announce",
        "actor": author.actor_uri,
        "published": iso(target.created_at_ms),
        "to": to,
        "cc": cc,
        "object": target.uri,
    }))
}

/// `Undo` of an `Announce`.
pub fn undo_announce(author: &Account, boost_uri: &str, target: &Status) -> Value {
    json!({
        "@context": context(),
        "id": format!("{boost_uri}/undo"),
        "type": "Undo",
        "actor": author.actor_uri,
        "object": {
            "id": boost_uri,
            "type": "Announce",
            "actor": author.actor_uri,
            "object": target.uri,
        },
    })
}

/// `Block` of a remote account by a local account.
pub fn block(author: &Account, target: &Account) -> Value {
    json!({
        "@context": context(),
        "id": format!("{}#blocks/{}", author.actor_uri, target.id.value()),
        "type": "Block",
        "actor": author.actor_uri,
        "object": target.actor_uri,
    })
}

/// `Undo` of a `Block`.
pub fn undo_block(author: &Account, target: &Account) -> Value {
    let block = block(author, target);
    json!({
        "@context": context(),
        "id": format!("{}/undo", block["id"].as_str().unwrap_or_default()),
        "type": "Undo",
        "actor": author.actor_uri,
        "object": block,
    })
}

/// `Flag` of a local report, to the target account's server.
pub fn flag(author: &Account, target: &Account, status_uris: &[String], comment: &str, report_id: Id) -> Value {
    let mut object = vec![target.actor_uri.clone()];
    object.extend(status_uris.iter().cloned());
    json!({
        "@context": context(),
        "id": format!("{}#reports/{}", author.actor_uri, report_id.value()),
        "type": "Flag",
        "actor": author.actor_uri,
        "object": object,
        "content": comment,
    })
}

/// The `QuoteRequest` of a quote of a remote status, sent to the quoted author.
pub fn quote_request(store: &dyn Store, public_url: &str, quote_uri: &str, quoting: &Status, quoted: &Status, now_ms: u64) -> Result<Value> {
    let author = store
        .account_by_id(quoting.account_id)?
        .ok_or_else(|| Error::NotFound("quoting author".into()))?;
    let instrument = note_for_status(store, public_url, quoting, now_ms)?;
    Ok(json!({
        "@context": context(),
        "id": quote_uri,
        "type": "QuoteRequest",
        "actor": author.actor_uri,
        "object": quoted.uri,
        "instrument": instrument,
    }))
}

/// `Accept` of a quote request: the authorization is the `result`.
pub fn accept_quote(quoted_author: &Account, quote_request: &Value, quote_id: Id, approval_uri: &str) -> Value {
    json!({
        "@context": context(),
        "id": format!("{}#accepts/quote_requests/{}", quoted_author.actor_uri, quote_id.value()),
        "type": "Accept",
        "actor": quoted_author.actor_uri,
        "object": quote_request,
        "result": approval_uri,
    })
}

/// `Reject` of a quote request.
pub fn reject_quote(quoted_author: &Account, quote_request: &Value, quote_id: Id) -> Value {
    json!({
        "@context": context(),
        "id": format!("{}#rejects/quote_requests/{}", quoted_author.actor_uri, quote_id.value()),
        "type": "Reject",
        "actor": quoted_author.actor_uri,
        "object": quote_request,
    })
}

/// `Delete` of a `QuoteAuthorization`: the quoted author revokes the quote.
pub fn revoke_quote(quoted_author: &Account, approval_uri: &str, quoting_uri: &str, quoted_uri: &str) -> Value {
    json!({
        "@context": context(),
        "id": format!("{approval_uri}#delete"),
        "type": "Delete",
        "actor": quoted_author.actor_uri,
        "to": [PUBLIC],
        "object": {
            "id": approval_uri,
            "type": "QuoteAuthorization",
            "attributedTo": quoted_author.actor_uri,
            "interactingObject": quoting_uri,
            "interactionTarget": quoted_uri,
        },
    })
}

/// The inbox an account is reached at: its shared inbox if it has one. Local accounts have none.
pub fn inbox_of(account: &Account) -> Option<String> {
    if account.local {
        return None;
    }
    account.shared_inbox_uri.clone().or_else(|| account.inbox_uri.clone())
}

/// The distinct remote inboxes of an account's remote followers.
pub fn follower_inboxes(store: &dyn Store, account_id: Id) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut inboxes = Vec::new();
    let followers = store.followers(account_id, None, 10_000)?;
    for id in followers {
        if let Some(account) = store.account_by_id(id)? {
            if let Some(inbox) = inbox_of(&account) {
                if seen.insert(inbox.clone()) {
                    inboxes.push(inbox);
                }
            }
        }
    }
    Ok(inboxes)
}

/// The distinct remote inboxes of the given accounts.
pub fn inboxes_of(store: &dyn Store, ids: &[Id]) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut inboxes = Vec::new();
    for id in ids {
        if let Some(account) = store.account_by_id(*id)? {
            if let Some(inbox) = inbox_of(&account) {
                if seen.insert(inbox.clone()) {
                    inboxes.push(inbox);
                }
            }
        }
    }
    Ok(inboxes)
}

/// Whether a URL is one birchtree serves (a local status, actor or collection).
pub fn is_local_url(public_url: &str, url: &str) -> bool {
    let base = public_url.trim_end_matches('/');
    url.starts_with(base)
}

/// The parsed form of a URL, for the activity's own identifiers.
pub fn parse_url(s: &str) -> Result<Url> {
    Url::parse(s).map_err(|e| Error::Internal(format!("activity url {s}: {e}")))
}

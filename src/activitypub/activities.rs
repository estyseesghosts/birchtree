//! The activities birchtree accepts (inbound) and sends (outbound) in Phase 6.
//!
//! Each type implements the crate's `ActivityHandler`. `verify` checks the
//! activity's own shape (its actor against the object it names); `receive`
//! applies the effect through `application::federation_inbound` or
//! `application::federation`, then records the activity URI if it applied (D33).
//!
//! The inbox parses the body as [`Activity`]. A type outside this set is refused
//! at parse time (422, D32), so nothing is accepted and dropped.

use activitypub_federation::{
    config::Data,
    fetch::object_id::ObjectId,
    kinds::activity::{
        AcceptType, AnnounceType, BlockType, CreateType, DeleteType, FlagType, FollowType, LikeType,
        RejectType, UndoType, UpdateType,
    },
    protocol::{context::WithContext, verification::verify_domains_match},
    traits::ActivityHandler,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::activitypub::actor::ActorRecord;
use crate::activitypub::error::FedError;
use crate::activitypub::objects::NoteRef;
use crate::activitypub::wire::{Note, ObjectRef, Question, QuoteRequest, PUBLIC};
use crate::activitypub::FedContext;
use crate::application::federation::{
    receive_accept, receive_follow, receive_reject, receive_undo_follow, InboundAccept, InboundFollow,
    InboundReject, InboundUndoFollow,
};
use crate::application::federation_inbound::{self as inbound, Applied, RemoteNote};
use crate::application::federation_ports::InboundDisposition;
use crate::domain::{Id, Visibility};
use crate::federation::tombstone::{self, CheckedIngest};

/// Every activity the inbox accepts. The body is the crate's `WithContext<Activity>`.
///
/// The order matters: an untagged enum takes the first variant that parses, and each
/// variant's `type` is a strict kind, so only one variant can match a given body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Activity {
    Follow(Follow),
    Accept(Accept),
    Reject(Reject),
    Undo(Undo),
    Create(Create),
    Update(Update),
    Delete(Delete),
    Like(Like),
    Announce(Announce),
    Block(Block),
    Flag(Flag),
    QuoteRequest(QuoteRequestActivity),
}

impl Activity {
    /// The activity's canonical ID.
    pub fn id(&self) -> &Url {
        match self {
            Activity::Follow(a) => &a.id,
            Activity::Accept(a) => &a.id,
            Activity::Reject(a) => &a.id,
            Activity::Undo(a) => &a.id,
            Activity::Create(a) => &a.id,
            Activity::Update(a) => &a.id,
            Activity::Delete(a) => &a.id,
            Activity::Like(a) => &a.id,
            Activity::Announce(a) => &a.id,
            Activity::Block(a) => &a.id,
            Activity::Flag(a) => &a.id,
            Activity::QuoteRequest(a) => &a.id,
        }
    }
}

macro_rules! delegate {
    ($self:ident, $method:ident $(, $arg:expr)*) => {
        match $self {
            Activity::Follow(a) => a.$method($($arg),*).await,
            Activity::Accept(a) => a.$method($($arg),*).await,
            Activity::Reject(a) => a.$method($($arg),*).await,
            Activity::Undo(a) => a.$method($($arg),*).await,
            Activity::Create(a) => a.$method($($arg),*).await,
            Activity::Update(a) => a.$method($($arg),*).await,
            Activity::Delete(a) => a.$method($($arg),*).await,
            Activity::Like(a) => a.$method($($arg),*).await,
            Activity::Announce(a) => a.$method($($arg),*).await,
            Activity::Block(a) => a.$method($($arg),*).await,
            Activity::Flag(a) => a.$method($($arg),*).await,
            Activity::QuoteRequest(a) => a.$method($($arg),*).await,
        }
    };
}

#[async_trait]
impl ActivityHandler for Activity {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        Activity::id(self)
    }

    fn actor(&self) -> &Url {
        match self {
            Activity::Follow(a) => a.actor.inner(),
            Activity::Accept(a) => a.actor.inner(),
            Activity::Reject(a) => a.actor.inner(),
            Activity::Undo(a) => a.actor.inner(),
            Activity::Create(a) => a.actor.inner(),
            Activity::Update(a) => a.actor.inner(),
            Activity::Delete(a) => a.actor.inner(),
            Activity::Like(a) => a.actor.inner(),
            Activity::Announce(a) => a.actor.inner(),
            Activity::Block(a) => a.actor.inner(),
            Activity::Flag(a) => a.actor.inner(),
            Activity::QuoteRequest(a) => &a.actor,
        }
    }

    async fn verify(&self, data: &Data<FedContext>) -> Result<(), FedError> {
        delegate!(self, verify, data)
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        match self {
            Activity::Follow(a) => a.receive(data).await,
            Activity::Accept(a) => a.receive(data).await,
            Activity::Reject(a) => a.receive(data).await,
            Activity::Undo(a) => a.receive(data).await,
            Activity::Create(a) => a.receive(data).await,
            Activity::Update(a) => a.receive(data).await,
            Activity::Delete(a) => a.receive(data).await,
            Activity::Like(a) => a.receive(data).await,
            Activity::Announce(a) => a.receive(data).await,
            Activity::Block(a) => a.receive(data).await,
            Activity::Flag(a) => a.receive(data).await,
            Activity::QuoteRequest(a) => a.receive(data).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Activity shapes.
// ---------------------------------------------------------------------------

/// `Follow`: the actor asks to follow the local `object`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    #[serde(rename = "type")]
    pub kind: FollowType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: ObjectId<ActorRecord>,
}

/// `Accept`: the actor accepts our follow or our quote request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accept {
    #[serde(rename = "type")]
    pub kind: AcceptType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: AcceptObject,
    /// For an accepted quote request: the authorization URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Url>,
}

/// `Reject`: the actor rejects our follow or our quote request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reject {
    #[serde(rename = "type")]
    pub kind: RejectType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: AcceptObject,
}

/// The object of an Accept or Reject: an embedded follow, an embedded quote request, or a URI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AcceptObject {
    Follow(Box<Follow>),
    QuoteRequest(Box<QuoteRequest>),
    Id(Url),
}

impl AcceptObject {
    /// The URI of the thing being answered.
    pub fn uri(&self) -> &Url {
        match self {
            AcceptObject::Follow(f) => &f.id,
            AcceptObject::QuoteRequest(q) => &q.id,
            AcceptObject::Id(url) => url,
        }
    }

    /// The account the embedded object is directed at (the answering actor must be it).
    fn directed_at(&self) -> Option<&Url> {
        match self {
            AcceptObject::Follow(f) => Some(f.object.inner()),
            AcceptObject::QuoteRequest(q) => Some(&q.object),
            AcceptObject::Id(_) => None,
        }
    }
}

/// `Undo`: the actor withdraws an earlier activity. Only the embedded types below are undone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Undo {
    #[serde(rename = "type")]
    pub kind: UndoType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: UndoObject,
}

/// The object of an Undo.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UndoObject {
    Follow(Box<Follow>),
    Like(Box<Like>),
    Announce(Box<Announce>),
    Block(Box<Block>),
}

/// `Create`: the actor publishes a Note.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    #[serde(rename = "type")]
    pub kind: CreateType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: CreateObject,
}

/// The object of a Create: an embedded Note, an embedded Question (a poll),
/// or its URI (dereferenced on receipt).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CreateObject {
    Note(Box<Note>),
    Question(Box<Question>),
    Id(Url),
}

/// `Update`: the actor edits a Note, or refreshes its own profile (object `Person`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    #[serde(rename = "type")]
    pub kind: UpdateType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: serde_json::Value,
}

/// `Delete`: the actor removes a Note (object a URI or a `Tombstone`), or its account (object the actor).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delete {
    #[serde(rename = "type")]
    pub kind: DeleteType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: serde_json::Value,
}

/// `Like`: the actor favourites a status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Like {
    #[serde(rename = "type")]
    pub kind: LikeType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: ObjectRef,
}

/// `Announce`: the actor boosts a status.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Announce {
    #[serde(rename = "type")]
    pub kind: AnnounceType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: ObjectRef,
    #[serde(default, deserialize_with = "crate::activitypub::wire::one_or_many_urls")]
    pub to: Vec<Url>,
    #[serde(default, deserialize_with = "crate::activitypub::wire::one_or_many_urls")]
    pub cc: Vec<Url>,
}

/// `Block`: the actor blocks a local account.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    #[serde(rename = "type")]
    pub kind: BlockType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: Url,
}

/// `Flag`: the actor reports accounts and statuses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flag {
    #[serde(rename = "type")]
    pub kind: FlagType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    #[serde(deserialize_with = "crate::activitypub::wire::one_or_many_refs")]
    pub object: Vec<ObjectRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// A `QuoteRequest` activity (its shape is [`QuoteRequest`]); the wrapper gives the
/// activity handler on the enum.
pub type QuoteRequestActivity = QuoteRequest;

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
fn log_outcome(kind: &str, id: &Url, actor: &Url, applied: &Applied) {
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

fn log_disposition(kind: &str, id: &Url, actor: &Url, disposition: &InboundDisposition) {
    let applied = match disposition {
        InboundDisposition::Applied => Applied::Applied,
        InboundDisposition::Duplicate => Applied::Duplicate,
        InboundDisposition::Ignored(reason) => Applied::Ignored(reason.clone()),
    };
    log_outcome(kind, id, actor, &applied);
}

/// Record an applied activity, so a redelivery is a duplicate (never an ignored one).
fn record(store: &dyn crate::application::ports::Store, id: &Url, applied: &Applied, now_ms: u64) -> Result<(), FedError> {
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
pub fn note_input_for(note: &Note, author_id: Id, author_domain: &str, author_sensitized: bool) -> RemoteNote {
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
                            .filter(|id| {
                                crate::domain::license::is_valid_license(*id)
                            })
                    }),
                })
            })
            .collect(),
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
        emojis: note
            .tag
            .iter()
            .filter(|t| t.kind == "Emoji")
            .filter_map(|t| {
                inbound::RemoteEmoji::from_tag(
                    t.name.as_deref().unwrap_or(""),
                    t.icon
                        .as_ref()
                        .and_then(|icon| icon.url.as_ref())
                        .map(|u| u.as_str())
                        .unwrap_or(""),
                    None,
                    author_domain,
                )
            })
            .collect(),
        quote_uri: note.quote.as_ref().map(|u| u.to_string()),
        poll: None,
    }
}

/// Turn a verified Note into the application's input, for the given author.
///
/// Kept for callers without the author's account row (existing tests):
/// production ingest uses [`note_input_for`], which also forces `sensitive`
/// for sensitized authors.
pub fn note_input(note: &Note, author_id: Id, author_domain: &str) -> RemoteNote {
    note_input_for(note, author_id, author_domain, false)
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
pub fn question_input_for(question: &Question, author_id: Id, author_sensitized: bool, now_ms: u64) -> (RemoteNote, inbound::RemotePoll) {
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
        emojis: Vec::new(),
        location: None,
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
pub fn question_input(question: &Question, author_id: Id, now_ms: u64) -> (RemoteNote, inbound::RemotePoll) {
    question_input_for(question, author_id, false, now_ms)
}

/// An object's author must be the actor that sent it, and its ID must be on the actor's host.
fn verify_object_author(id: &Url, attributed_to: &Url, actor: &Url) -> Result<(), FedError> {
    if attributed_to != actor {
        return Err(FedError::Refused("object is attributed to another actor".into()));
    }
    verify_domains_match(id, actor)?;
    Ok(())
}

/// The Note's author must be the actor that sent it, and its ID must be on the actor's host.
fn verify_note_author(note: &Note, actor: &Url) -> Result<(), FedError> {
    verify_object_author(&note.id, &note.attributed_to, actor)
}

// ---------------------------------------------------------------------------
// Follow, Accept, Reject, Undo of Follow (Phase 6 first slice).
// ---------------------------------------------------------------------------

#[async_trait]
impl ActivityHandler for Follow {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, data: &Data<FedContext>) -> Result<(), FedError> {
        if !self.object.is_local(data) {
            return Err(FedError::Refused("follow object is not a local account".into()));
        }
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let target = self.object.dereference(data).await?;
        let follower = self.actor.dereference(data).await?;
        let accept = accept_for_follow(&self, &target);
        let accept_body = serde_json::to_value(WithContext::new_default(Activity::Accept(accept.clone())))
            .map_err(|e| FedError::Refused(e.to_string()))?;
        let disposition = receive_follow(
            data.app.store.as_ref(),
            InboundFollow {
                activity_uri: self.id.to_string(),
                actor: follower.to_upsert()?,
                target_id: target.account.id,
                accept_uri: accept.id.to_string(),
                accept_body,
            },
            data.app.now_ms(),
        )?;
        log_disposition("Follow", &self.id, self.actor.inner(), &disposition);
        Ok(())
    }
}

/// The Accept that answers a Follow. Its ID lives on the accepting actor's URL,
/// because the crate requires an activity's ID and actor to share a domain. It is
/// derived from the follow's ID, so a redelivered Follow yields the same Accept.
fn accept_for_follow(follow: &Follow, target: &ActorRecord) -> Accept {
    let mut id = target.url().clone();
    id.set_fragment(Some(&format!("accepts/{}", follow.id)));
    Accept {
        kind: Default::default(),
        id,
        actor: target.url().clone().into(),
        object: AcceptObject::Follow(Box::new(follow.clone())),
        result: None,
    }
}

#[async_trait]
impl ActivityHandler for Accept {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        match self.object.directed_at() {
            Some(followed) if followed != self.actor.inner() => {
                Err(FedError::Refused("accept names a request of another account".into()))
            }
            _ => Ok(()),
        }
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let actor = self.actor.dereference(data).await?;
        match &self.object {
            AcceptObject::QuoteRequest(quote) => {
                // Our quote request, accepted: the quoting status is the instrument.
                let quoting = quote.instrument.uri().to_string();
                let authorization = self
                    .result
                    .as_ref()
                    .ok_or_else(|| FedError::Refused("accept of a quote has no result".into()))?;
                let applied = inbound::quote_accepted(store, &quoting, authorization.as_str())?;
                log_outcome("Accept", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
                Ok(())
            }
            other => {
                let disposition = receive_accept(
                    store,
                    InboundAccept {
                        activity_uri: self.id.to_string(),
                        actor: actor.to_upsert()?,
                        follow_uri: other.uri().to_string(),
                    },
                    now,
                )?;
                log_disposition("Accept", &self.id, self.actor.inner(), &disposition);
                Ok(())
            }
        }
    }
}

#[async_trait]
impl ActivityHandler for Reject {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        match self.object.directed_at() {
            Some(followed) if followed != self.actor.inner() => {
                Err(FedError::Refused("reject names a request of another account".into()))
            }
            _ => Ok(()),
        }
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let actor = self.actor.dereference(data).await?;
        match &self.object {
            AcceptObject::QuoteRequest(quote) => {
                let applied = inbound::quote_rejected(store, &quote.instrument.uri().to_string())?;
                log_outcome("Reject", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
                Ok(())
            }
            other => {
                let disposition = receive_reject(
                    store,
                    InboundReject {
                        activity_uri: self.id.to_string(),
                        actor: actor.to_upsert()?,
                        follow_uri: other.uri().to_string(),
                    },
                    now,
                )?;
                log_disposition("Reject", &self.id, self.actor.inner(), &disposition);
                Ok(())
            }
        }
    }
}

#[async_trait]
impl ActivityHandler for Undo {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, data: &Data<FedContext>) -> Result<(), FedError> {
        // Only the actor may undo its own activity.
        let embedded_actor = match &self.object {
            UndoObject::Follow(f) => f.actor.inner(),
            UndoObject::Like(l) => l.actor.inner(),
            UndoObject::Announce(a) => a.actor.inner(),
            UndoObject::Block(b) => b.actor.inner(),
        };
        if embedded_actor != self.actor.inner() {
            return Err(FedError::Refused("undo names an activity by another actor".into()));
        }
        if let UndoObject::Follow(f) = &self.object {
            if !f.object.is_local(data) {
                return Err(FedError::Refused("undone follow is not of a local account".into()));
            }
        }
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        match self.object.clone() {
            UndoObject::Follow(follow) => {
                let target = follow.object.dereference(data).await?;
                let follower = self.actor.dereference(data).await?;
                let disposition = receive_undo_follow(
                    store,
                    InboundUndoFollow {
                        activity_uri: self.id.to_string(),
                        actor: follower.to_upsert()?,
                        target_id: target.account.id,
                    },
                    now,
                )?;
                log_disposition("Undo", &self.id, self.actor.inner(), &disposition);
            }
            UndoObject::Like(like) => {
                let actor = self.actor.dereference(data).await?;
                let applied = inbound::unlike(store, actor.account.id, like.object.uri().as_str(), now)?;
                log_outcome("Undo", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
            UndoObject::Announce(announce) => {
                let actor = self.actor.dereference(data).await?;
                let applied = inbound::unannounce(store, actor.account.id, announce.object.uri().as_str(), now)?;
                log_outcome("Undo", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
            UndoObject::Block(block) => {
                let actor = self.actor.dereference(data).await?;
                let target = ObjectId::<ActorRecord>::from(block.object.clone()).dereference(data).await?;
                let applied = inbound::unblock(store, actor.account.id, target.account.id, now)?;
                log_outcome("Undo", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Create, Update, Delete: Notes and profiles.
// ---------------------------------------------------------------------------

#[async_trait]
impl ActivityHandler for Create {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        // The Note's own checks run on receipt, after its author is known.
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let author = self.actor.dereference(data).await?;
        match self.object {
            CreateObject::Question(question) => {
                verify_object_author(&question.id, &question.attributed_to, self.actor.inner())?;
                let (note, poll) = question_input_for(&question, author.account.id, author.account.sensitized, now);
                let applied = match tombstone::ingest_checked(store, note, Some(poll), now, data.aperture_mode)? {
                    CheckedIngest::Live(ingested) => {
                        if let Some(parent_uri) = &ingested.parent_unknown {
                            crate::federation::threads::schedule_parent(store, ingested.status.id, parent_uri, now)?;
                        }
                        if ingested.created { Applied::Applied } else { Applied::Duplicate }
                    }
                    // The Delete won the race: the note stays gone.
                    CheckedIngest::Tombstoned => Applied::Duplicate,
                    // Aperture: accepted transport, no store, no record.
                    CheckedIngest::Dropped => {
                        let dropped =
                            Applied::Ignored("aperture: text-only top-level dropped".into());
                        log_outcome("Create", &self.id, self.actor.inner(), &dropped);
                        return Ok(());
                    }
                };
                log_outcome("Create", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
                Ok(())
            }
            CreateObject::Note(note) => {
                verify_note_author(&note, self.actor.inner())?;
                // A vote-shaped Note tallies a vote on a known local poll instead
                // of becoming a status. Anything else falls through to ingest.
                if let Some(vote) = vote_of(&note) {
                    match inbound::vote(store, author.account.id, &vote.poll_uri, &vote.choice_name, now)? {
                        inbound::VoteDisposition::Tallied(applied) => {
                            log_outcome("Create", &self.id, self.actor.inner(), &applied);
                            record(store, &self.id, &applied, now)?;
                            return Ok(());
                        }
                        inbound::VoteDisposition::NotAVote => {}
                    }
                }
                let ingested = match tombstone::ingest_checked(store, note_input_for(&note, author.account.id, &author.account.domain, author.account.sensitized), None, now, data.aperture_mode)? {
                    CheckedIngest::Live(ingested) => ingested,
                    // The Delete won the race: the note stays gone.
                    CheckedIngest::Tombstoned => {
                        let tombstoned = Applied::Duplicate;
                        log_outcome("Create", &self.id, self.actor.inner(), &tombstoned);
                        record(store, &self.id, &tombstoned, now)?;
                        return Ok(());
                    }
                    // Aperture: accepted transport, no store, no record.
                    CheckedIngest::Dropped => {
                        let dropped =
                            Applied::Ignored("aperture: text-only top-level dropped".into());
                        log_outcome("Create", &self.id, self.actor.inner(), &dropped);
                        return Ok(());
                    }
                };
                if let Some(parent_uri) = &ingested.parent_unknown {
                    crate::federation::threads::schedule_parent(store, ingested.status.id, parent_uri, now)?;
                }
                let applied = if ingested.created { Applied::Applied } else { Applied::Duplicate };
                log_outcome("Create", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
                Ok(())
            }
            CreateObject::Id(url) => {
                // Dereferenced, verified and stored by the crate through `NoteRef`.
                let stored = ObjectId::<NoteRef>::from(url).dereference(data).await?;
                tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "Create", activity = %self.id, status = %stored.status.uri, "note fetched");
                record(store, &self.id, &Applied::Applied, now)?;
                Ok(())
            }
        }
    }
}

#[async_trait]
impl ActivityHandler for Update {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let author = self.actor.dereference(data).await?;
        match self.object.get("type").and_then(|t| t.as_str()) {
            Some("Note") => {
                let note: Note = serde_json::from_value(self.object.clone())
                    .map_err(|e| FedError::Refused(format!("update of a note that does not parse: {e}")))?;
                verify_note_author(&note, self.actor.inner())?;
                let applied = inbound::update_note(store, author.account.id, &note_input_for(&note, author.account.id, &author.account.domain, author.account.sensitized), now)?;
                log_outcome("Update", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
            Some("Person") => {
                // A profile change: refetch the actor, which upserts its fields and key.
                let refreshed = ObjectId::<ActorRecord>::from(self.actor.inner().clone())
                    .dereference_forced(data)
                    .await?;
                tracing::info!(target: "birchtree::federation", direction = "inbound", kind = "Update", activity = %self.id, actor = %refreshed.account.actor_uri, outcome = "applied: profile refreshed", "activity handled");
                record(store, &self.id, &Applied::Applied, now)?;
            }
            Some("Question") => {
                let question: Question = serde_json::from_value(self.object.clone())
                    .map_err(|e| FedError::Refused(format!("update of a question that does not parse: {e}")))?;
                verify_object_author(&question.id, &question.attributed_to, self.actor.inner())?;
                let author = self.actor.dereference(data).await?;
                let (note, poll) = question_input_for(&question, author.account.id, author.account.sensitized, now);
                // Out-of-order delivery: an Update for a status never seen is
                // ingested as its Create (Mastodon does the same), unless its
                // Delete arrived first (then it stays gone).
                let applied = match store.status_by_uri(&note.uri)? {
                    Some(_) => inbound::sync_question(store, author.account.id, &note.uri, &poll, now)?,
                    None => match tombstone::ingest_checked(store, note, Some(poll), now, data.aperture_mode)? {
                        CheckedIngest::Live(ingested) => {
                            if let Some(parent_uri) = &ingested.parent_unknown {
                                crate::federation::threads::schedule_parent(store, ingested.status.id, parent_uri, now)?;
                            }
                            if ingested.created { Applied::Applied } else { Applied::Duplicate }
                        }
                        CheckedIngest::Tombstoned => Applied::Duplicate,
                        // Aperture: accepted transport, no store, no record.
                        CheckedIngest::Dropped => {
                            let dropped =
                                Applied::Ignored("aperture: text-only top-level dropped".into());
                            log_outcome("Update", &self.id, self.actor.inner(), &dropped);
                            return Ok(());
                        }
                    },
                };
                log_outcome("Update", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
            other => {
                return Err(FedError::Unsupported(format!("update of {other:?} is not in the profile")));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ActivityHandler for Delete {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let author = self.actor.dereference(data).await?;
        let object_uri = match &self.object {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Object(o) => o
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| FedError::Refused("delete object has no id".into()))?,
            _ => return Err(FedError::Refused("delete object is not a reference".into())),
        };
        if let Some(quote) = store.quote_by_approval_uri(&object_uri)? {
            // Delete of a QuoteAuthorization: the quoted author revokes the quote.
            let applied = inbound::quote_revoked(store, author.account.id, &object_uri)?;
            log_outcome("Delete", &self.id, self.actor.inner(), &applied);
            record(store, &self.id, &applied, now)?;
            let _ = quote;
            return Ok(());
        }
        if object_uri == self.actor.inner().as_str() {
            // The account itself is deleted: its statuses go with it.
            let applied = inbound::delete_account(store, author.account.id, now)?;
            log_outcome("Delete", &self.id, self.actor.inner(), &applied);
            record(store, &self.id, &applied, now)?;
            return Ok(());
        }
        let applied = inbound::delete_note(store, author.account.id, &object_uri, now)?;
        // Delete-beats-Create: a Delete for an object never seen here files a
        // tombstone (same-host only), so a late Create cannot resurrect it.
        let applied = tombstone::file_remote_tombstone(store, &object_uri, self.actor.inner(), applied, now)?;
        log_outcome("Delete", &self.id, self.actor.inner(), &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Like, Announce, Block, Flag, QuoteRequest.
// ---------------------------------------------------------------------------

#[async_trait]
impl ActivityHandler for Like {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let actor = self.actor.dereference(data).await?;
        let applied = inbound::like(store, actor.account.id, self.object.uri().as_str(), now)?;
        log_outcome("Like", &self.id, self.actor.inner(), &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

#[async_trait]
impl ActivityHandler for Announce {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let actor = self.actor.dereference(data).await?;
        // The boosted status may be unknown here: fetch and store it first.
        let object_uri = self.object.uri().clone();
        if store.status_by_uri(object_uri.as_str())?.is_none() {
            let _ = ObjectId::<NoteRef>::from(object_uri.clone()).dereference(data).await;
        }
        let applied = inbound::announce(
            store,
            actor.account.id,
            object_uri.as_str(),
            audience_visibility(
                &self.to,
                &self.cc,
                &crate::activitypub::wire::followers_uri_of(self.actor.inner()),
            ),
            now,
        )?;
        log_outcome("Announce", &self.id, self.actor.inner(), &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

#[async_trait]
impl ActivityHandler for Block {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let actor = self.actor.dereference(data).await?;
        let target = ObjectId::<ActorRecord>::from(self.object.clone()).dereference(data).await?;
        if !target.account.local {
            let applied = Applied::Ignored("block of an account that is not local".into());
            log_outcome("Block", &self.id, self.actor.inner(), &applied);
            return Ok(());
        }
        let applied = inbound::block(store, actor.account.id, target.account.id, now)?;
        log_outcome("Block", &self.id, self.actor.inner(), &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

#[async_trait]
impl ActivityHandler for Flag {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let actor = self.actor.dereference(data).await?;
        let uris: Vec<String> = self.object.iter().map(|r| r.uri().to_string()).collect();
        let comment = self.content.clone().unwrap_or_default();
        let applied = inbound::flag(store, actor.account.id, &uris, &comment, self.id.as_str(), now)?;
        log_outcome("Flag", &self.id, self.actor.inner(), &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

#[async_trait]
impl ActivityHandler for QuoteRequest {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        &self.actor
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let author = ObjectId::<ActorRecord>::from(self.actor.clone()).dereference(data).await?;
        // The quoting status arrives inline or by URI: store it, then decide.
        // A quoting status whose Delete arrived first stays gone.
        let quoting = match &self.instrument {
            crate::activitypub::wire::Instrument::Note(note) => {
                verify_note_author(note, &self.actor)?;
                match tombstone::ingest_checked(store, note_input_for(note, author.account.id, &author.account.domain, author.account.sensitized), None, now, data.aperture_mode)? {
                    CheckedIngest::Live(ingested) => ingested.status.id,
                    CheckedIngest::Tombstoned => {
                        let tombstoned = Applied::Ignored("quoting status was deleted before it arrived".into());
                        log_outcome("QuoteRequest", &self.id, &self.actor, &tombstoned);
                        return Ok(());
                    }
                    // Aperture: quoting status dropped — nothing to decide.
                    CheckedIngest::Dropped => {
                        let dropped =
                            Applied::Ignored("aperture: quoting status dropped".into());
                        log_outcome("QuoteRequest", &self.id, &self.actor, &dropped);
                        return Ok(());
                    }
                }
            }
            crate::activitypub::wire::Instrument::Question(question) => {
                verify_object_author(&question.id, &question.attributed_to, &self.actor)?;
                let (note, poll) = question_input_for(question, author.account.id, author.account.sensitized, now);
                match tombstone::ingest_checked(store, note, Some(poll), now, data.aperture_mode)? {
                    CheckedIngest::Live(ingested) => ingested.status.id,
                    CheckedIngest::Tombstoned => {
                        let tombstoned = Applied::Ignored("quoting status was deleted before it arrived".into());
                        log_outcome("QuoteRequest", &self.id, &self.actor, &tombstoned);
                        return Ok(());
                    }
                    // Aperture: quoting status dropped — nothing to decide.
                    CheckedIngest::Dropped => {
                        let dropped =
                            Applied::Ignored("aperture: quoting status dropped".into());
                        log_outcome("QuoteRequest", &self.id, &self.actor, &dropped);
                        return Ok(());
                    }
                }
            }
            crate::activitypub::wire::Instrument::Id(url) => {
                ObjectId::<NoteRef>::from(url.clone()).dereference(data).await?.status.id
            }
        };
        let applied = inbound::quote_request(store, quoting, self.object.as_str(), self.id.as_str(), now)?;
        log_outcome("QuoteRequest", &self.id, &self.actor, &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

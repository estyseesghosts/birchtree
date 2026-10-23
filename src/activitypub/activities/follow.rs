use activitypub_federation::{
    config::Data,
    fetch::object_id::ObjectId,
    kinds::activity::{AcceptType, FollowType, RejectType, UndoType},
    protocol::context::WithContext,
    traits::ActivityHandler,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use super::envelope::Activity;
use super::inputs::{log_disposition, log_outcome, record};
use super::social::{Announce, Block, Like};
use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::activitypub::error::FedError;
use crate::activitypub::wire::QuoteRequest;
use crate::application::federation::{
    InboundAccept, InboundFollow, InboundReject, InboundUndoFollow, receive_accept, receive_follow,
    receive_reject, receive_undo_follow,
};
use crate::application::federation_inbound::{self as inbound};

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
            return Err(FedError::Refused(
                "follow object is not a local account".into(),
            ));
        }
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let target = self.object.dereference(data).await?;
        let follower = self.actor.dereference(data).await?;
        let accept = accept_for_follow(&self, &target);
        let accept_body =
            serde_json::to_value(WithContext::new_default(Activity::Accept(accept.clone())))
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
pub(super) fn accept_for_follow(follow: &Follow, target: &ActorRecord) -> Accept {
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
            Some(followed) if followed != self.actor.inner() => Err(FedError::Refused(
                "accept names a request of another account".into(),
            )),
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
            Some(followed) if followed != self.actor.inner() => Err(FedError::Refused(
                "reject names a request of another account".into(),
            )),
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
            return Err(FedError::Refused(
                "undo names an activity by another actor".into(),
            ));
        }
        if let UndoObject::Follow(f) = &self.object {
            if !f.object.is_local(data) {
                return Err(FedError::Refused(
                    "undone follow is not of a local account".into(),
                ));
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
                let applied =
                    inbound::unlike(store, actor.account.id, like.object.uri().as_str(), now)?;
                log_outcome("Undo", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
            UndoObject::Announce(announce) => {
                let actor = self.actor.dereference(data).await?;
                let applied = inbound::unannounce(
                    store,
                    actor.account.id,
                    announce.object.uri().as_str(),
                    now,
                )?;
                log_outcome("Undo", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
            UndoObject::Block(block) => {
                let actor = self.actor.dereference(data).await?;
                let target = ObjectId::<ActorRecord>::from(block.object.clone())
                    .dereference(data)
                    .await?;
                let applied = inbound::unblock(store, actor.account.id, target.account.id, now)?;
                log_outcome("Undo", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
        }
        Ok(())
    }
}

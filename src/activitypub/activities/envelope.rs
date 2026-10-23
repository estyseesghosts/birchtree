use activitypub_federation::{config::Data, traits::ActivityHandler};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use super::follow::{Accept, Follow, Reject, Undo};
use super::move_::Move;
use super::social::{Announce, Block, Flag, Like, QuoteRequestActivity};
use super::statuses::{Create, Delete, Update};
use crate::activitypub::FedContext;
use crate::activitypub::error::FedError;

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
    Move(Move),
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
            Activity::Move(a) => &a.id,
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
            Activity::Move(a) => a.$method($($arg),*).await,
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
            Activity::Move(a) => a.actor.inner(),
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
            Activity::Move(a) => a.receive(data).await,
        }
    }
}

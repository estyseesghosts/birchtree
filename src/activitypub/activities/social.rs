use activitypub_federation::{
    config::Data,
    fetch::object_id::ObjectId,
    kinds::activity::{AnnounceType, BlockType, FlagType, LikeType},
    traits::ActivityHandler,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use super::inputs::{
    audience_visibility, log_outcome, note_input_for, question_input_for, record,
    verify_note_author, verify_object_author,
};
use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::activitypub::error::FedError;
use crate::activitypub::objects::NoteRef;
use crate::activitypub::wire::{ObjectRef, QuoteRequest};
use crate::application::federation_inbound::{self as inbound, Applied};
use crate::federation::tombstone::{self, CheckedIngest};

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
    #[serde(
        default,
        deserialize_with = "crate::activitypub::wire::one_or_many_urls"
    )]
    pub to: Vec<Url>,
    #[serde(
        default,
        deserialize_with = "crate::activitypub::wire::one_or_many_urls"
    )]
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
            let _ = ObjectId::<NoteRef>::from(object_uri.clone())
                .dereference(data)
                .await;
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
        let target = ObjectId::<ActorRecord>::from(self.object.clone())
            .dereference(data)
            .await?;
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
        let applied = inbound::flag(
            store,
            actor.account.id,
            &uris,
            &comment,
            self.id.as_str(),
            now,
        )?;
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
        let author = ObjectId::<ActorRecord>::from(self.actor.clone())
            .dereference(data)
            .await?;
        // The quoting status arrives inline or by URI: store it, then decide.
        // A quoting status whose Delete arrived first stays gone.
        let quoting = match &self.instrument {
            crate::activitypub::wire::Instrument::Note(note) => {
                verify_note_author(note, &self.actor)?;
                match tombstone::ingest_checked_with_config(
                    store,
                    note_input_for(note, author.account.id, author.account.sensitized),
                    None,
                    now,
                    &data.remote_cache,
                )? {
                    CheckedIngest::Live(ingested) => ingested.status.id,
                    CheckedIngest::Tombstoned => {
                        let tombstoned =
                            Applied::Ignored("quoting status was deleted before it arrived".into());
                        log_outcome("QuoteRequest", &self.id, &self.actor, &tombstoned);
                        return Ok(());
                    }
                }
            }
            crate::activitypub::wire::Instrument::Question(question) => {
                verify_object_author(&question.id, &question.attributed_to, &self.actor)?;
                let (note, poll) =
                    question_input_for(question, author.account.id, author.account.sensitized, now);
                match tombstone::ingest_checked_with_config(
                    store,
                    note,
                    Some(poll),
                    now,
                    &data.remote_cache,
                )? {
                    CheckedIngest::Live(ingested) => ingested.status.id,
                    CheckedIngest::Tombstoned => {
                        let tombstoned =
                            Applied::Ignored("quoting status was deleted before it arrived".into());
                        log_outcome("QuoteRequest", &self.id, &self.actor, &tombstoned);
                        return Ok(());
                    }
                }
            }
            crate::activitypub::wire::Instrument::Id(url) => {
                ObjectId::<NoteRef>::from(url.clone())
                    .dereference(data)
                    .await?
                    .status
                    .id
            }
        };
        let applied =
            inbound::quote_request(store, quoting, self.object.as_str(), self.id.as_str(), now)?;
        log_outcome("QuoteRequest", &self.id, &self.actor, &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

//! Remote statuses as the crate's `Object`: dereferenced by URI (fetched and
//! verified by the crate, then stored through the Note ingest path), so an
//! announced, quoted or parent status is handled by the same code as a Create.

use activitypub_federation::{
    config::Data,
    protocol::verification::verify_domains_match,
    traits::Object,
};
use async_trait::async_trait;
use url::Url;

use crate::activitypub::actor::ActorRecord;
use crate::activitypub::activities::{note_input_for, question_input_for};
use crate::activitypub::error::FedError;
use crate::activitypub::outgoing;
use crate::activitypub::wire::NoteOrQuestion;
use crate::activitypub::FedContext;
use crate::application::federation_inbound::ingest_note;
use crate::domain::Status;

/// A status known here (local or remote), as the crate's object.
#[derive(Debug, Clone)]
pub struct NoteRef {
    pub status: Status,
}

#[async_trait]
impl Object for NoteRef {
    type DataType = FedContext;
    type Kind = NoteOrQuestion;
    type Error = FedError;

    async fn read_from_id(object_id: Url, data: &Data<FedContext>) -> Result<Option<Self>, FedError> {
        let store = &*data.app.store;
        Ok(store.status_by_uri(object_id.as_str())?.map(|status| NoteRef { status }))
    }

    async fn into_json(self, data: &Data<FedContext>) -> Result<NoteOrQuestion, FedError> {
        let store = &*data.app.store;
        let now = data.app.now_ms();
        let poll = store.poll_by_status(self.status.id)?;
        match poll {
            Some(poll) => {
                let value = outgoing::question_for_status(store, &data.public_url, &self.status, &poll, now, &data.license_policy, &data.place_policy)?;
                serde_json::from_value(value)
                    .map(|q| NoteOrQuestion::Question(Box::new(q)))
                    .map_err(|e| FedError::Refused(format!("question does not serialize: {e}")))
            }
            None => {
                let value = outgoing::note_for_status(store, &data.public_url, &self.status, now, &data.license_policy, &data.place_policy)?;
                serde_json::from_value(value)
                    .map(|n| NoteOrQuestion::Note(Box::new(n)))
                    .map_err(|e| FedError::Refused(format!("note does not serialize: {e}")))
            }
        }
    }

    async fn verify(json: &NoteOrQuestion, expected_domain: &Url, _data: &Data<FedContext>) -> Result<(), FedError> {
        match json {
            NoteOrQuestion::Note(note) => {
                verify_domains_match(&note.id, expected_domain)?;
                verify_domains_match(&note.id, &note.attributed_to)?;
            }
            NoteOrQuestion::Question(question) => {
                verify_domains_match(&question.id, expected_domain)?;
                verify_domains_match(&question.id, &question.attributed_to)?;
            }
        }
        Ok(())
    }

    async fn from_json(json: NoteOrQuestion, data: &Data<FedContext>) -> Result<Self, FedError> {
        let now = data.app.now_ms();
        match json {
            NoteOrQuestion::Note(note) => {
                // A dereferenced object whose Delete arrived first stays gone:
                // report it deleted (terminal for the fetcher) instead of
                // resurrecting it.
                if data.app.store.remote_tombstone(note.id.as_str())?.is_some() {
                    return Err(FedError::Protocol(activitypub_federation::error::Error::ObjectDeleted(
                        note.id.clone(),
                    )));
                }
                let author = crate::activitypub::objects::author_of(data, &note.attributed_to).await?;
                let account = data
                    .app
                    .store
                    .account_by_id(author)?
                    .ok_or_else(|| FedError::Refused("note author has no account row".into()))?;
                let input = note_input_for(&note, author, &account.domain, account.sensitized);
                let ingested = ingest_note(&*data.app.store, input, None, now)?;
                Ok(NoteRef { status: ingested.status })
            }
            NoteOrQuestion::Question(question) => {
                if data.app.store.remote_tombstone(question.id.as_str())?.is_some() {
                    return Err(FedError::Protocol(activitypub_federation::error::Error::ObjectDeleted(
                        question.id.clone(),
                    )));
                }
                let author = crate::activitypub::objects::author_of(data, &question.attributed_to).await?;
                let sensitized = data
                    .app
                    .store
                    .account_by_id(author)?
                    .is_some_and(|account| account.sensitized);
                let (input, poll) = question_input_for(&question, author, sensitized, now);
                let ingested = ingest_note(&*data.app.store, input, Some(poll), now)?;
                Ok(NoteRef { status: ingested.status })
            }
        }
    }
}

/// The local account id of a remote author, dereferencing (and storing) the actor if needed.
pub async fn author_of(data: &Data<FedContext>, actor: &Url) -> Result<crate::domain::Id, FedError> {
    let account = activitypub_federation::fetch::object_id::ObjectId::<ActorRecord>::from(actor.clone())
        .dereference(data)
        .await?;
    Ok(account.account.id)
}

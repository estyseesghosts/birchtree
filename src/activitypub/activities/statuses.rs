use activitypub_federation::{
    config::Data,
    fetch::object_id::ObjectId,
    kinds::activity::{CreateType, DeleteType, UpdateType},
    traits::ActivityHandler,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use super::inputs::{
    log_outcome, note_input_for, question_input_for, record, verify_note_author,
    verify_object_author, vote_of,
};
use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::activitypub::error::FedError;
use crate::activitypub::objects::NoteRef;
use crate::activitypub::wire::{Note, Question};
use crate::application::federation_inbound::{self as inbound, Applied};
use crate::federation::tombstone::{self, CheckedIngest};

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
                let (note, poll) = question_input_for(
                    &question,
                    author.account.id,
                    author.account.sensitized,
                    now,
                );
                let applied = match tombstone::ingest_checked_with_config(
                    store,
                    note,
                    Some(poll),
                    now,
                    &data.remote_cache,
                )? {
                    CheckedIngest::Live(ingested) => {
                        if let Some(parent_uri) = &ingested.parent_unknown {
                            crate::federation::threads::schedule_parent(
                                store,
                                ingested.status.id,
                                parent_uri,
                                now,
                            )?;
                        }
                        if ingested.created {
                            Applied::Applied
                        } else {
                            Applied::Duplicate
                        }
                    }
                    // The Delete won the race: the note stays gone.
                    CheckedIngest::Tombstoned => Applied::Duplicate,
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
                    match inbound::vote(
                        store,
                        author.account.id,
                        &vote.poll_uri,
                        &vote.choice_name,
                        now,
                    )? {
                        inbound::VoteDisposition::Tallied(applied) => {
                            log_outcome("Create", &self.id, self.actor.inner(), &applied);
                            record(store, &self.id, &applied, now)?;
                            return Ok(());
                        }
                        inbound::VoteDisposition::NotAVote => {}
                    }
                }
                let ingested = match tombstone::ingest_checked_with_config(
                    store,
                    note_input_for(&note, author.account.id, author.account.sensitized),
                    None,
                    now,
                    &data.remote_cache,
                )? {
                    CheckedIngest::Live(ingested) => ingested,
                    // The Delete won the race: the note stays gone.
                    CheckedIngest::Tombstoned => {
                        let tombstoned = Applied::Duplicate;
                        log_outcome("Create", &self.id, self.actor.inner(), &tombstoned);
                        record(store, &self.id, &tombstoned, now)?;
                        return Ok(());
                    }
                };
                if let Some(parent_uri) = &ingested.parent_unknown {
                    crate::federation::threads::schedule_parent(
                        store,
                        ingested.status.id,
                        parent_uri,
                        now,
                    )?;
                }
                let applied = if ingested.created {
                    Applied::Applied
                } else {
                    Applied::Duplicate
                };
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
                let note: Note = serde_json::from_value(self.object.clone()).map_err(|e| {
                    FedError::Refused(format!("update of a note that does not parse: {e}"))
                })?;
                verify_note_author(&note, self.actor.inner())?;
                let applied = inbound::update_note(
                    store,
                    author.account.id,
                    &note_input_for(&note, author.account.id, author.account.sensitized),
                    now,
                )?;
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
                let question: Question =
                    serde_json::from_value(self.object.clone()).map_err(|e| {
                        FedError::Refused(format!("update of a question that does not parse: {e}"))
                    })?;
                verify_object_author(&question.id, &question.attributed_to, self.actor.inner())?;
                let author = self.actor.dereference(data).await?;
                let (note, poll) = question_input_for(
                    &question,
                    author.account.id,
                    author.account.sensitized,
                    now,
                );
                // Out-of-order delivery: an Update for a status never seen is
                // ingested as its Create (Mastodon does the same), unless its
                // Delete arrived first (then it stays gone).
                let applied = match store.status_by_uri(&note.uri)? {
                    Some(_) => {
                        inbound::sync_question(store, author.account.id, &note.uri, &poll, now)?
                    }
                    None => match tombstone::ingest_checked_with_config(
                        store,
                        note,
                        Some(poll),
                        now,
                        &data.remote_cache,
                    )? {
                        CheckedIngest::Live(ingested) => {
                            if let Some(parent_uri) = &ingested.parent_unknown {
                                crate::federation::threads::schedule_parent(
                                    store,
                                    ingested.status.id,
                                    parent_uri,
                                    now,
                                )?;
                            }
                            if ingested.created {
                                Applied::Applied
                            } else {
                                Applied::Duplicate
                            }
                        }
                        CheckedIngest::Tombstoned => Applied::Duplicate,
                    },
                };
                log_outcome("Update", &self.id, self.actor.inner(), &applied);
                record(store, &self.id, &applied, now)?;
            }
            other => {
                return Err(FedError::Unsupported(format!(
                    "update of {other:?} is not in the profile"
                )));
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
        let applied =
            tombstone::file_remote_tombstone(store, &object_uri, self.actor.inner(), applied, now)?;
        log_outcome("Delete", &self.id, self.actor.inner(), &applied);
        record(store, &self.id, &applied, now)?;
        Ok(())
    }
}

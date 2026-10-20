//! Resolving a remote handle (`user@host`) to an account, for search with
//! `resolve=true`. WebFinger, the document fetch and its verification are the
//! crate's (`webfinger_resolve_actor`); this module reads the result back from
//! the store.

use activitypub_federation::{config::FederationConfig, fetch::webfinger::webfinger_resolve_actor};

use activitypub_federation::fetch::object_id::ObjectId;

use crate::activitypub::actor::ActorRecord;
use crate::activitypub::objects::NoteRef;
use crate::activitypub::error::FedError;
use crate::activitypub::FedContext;
use crate::domain::{Account, Status};

/// Resolve `@user@host` (or `user@host`). `Ok(None)` for a handle on this server,
/// which local search already covers.
pub async fn resolve_handle(config: &FederationConfig<FedContext>, handle: &str) -> Result<Option<Account>, FedError> {
    let identifier = handle.trim_start_matches('@');
    let Some((_, domain)) = identifier.split_once('@') else {
        return Ok(None);
    };
    if domain.eq_ignore_ascii_case(config.domain()) {
        return Ok(None);
    }
    let data = config.to_request_data();
    let actor: ActorRecord = webfinger_resolve_actor(identifier, &data).await?;
    let ctx: &FedContext = config;
    Ok(ctx.app.store.account_by_actor_uri(actor.url().as_str())?)
}

/// Resolve a remote status by its URL, for search with `resolve=true`: the crate fetches the Note,
/// verifies it and stores it through `NoteRef`; a status already stored is returned without a fetch.
pub async fn resolve_status(config: &FederationConfig<FedContext>, url: &str) -> Result<Status, FedError> {
    let url = url::Url::parse(url).map_err(|e| FedError::Refused(format!("not a status url: {e}")))?;
    let data = config.to_request_data();
    let note = ObjectId::<NoteRef>::from(url).dereference(&data).await?;
    Ok(note.status)
}

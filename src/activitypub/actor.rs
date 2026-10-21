//! Actors: the crate's `Object` and `Actor` for birchtree accounts, and the
//! `Person` document that carries them on the wire.
//!
//! One record type, [`ActorRecord`], serves local and remote accounts. The crate
//! dereferences an actor through `read_from_id` (local or cached) and
//! `from_json` (a fetched document, upserted into the store). Its refresh rule
//! reads `last_refreshed_at`, so a rotated key is picked up once the cached copy
//! is stale.

use activitypub_federation::{
    config::{Data, FederationConfig},
    fetch::object_id::ObjectId,
    http_signatures::generate_actor_keypair,
    kinds::actor::PersonType,
    protocol::{public_key::PublicKey, verification::verify_domains_match},
    traits::{Actor, Object},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::activitypub::error::FedError;
use crate::activitypub::FedContext;
use crate::activitypub::key_generation;
use crate::application::accounts::host_from_url;
use crate::application::federation_ports::RemoteAccountUpsert;
use crate::application::ports::Store;
use crate::domain::account::normalize_username;
use crate::domain::federation::main_key_id;
use crate::domain::{Account, ActorKeys, Id};

/// An account with the keys that verify and sign its activities.
#[derive(Debug, Clone)]
pub struct ActorRecord {
    pub account: Account,
    pub keys: ActorKeys,
    /// The actor's canonical URI (`account.actor_uri`, parsed once).
    url: Url,
}

impl ActorRecord {
    pub fn new(account: Account, keys: ActorKeys) -> Result<Self, FedError> {
        let url = Url::parse(&account.actor_uri)
            .map_err(|e| FedError::Refused(format!("stored actor uri is not a URL: {e}")))?;
        Ok(Self { account, keys, url })
    }

    /// The local account with this username, with its keys (generated on first need).
    pub async fn local_by_username(
        data: &Data<FedContext>,
        username: &str,
    ) -> Result<Option<Self>, FedError> {
        let store: &dyn Store = &*data.app.store;
        // Accounts carry the host without a port (`host_from_url`); the crate's domain keeps it.
        let domain = host_from_url(&data.public_url);
        let Some(account) = store.account_by_handle(&normalize_username(username), &domain)? else {
            return Ok(None);
        };
        if !account.local {
            return Ok(None);
        }
        let keys = keys_for_local(data, &account).await?;
        Ok(Some(Self::new(account, keys)?))
    }

    /// The account's `Person` URI, as the crate and remote servers see it.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The store's upsert input for this actor (a remote account, as fetched).
    pub fn to_upsert(&self) -> Result<RemoteAccountUpsert, FedError> {
        let inbox_uri = self
            .account
            .inbox_uri
            .clone()
            .ok_or_else(|| FedError::Refused("remote actor has no inbox".into()))?;
        Ok(RemoteAccountUpsert {
            actor_uri: self.account.actor_uri.clone(),
            username: self.account.username.clone(),
            domain: self.account.domain.clone(),
            display_name: self.account.display_name.clone(),
            note: self.account.note.clone(),
            locked: self.account.locked,
            inbox_uri,
            shared_inbox_uri: self.account.shared_inbox_uri.clone(),
            public_key_pem: self.keys.public_key_pem.clone(),
            key_id: self.keys.key_id.clone(),
        })
    }

    fn inbox_url(&self) -> Url {
        self.account
            .inbox_uri
            .as_deref()
            .and_then(|u| Url::parse(u).ok())
            .unwrap_or_else(|| self.url.join("inbox").unwrap_or_else(|_| self.url.clone()))
    }
}

/// Create a local account's signing key ahead of any remote request. The
/// `user_created` job calls this, so the RSA generation (seconds in a debug build)
/// never sits inside an inbound request that the remote may time out on.
pub async fn ensure_local_keys_for_account(config: &FederationConfig<FedContext>, account_id: Id) -> Result<(), FedError> {
    let ctx: &FedContext = config;
    let Some(account) = ctx.app.store.account_by_id(account_id)? else {
        return Ok(());
    };
    if account.local {
        keys_for_local(&config.to_request_data(), &account).await?;
    }
    Ok(())
}

/// The keys of a local account. Missing keys are generated once (single-flight per
/// account, on a blocking thread: 2048-bit RSA is CPU work) and stored; the stored pair wins.
pub async fn keys_for_local(data: &Data<FedContext>, account: &Account) -> Result<ActorKeys, FedError> {
    let store: &dyn Store = &*data.app.store;
    let key_id = main_key_id(&account.actor_uri);
    key_generation::ensure_keys(store, account.id, &key_id, generate_actor_keypair).await
}

#[async_trait]
impl Object for ActorRecord {
    type DataType = FedContext;
    type Kind = Person;
    type Error = FedError;

    /// Remote actors are refetched once their cached copy is older than the
    /// crate's interval. Local actors are never refetched.
    fn last_refreshed_at(&self) -> Option<DateTime<Utc>> {
        if self.account.local {
            return None;
        }
        // A remote record without a timestamp is treated as the epoch: stale.
        DateTime::from_timestamp_millis(self.keys.refreshed_at_ms.unwrap_or(0) as i64)
    }

    async fn read_from_id(object_id: Url, data: &Data<FedContext>) -> Result<Option<Self>, FedError> {
        let store: &dyn Store = &*data.app.store;
        let Some(account) = store.account_by_actor_uri(object_id.as_str())? else {
            return Ok(None);
        };
        let keys = match store.actor_keys(account.id)? {
            Some(keys) => keys,
            None if account.local => keys_for_local(data, &account).await?,
            // A remote actor without keys is refetched.
            None => return Ok(None),
        };
        Ok(Some(Self::new(account, keys)?))
    }

    async fn into_json(self, data: &Data<FedContext>) -> Result<Person, FedError> {
        let shared_inbox = if self.account.local {
            Some(Url::parse(&format!("{}/inbox", data.public_url))
                .map_err(|e| FedError::Refused(e.to_string()))?)
        } else {
            self.account.shared_inbox_uri.as_deref().and_then(|u| Url::parse(u).ok())
        };
        // Avatar/header attachments resolve against this server's file route
        // (the same URLs the REST serializers emit). A missing blob reference
        // omits the attachment rather than serving a dead link.
        let icon = self.account.avatar_key.as_deref().filter(|key| !key.is_empty()).and_then(|key| {
            Url::parse(&format!(
                "{}/system/media_attachments/files/{key}",
                data.public_url.trim_end_matches('/')
            ))
            .ok()
            .map(|url| {
                PersonImage::new(
                    url,
                    self.account.avatar_content_type.clone().unwrap_or_else(|| "application/octet-stream".to_string()),
                    &self.account.avatar_description,
                )
            })
        });
        let image = self.account.header_key.as_deref().filter(|key| !key.is_empty()).and_then(|key| {
            Url::parse(&format!(
                "{}/system/media_attachments/files/{key}",
                data.public_url.trim_end_matches('/')
            ))
            .ok()
            .map(|url| {
                PersonImage::new(
                    url,
                    self.account.header_content_type.clone().unwrap_or_else(|| "application/octet-stream".to_string()),
                    &self.account.header_description,
                )
            })
        });
        Ok(Person {
            kind: Default::default(),
            id: self.url.clone().into(),
            preferred_username: self.account.username.clone(),
            name: non_empty(&self.account.display_name),
            summary: non_empty(&self.account.note),
            inbox: self.inbox_url(),
            endpoints: shared_inbox.map(|shared_inbox| Endpoints { shared_inbox }),
            public_key: self.public_key(),
            manually_approves_followers: self.account.locked.then_some(true),
            followers: self.account.local.then(|| Url::parse(&format!("{}/followers", self.account.actor_uri)).ok()).flatten(),
            following: self.account.local.then(|| Url::parse(&format!("{}/following", self.account.actor_uri)).ok()).flatten(),
            outbox: self.account.local.then(|| Url::parse(&format!("{}/outbox", self.account.actor_uri)).ok()).flatten(),
            // G-D: `None` until the move flow records a successor via
            // `set_moved_to`; then this renders `{successor_actor_uri}` so
            // verifiers can check the link-back (D32a).
            also_known_as: None,
            // Local rows serve `{actor}/collections/featured`; remotes carry
            // none (same convention as followers/following/outbox above).
            featured_tags: self.account.local.then(|| {
                Url::parse(&crate::activitypub::objects::featured_tags_collection_uri(
                    &self.account.actor_uri,
                ))
                .ok()
            }).flatten(),
            icon,
            image,
        })
    }

    /// The document must be served from the host it names, and its key must
    /// belong to the actor it is published under.
    async fn verify(json: &Person, expected_domain: &Url, _data: &Data<FedContext>) -> Result<(), FedError> {
        verify_domains_match(json.id.inner(), expected_domain)?;
        if json.public_key.owner != *json.id.inner() {
            return Err(FedError::Refused("publicKey.owner is not the actor".into()));
        }
        Ok(())
    }

    /// Upsert a fetched remote actor, with its key, in one store transaction.
    ///
    /// The document's `icon`/`image` attachments are intentionally not
    /// ingested (remote avatar caching is out of profile — see the
    /// remote-media-cache plan); the refetch still updates every other
    /// profile field and the key.
    async fn from_json(json: Person, data: &Data<FedContext>) -> Result<Self, FedError> {
        let actor_url = json.id.inner().clone();
        let domain = super::host_of(actor_url.as_str())
            .ok_or_else(|| FedError::Refused("actor url has no host".into()))?;
        let upsert = RemoteAccountUpsert {
            actor_uri: actor_url.to_string(),
            username: json.preferred_username,
            domain,
            display_name: json.name.unwrap_or_default(),
            note: json.summary.unwrap_or_default(),
            locked: json.manually_approves_followers.unwrap_or(false),
            inbox_uri: json.inbox.to_string(),
            shared_inbox_uri: json.endpoints.map(|e| e.shared_inbox.to_string()),
            public_key_pem: json.public_key.public_key_pem,
            key_id: json.public_key.id,
        };
        let store: &dyn Store = &*data.app.store;
        let account = store.upsert_remote(upsert, data.app.now_ms())?;
        let keys = store
            .actor_keys(account.id)?
            .ok_or_else(|| FedError::Refused("remote actor has no key after upsert".into()))?;
        Self::new(account, keys)
    }
}

impl Actor for ActorRecord {
    fn id(&self) -> Url {
        self.url.clone()
    }

    fn public_key_pem(&self) -> &str {
        &self.keys.public_key_pem
    }

    fn private_key_pem(&self) -> Option<String> {
        self.keys.private_key_pem.clone()
    }

    fn inbox(&self) -> Url {
        self.inbox_url()
    }

    fn shared_inbox(&self) -> Option<Url> {
        self.account.shared_inbox_uri.as_deref().and_then(|u| Url::parse(u).ok())
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// The `Person` document birchtree serves and accepts (ActivityStreams, with the
/// Mastodon key and endpoint extensions).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    #[serde(rename = "type")]
    pub kind: PersonType,
    pub id: ObjectId<ActorRecord>,
    pub preferred_username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub inbox: Url,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Endpoints>,
    pub public_key: PublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manually_approves_followers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followers: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub following: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox: Option<Url>,
    /// `alsoKnownAs` aliases (federation tail, G-D): populated on local
    /// rows once the move flow sets a successor (S4); parsed but not yet
    /// link-checked on ingest (recorded as D32a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub also_known_as: Option<Vec<Url>>,
    /// `featuredTags` collection URI, served at `{actor}/collections/featured`
    /// for local rows (federation tail, G-D; `None` for remotes, like the
    /// followers/following/outbox collections).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub featured_tags: Option<Url>,
    /// Avatar (`icon`) and header (`image`) attachments, present only while
    /// the account references a staged blob (upstream `avatar_exists?` /
    /// `header_exists?`). Remote avatar caching is out of profile: remote
    /// rows carry no keys, so their documents omit both, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PersonImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<PersonImage>,
}

/// An `icon`/`image` attachment on a served actor document (upstream
/// `ActivityPub::ImageSerializer`: `type`/`mediaType`/`url`, plus `summary`
/// when the account carries an image description).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonImage {
    #[serde(rename = "type", default = "image_kind")]
    pub kind: String,
    #[serde(default = "octet_stream")]
    pub media_type: String,
    pub url: Url,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

fn image_kind() -> String {
    "Image".to_string()
}

fn octet_stream() -> String {
    "application/octet-stream".to_string()
}

impl PersonImage {
    fn new(url: Url, media_type: String, description: &str) -> Self {
        let summary = (!description.is_empty()).then(|| description.to_string());
        Self {
            kind: "Image".to_string(),
            media_type,
            url,
            summary,
        }
    }
}

/// The `endpoints` object: the shared inbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoints {
    pub shared_inbox: Url,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn person_document_round_trips_with_the_key_and_shared_inbox() {
        let json = serde_json::json!({
            "type": "Person",
            "id": "https://example.social/users/alice",
            "preferredUsername": "alice",
            "inbox": "https://example.social/users/alice/inbox",
            "endpoints": {"sharedInbox": "https://example.social/inbox"},
            "publicKey": {
                "id": "https://example.social/users/alice#main-key",
                "owner": "https://example.social/users/alice",
                "publicKeyPem": "PEM"
            }
        });
        let person: Person = serde_json::from_value(json).expect("parse");
        assert_eq!(person.preferred_username, "alice");
        assert_eq!(person.public_key.id, "https://example.social/users/alice#main-key");
        assert_eq!(
            person.endpoints.map(|e| e.shared_inbox.to_string()).as_deref(),
            Some("https://example.social/inbox")
        );
    }
}

//! The instance actor (`/actor`): the server's own signing identity for signed
//! fetches (`docs/federation-contract.md`, "Signed fetch"). Its key lives in the
//! actor-key table under a reserved ID that the id generator never issues.
//!
//! The crate signs every outbound fetch with this key once it is configured as
//! the signed-fetch actor (`FederationConfigBuilder::signed_fetch_actor`).

use activitypub_federation::{
    config::Data,
    http_signatures::generate_actor_keypair,
    kinds::actor::PersonType,
    traits::{Actor, Object},
};
use async_trait::async_trait;
use url::Url;

use crate::activitypub::actor::Person;
use crate::activitypub::error::FedError;
use crate::activitypub::FedContext;
use crate::application::ports::Store;
use crate::domain::federation::main_key_id;
use crate::domain::Id;

/// The reserved account ID of the instance actor's keys (never issued by the id generator).
pub const INSTANCE_KEY_ID: u64 = 0;

/// The instance actor: its ID and key pair.
#[derive(Debug, Clone)]
pub struct InstanceActor {
    pub id: Url,
    pub public_key_pem: String,
    pub private_key_pem: String,
}

/// Load the instance actor's keys, generating them on first start.
pub async fn ensure(store: &dyn Store, public_url: &str) -> Result<InstanceActor, String> {
    let id = Url::parse(&format!("{}/actor", public_url.trim_end_matches('/'))).map_err(|e| e.to_string())?;
    let keys = match store.actor_keys(Id::new(INSTANCE_KEY_ID)).map_err(|e| e.to_string())? {
        Some(keys) if keys.private_key_pem.is_some() => keys,
        _ => {
            let pair = tokio::task::spawn_blocking(generate_actor_keypair)
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            store
                .store_local_keys(Id::new(INSTANCE_KEY_ID), &main_key_id(id.as_str()), &pair.public_key, &pair.private_key)
                .map_err(|e| e.to_string())?
        }
    };
    Ok(InstanceActor {
        id,
        public_key_pem: keys.public_key_pem,
        private_key_pem: keys.private_key_pem.ok_or("instance key is missing")?,
    })
}

#[async_trait]
impl Object for InstanceActor {
    type DataType = FedContext;
    type Kind = Person;
    type Error = FedError;

    async fn read_from_id(_object_id: Url, _data: &Data<FedContext>) -> Result<Option<Self>, FedError> {
        // The instance actor is never dereferenced as a remote object.
        Ok(None)
    }

    async fn into_json(self, _data: &Data<FedContext>) -> Result<Person, FedError> {
        Err(FedError::Refused("the instance actor is served by its own route".into()))
    }

    async fn verify(_json: &Person, _expected_domain: &Url, _data: &Data<FedContext>) -> Result<(), FedError> {
        Err(FedError::Refused("the instance actor is not a remote object".into()))
    }

    async fn from_json(_json: Person, _data: &Data<FedContext>) -> Result<Self, FedError> {
        Err(FedError::Refused("the instance actor is not a remote object".into()))
    }
}

impl Actor for InstanceActor {
    fn id(&self) -> Url {
        self.id.clone()
    }

    fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }

    fn private_key_pem(&self) -> Option<String> {
        Some(self.private_key_pem.clone())
    }

    fn inbox(&self) -> Url {
        Url::parse(&format!("{}/inbox", self.id.as_str().trim_end_matches("/actor"))).unwrap_or_else(|_| self.id.clone())
    }
}

/// The `/actor` document: an `Application` with its key.
pub fn document(actor: &InstanceActor, host: &str) -> serde_json::Value {
    let _ = PersonType::default();
    serde_json::json!({
        "@context": crate::activitypub::outgoing::context(),
        "id": actor.id.as_str(),
        "type": "Application",
        "preferredUsername": host,
        "inbox": actor.inbox().as_str(),
        "publicKey": {
            "id": format!("{}#main-key", actor.id),
            "owner": actor.id.as_str(),
            "publicKeyPem": actor.public_key_pem,
        },
    })
}

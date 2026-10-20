//! Handles incoming activities, verifying HTTP signatures and other checks
//!
#![doc = include_str!("../../docs/08_receiving_activities.md")]

use crate::{
    config::Data,
    error::Error,
    fetch::object_id::ObjectId,
    http_signatures::{verify_body_hash, verify_signature},
    parse_received_activity,
    reqwest_shim::MAX_INBOX_BODY_SIZE,
    traits::{ActivityHandler, Actor, Object},
};
use axum::{
    async_trait,
    body::Body,
    extract::FromRequest,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use http::{HeaderMap, Method, Uri};
use serde::de::DeserializeOwned;
use tracing::debug;

/// A remote actor fetched more recently than this is not refetched after a signature failure.
const KEY_REFRESH_COOLDOWN_SECS: i64 = 60;

/// Handles incoming activities, verifying HTTP signatures and other checks
pub async fn receive_activity<Activity, ActorT, Datatype>(
    activity_data: ActivityData,
    data: &Data<Datatype>,
) -> Result<(), <Activity as ActivityHandler>::Error>
where
    Activity: ActivityHandler<DataType = Datatype> + DeserializeOwned + Send + 'static,
    ActorT: Object<DataType = Datatype> + Actor + Send + 'static,
    for<'de2> <ActorT as Object>::Kind: serde::Deserialize<'de2>,
    <Activity as ActivityHandler>::Error: From<Error> + From<<ActorT as Object>::Error>,
    <ActorT as Object>::Error: From<Error>,
    Datatype: Clone,
{
    let digest_header = activity_data.headers.get("Digest");
    verify_body_hash(digest_header, &activity_data.body)?;

    let (activity, actor) =
        parse_received_activity::<Activity, ActorT, _>(&activity_data.body, data).await?;

    let verified = verify_signature(
        &activity_data.headers,
        &activity_data.method,
        &activity_data.uri,
        actor.public_key_pem(),
    );
    if let Err(err) = verified {
        // The cached key may be stale (the sender rotated or regenerated it). Fetch the actor once
        // more and check again, as Mastodon does. A copy fetched within KEY_REFRESH_COOLDOWN is
        // already current, so a forged signature cannot make us refetch on every request.
        let recent = actor.last_refreshed_at().is_some_and(|at| {
            chrono::Utc::now().signed_duration_since(at) < chrono::Duration::seconds(KEY_REFRESH_COOLDOWN_SECS)
        });
        if recent {
            return Err(err.into());
        }
        let refreshed = ObjectId::<ActorT>::from(activity.actor().clone())
            .dereference_forced(data)
            .await
            .map_err(|_| err)?;
        verify_signature(
            &activity_data.headers,
            &activity_data.method,
            &activity_data.uri,
            refreshed.public_key_pem(),
        )?;
    }

    debug!("Receiving activity {}", activity.id().to_string());
    activity.verify(data).await?;
    activity.receive(data).await?;
    Ok(())
}

/// Contains all data that is necessary to receive an activity from an HTTP request
#[derive(Debug)]
pub struct ActivityData {
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: Vec<u8>,
}

#[async_trait]
impl<S> FromRequest<S> for ActivityData
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request<Body>, _state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();

        // this wont work if the body is an long running stream
        let bytes = axum::body::to_bytes(body, MAX_INBOX_BODY_SIZE)
            .await
            .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response())?;

        Ok(Self {
            headers: parts.headers,
            method: parts.method,
            uri: parts.uri,
            body: bytes.to_vec(),
        })
    }
}

// TODO: copy tests from actix-web inbox and implement for axum as well

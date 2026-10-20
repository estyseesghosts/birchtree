//! Outbound activities for REST follow and unfollow of a remote account.
//!
//! The adapter builds the activity (its ID and wire JSON) and the application
//! layer commits the state change and its `DeliverActivity` job in one
//! transaction. Signing and sending happen later, in `federation::delivery`.

use activitypub_federation::{config::FederationConfig, kinds::activity::FollowType, protocol::context::WithContext};
use url::Url;

use crate::activitypub::actor::keys_for_local;
use crate::activitypub::activities::{Activity, Follow, Undo, UndoObject};
use crate::activitypub::error::FedError;
use crate::activitypub::FedContext;
use crate::application::federation::{request_remote_follow, undo_remote_follow, OutboundFollow, OutboundUndo};
use crate::domain::{Account, Follow as FollowRow};

/// The inbox a delivery to `account` goes to: its shared inbox if it has one.
fn inbox_of(account: &Account) -> Result<String, FedError> {
    account
        .shared_inbox_uri
        .clone()
        .or_else(|| account.inbox_uri.clone())
        .ok_or_else(|| FedError::Refused(format!("remote account {} has no inbox", account.actor_uri)))
}

fn parse(uri: &str) -> Result<Url, FedError> {
    Url::parse(uri).map_err(|e| FedError::Refused(format!("invalid activity uri {uri}: {e}")))
}

/// Follow a remote account: the pending follow and its `Follow` delivery commit together.
pub async fn send_follow(
    config: &FederationConfig<FedContext>,
    follower: &Account,
    target: &Account,
) -> Result<FollowRow, FedError> {
    let ctx: &FedContext = config;
    keys_for_local(&config.to_request_data(), follower).await?;
    let follow_id = parse(&format!("{}/follows/{}", follower.actor_uri, ctx.app.next_id().value()))?;
    let activity = Activity::Follow(Follow {
        kind: FollowType::default(),
        id: follow_id.clone(),
        actor: parse(&follower.actor_uri)?.into(),
        object: parse(&target.actor_uri)?.into(),
    });
    let body = serde_json::to_value(WithContext::new_default(activity))
        .map_err(|e| FedError::Refused(e.to_string()))?;
    let row = request_remote_follow(
        &*ctx.app.store,
        OutboundFollow {
            follower_id: follower.id,
            followed_id: target.id,
            follow_uri: follow_id.to_string(),
            inbox_uri: inbox_of(target)?,
            body,
        },
        ctx.app.now_ms(),
    )?;
    Ok(row)
}

/// Undo a remote follow. `Ok(None)` when the follow does not exist.
pub async fn send_unfollow(
    config: &FederationConfig<FedContext>,
    follower: &Account,
    target: &Account,
) -> Result<Option<FollowRow>, FedError> {
    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let Some(existing) = store.follow_of(follower.id, target.id)? else {
        return Ok(None);
    };
    let follow_uri = existing
        .uri
        .clone()
        .ok_or_else(|| FedError::Refused("follow to a remote account has no activity uri".into()))?;
    keys_for_local(&config.to_request_data(), follower).await?;
    let undo_id = parse(&format!("{}/undo/{}", follower.actor_uri, ctx.app.next_id().value()))?;
    let activity = Activity::Undo(Undo {
        kind: Default::default(),
        id: undo_id.clone(),
        actor: parse(&follower.actor_uri)?.into(),
        object: UndoObject::Follow(Box::new(Follow {
            kind: FollowType::default(),
            id: parse(&follow_uri)?,
            actor: parse(&follower.actor_uri)?.into(),
            object: parse(&target.actor_uri)?.into(),
        })),
    });
    let body = serde_json::to_value(WithContext::new_default(activity))
        .map_err(|e| FedError::Refused(e.to_string()))?;
    let removed = undo_remote_follow(
        store,
        OutboundUndo {
            follower_id: follower.id,
            followed_id: target.id,
            inbox_uri: inbox_of(target)?,
            undo_uri: undo_id.to_string(),
            body,
        },
        ctx.app.now_ms(),
    )?;
    Ok(removed)
}

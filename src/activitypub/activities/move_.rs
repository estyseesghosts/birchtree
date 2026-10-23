use activitypub_federation::{config::Data, fetch::object_id::ObjectId, traits::ActivityHandler};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use url::Url;

use super::inputs::{log_outcome, record};
use crate::activitypub::FedContext;
use crate::activitypub::actor::ActorRecord;
use crate::activitypub::error::FedError;
use crate::application::federation_inbound::{self as inbound};
use crate::domain::Id;

// ---------------------------------------------------------------------------
// Move (account migration) + migrated follows (federation tail, G-D).
// ---------------------------------------------------------------------------

/// The `type` of a `Move`: a literal string, checked on parse (the vendored
/// crate defines no Move kind, like `QuoteRequestType` above).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MoveType;

impl Serialize for MoveType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("Move")
    }
}

impl<'de> Deserialize<'de> for MoveType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "Move" {
            Ok(MoveType)
        } else {
            Err(serde::de::Error::custom(format!("expected Move, got {s}")))
        }
    }
}

/// A `Move`: the actor (the OLD account) announces it moved to `target` (the
/// NEW account). Upstream (`Move` + `MoveWorker`): `actor == object` (the old
/// profile), `target` names the new profile, and the move is honored only
/// when the new profile's `alsoKnownAs` links back to the old one. Followers
/// of the old account automatically follow the new one.
///
/// The `alsoKnownAs` half cannot be checked yet: the served/fetched `Person`
/// document carries no `alsoKnownAs` field (see the parent-merge snippet in
/// the G-D report). Until that lands, receipt requires `object == actor` and
/// `target != actor`, the target must dereference to a real actor, and the
/// old account must be remote (a local account never moves via the inbox).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    #[serde(rename = "type")]
    pub kind: MoveType,
    pub id: Url,
    pub actor: ObjectId<ActorRecord>,
    pub object: Url,
    pub target: Url,
}

/// How many local followers a `Move` walked.
pub const MIGRATE_FOLLOWERS_CAP: usize = 10_000;

/// The outcome of migrating one account's local followers to its successor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoveOutcome {
    /// Local followers now following the new account.
    pub migrated: usize,
    /// Local followers already following the new account.
    pub already: usize,
    /// Local followers left alone (blocked either way, or the successor itself).
    pub skipped: usize,
}

/// Migrate the LOCAL followers of `old_id` to `new_id` (upstream `MoveWorker`:
/// followers of the old profile follow the new one).
///
/// Only local followers move (remote followers' servers apply their own
/// moves); a follower already following the new account, the new account
/// itself, or a follower blocked by / blocking the new account is left alone.
/// A locked successor keeps a pending request (the `follow` row records it).
/// Idempotent: a redelivered `Move` finds every row in place (`already`).
/// Committed follows are never rolled back by a later federation failure.
pub fn apply_move(
    store: &dyn crate::application::ports::Store,
    old_id: Id,
    new_id: Id,
    now_ms: u64,
) -> Result<(), FedError> {
    apply_move_counted(store, old_id, new_id, now_ms).map(|_| ())
}

/// As [`apply_move`], also reporting what moved.
pub fn apply_move_counted(
    store: &dyn crate::application::ports::Store,
    old_id: Id,
    new_id: Id,
    now_ms: u64,
) -> Result<MoveOutcome, FedError> {
    use crate::application::ports::FollowOptions;
    let mut outcome = MoveOutcome::default();
    if old_id == new_id {
        return Ok(outcome);
    }
    let Some(new_account) = store.account_by_id(new_id)? else {
        return Err(FedError::Refused("move target is unknown".into()));
    };
    let followers = store.followers(old_id, None, MIGRATE_FOLLOWERS_CAP)?;
    for follower_id in followers {
        if follower_id == new_id {
            outcome.skipped += 1;
            continue;
        }
        let Some(follower) = store.account_by_id(follower_id)? else {
            outcome.skipped += 1;
            continue;
        };
        if !follower.local {
            // Remote followers' servers apply their own moves.
            outcome.skipped += 1;
            continue;
        }
        if store.follow_of(follower_id, new_id)?.is_some() {
            outcome.already += 1;
            continue;
        }
        let pair = store.pair_state(new_id, follower_id, now_ms)?;
        if pair.blocking || pair.blocked_by {
            outcome.skipped += 1;
            continue;
        }
        store.follow(
            follower_id,
            new_id,
            FollowOptions {
                show_reblogs: true,
                notify: false,
                languages: None,
            },
            new_account.locked,
            now_ms,
        )?;
        outcome.migrated += 1;
    }
    // The old profile keeps its row with `moved_to` pointing at the successor
    // (upstream keeps the tombstoned profile); the `Move::receive` arm writes
    // the marker right after this returns. Follows move regardless.
    Ok(outcome)
}

#[async_trait]
impl ActivityHandler for Move {
    type DataType = FedContext;
    type Error = FedError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<FedContext>) -> Result<(), FedError> {
        // The move must be about the actor itself, to a different profile.
        if self.object != *self.actor.inner() {
            return Err(FedError::Refused("move object is not the actor".into()));
        }
        if self.target == *self.actor.inner() {
            return Err(FedError::Refused("move target is the actor itself".into()));
        }
        Ok(())
    }

    async fn receive(self, data: &Data<FedContext>) -> Result<(), FedError> {
        let store = data.app.store.as_ref();
        let now = data.app.now_ms();
        let old = self.actor.dereference(data).await?;
        if old.account.local {
            // A local account never moves via the inbox (migration is a
            // settings flow): no effect, and per D33 not recorded as seen.
            let ignored = inbound::Applied::Ignored("move of a local account".into());
            log_outcome("Move", &self.id, self.actor.inner(), &ignored);
            return Ok(());
        }
        // The successor must resolve to a real actor; a forged target that
        // dereferences to nothing fails here, before any follow moves.
        let new_account = ObjectId::<ActorRecord>::from(self.target.clone())
            .dereference(data)
            .await?;
        let outcome = apply_move_counted(store, old.account.id, new_account.account.id, now)?;
        // The old profile keeps its row with `moved_to` pointing at the
        // successor (upstream keeps the tombstoned profile). Idempotent, so
        // a retry after a crash between the moves and this write converges.
        store.set_moved_to(old.account.id, Some(new_account.account.id), now)?;
        // The successor's counters re-sync from its served collections when
        // it is remote (locals are authoritative; the worker skips them).
        if !new_account.account.local {
            store.enqueue(
                crate::domain::JobKind::SyncFollowers,
                serde_json::json!({"account_id": new_account.account.id.value()}),
                now,
            )?;
        }
        tracing::info!(
            target: "birchtree::federation",
            direction = "inbound",
            kind = "Move",
            activity = %self.id,
            old = %old.account.actor_uri,
            new = %new_account.account.actor_uri,
            migrated = outcome.migrated,
            already = outcome.already,
            skipped = outcome.skipped,
            outcome = "applied: follows migrated",
            "activity handled"
        );
        record(store, &self.id, &inbound::Applied::Applied, now)?;
        Ok(())
    }
}

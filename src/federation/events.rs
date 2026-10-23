//! The `FederationEvent` job: a local change becomes one `DeliverActivity` job per
//! destination inbox. The activity JSON is built here from the state at this time,
//! and is then fixed in each delivery job, so a retry sends the same bytes.
//!
//! Recipients follow the activity's audience: followers for public, unlisted and
//! private statuses (none for direct ones); mentioned accounts and the author of
//! the parent of a reply; the target of a like, boost, block, flag or quote request.

use activitypub_federation::config::FederationConfig;
use serde_json::Value;

use crate::activitypub::FedContext;
use crate::domain::{Job, QuoteState};
use crate::federation::delivery::DeliveryError;
use crate::federation::events_build;

/// Run one event: build its activity and enqueue its deliveries.
pub async fn run(
    config: &FederationConfig<FedContext>,
    job: &Job,
) -> std::result::Result<(), DeliveryError> {
    let ctx: &FedContext = config;
    let store = &*ctx.app.store;
    let now = ctx.app.now_ms();
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let quarantine = DeliveryError::Quarantine;
    let head = events_build::load_head(store, job)?;
    let public = ctx.public_url.as_ref();
    let planned: Option<(Vec<Value>, Vec<String>)> = match head.event.as_str() {
        "create" | "update" | "delete" | "profile_update" | "delete_actor" => {
            events_build::build_create_update_delete(
                config,
                store,
                public,
                &head.author,
                &head.event,
                head.status_id,
                now,
                &ctx.license_policy,
                &ctx.place_policy,
            )
            .await?
        }
        "like" | "unlike" => {
            events_build::build_like(store, &head.author, &head.event, head.status_id)?
        }
        "poll_vote" | "question_update" => events_build::build_poll_vote(
            store,
            public,
            &head.author,
            &head.event,
            head.status_id,
            now,
            &ctx.license_policy,
            &ctx.place_policy,
        )?,
        "announce" | "undo_announce" | "block" | "unblock" | "flag" => {
            events_build::build_announce_block_flag(
                store,
                &head.author,
                &head.event,
                head.status_id,
                head.target_id,
                head.announce_uri.as_deref(),
                head.report_id,
            )?
        }
        "quote_request" | "quote_accept" | "quote_reject" | "quote_revoke" => {
            events_build::build_quote(
                store,
                public,
                &head.author,
                &head.event,
                head.quote_id,
                now,
                &ctx.license_policy,
                &ctx.place_policy,
            )?
        }
        other => return Err(quarantine(format!("unknown federation event {other}"))),
    };
    let Some((bodies, inboxes)) = planned else {
        return Ok(());
    };
    events_build::enqueue_relays(store, &ctx.relays, &head, &bodies, now).map_err(retry)?;
    events_build::enqueue_deliveries(store, &head, &bodies, &inboxes, now).map_err(retry)?;
    Ok(())
}

/// Whether a quote state is one a remote should still see an activity for.
pub fn quote_is_live(state: QuoteState) -> bool {
    !matches!(
        state,
        QuoteState::Rejected | QuoteState::Revoked | QuoteState::Deleted
    )
}

//! Fan-out: one committed change reaches every open stream whose gate admits
//! it. The gates are the REST timeline policies (`application::timelines`), so
//! a stream and the REST timeline cannot disagree about a status.
//!
//! Runs on the hub thread. It reads durable state for each decision. A read
//! error skips that delivery and logs it: a failed read never widens access.

use std::iter;
use std::sync::Arc;

use serde_json::Value;

use super::channels::{Locality, StreamKind};
use super::hub::{Connection, Core, Frame, Subscription};
use super::render;
use crate::application::ports::Store;
use crate::application::streaming::StreamEvent;
use crate::application::timelines;
use crate::domain::{Id, Status, Visibility};

/// Apply one committed event to every subscription.
pub(crate) fn process(core: &Core, event: StreamEvent) {
    match event {
        StreamEvent::StatusCreated { status_id } => status_changed(core, status_id, false),
        StreamEvent::StatusEdited { status_id } => status_changed(core, status_id, true),
        StreamEvent::StatusDeleted { status, reblogs } => {
            for deleted in iter::once(&status).chain(reblogs.iter()) {
                delete_everywhere(core, deleted);
            }
        }
        StreamEvent::Notification { notification_id } => notification(core, notification_id),
        // Bump before any `Retract` for the same viewer, so the retraction's
        // frames carry the new epoch.
        StreamEvent::PolicyChanged { viewers } => core.bump_epochs(&viewers),
        StreamEvent::Retract { viewer, statuses } => retract(core, viewer, &statuses),
        StreamEvent::TokenRevoked { token_id } => core.close_token(token_id),
        StreamEvent::Conversation { viewer_id, status_id } => {
            conversation(core, viewer_id, status_id)
        }
    }
}

fn warn(context: &str, err: impl std::fmt::Debug) {
    tracing::warn!(error = ?err, "stream delivery skipped: {context}");
}

/// Whether an author is local. Every account is local until federation (Phase 6).
fn is_local(store: &dyn Store, author: Id) -> bool {
    match store.account_by_id(author) {
        Ok(Some(account)) => account.local,
        _ => false,
    }
}

/// A status a public stream would carry, ignoring the viewer: public, not a
/// reblog, and the locality and media selectors. Used for deletes, which
/// upstream sends to these streams without a per-viewer check.
fn public_selected(store: &dyn Store, status: &Status, locality: Locality, media: bool) -> bool {
    if status.visibility != Visibility::Public || status.is_reblog() {
        return false;
    }
    let locality_ok = match locality {
        Locality::Any => true,
        Locality::Local => is_local(store, status.account_id),
        Locality::Remote => !is_local(store, status.account_id),
    };
    locality_ok && (!media || !status.media_ids.is_empty())
}

/// Create (`update`) or edit (`status.update`) of one status.
fn status_changed(core: &Core, status_id: Id, edited: bool) {
    let store = &*core.store;
    let status = match store.status_by_id(status_id) {
        Ok(Some(status)) if !status.is_deleted() => status,
        Ok(_) => return,
        Err(err) => return warn("status read", err),
    };
    let now = core.clock.now_ms();
    let event = if edited { "status.update" } else { "update" };
    // The anonymous public payload is rendered once per event, then shared.
    let mut anonymous: Option<Value> = None;
    for (conn, sub) in core.subscriptions() {
        let frames = match status_frames(core, now, &sub, &status, edited, event, &mut anonymous) {
            Ok(frames) => frames,
            Err(err) => {
                warn("status gate or render", err);
                continue;
            }
        };
        for (name, payload) in frames {
            deliver_json(core, &conn, &sub, name, &payload);
        }
    }
}

/// The frames one status produces for one subscription.
fn status_frames(
    core: &Core,
    now: u64,
    sub: &Subscription,
    status: &Status,
    edited: bool,
    event: &'static str,
    anonymous: &mut Option<Value>,
) -> Result<Vec<(&'static str, Value)>, String> {
    let store = &*core.store;
    let viewer = sub.viewer;
    let mentioned = edited && status.mentions.contains(&viewer);
    let mut out = Vec::new();
    match &sub.kind {
        StreamKind::User => {
            if timelines::home_includes(store, viewer, status, now).map_err(err)? {
                out.push((event, personal(core, now, viewer, status)?));
            }
            // Upstream's notification channel carries edits to mentioned
            // viewers, and `user` includes that channel when the token may
            // read notifications. A followed mention therefore gets the edit
            // twice, as upstream does.
            if mentioned && sub.notifications_scope {
                out.push(("status.update", personal(core, now, viewer, status)?));
            }
        }
        StreamKind::UserNotification => {
            if mentioned {
                out.push(("status.update", personal(core, now, viewer, status)?));
            }
        }
        StreamKind::Direct => {}
        StreamKind::Public { locality, media } => {
            let selected = public_selected(store, status, *locality, *media);
            if selected && timelines::public_stream_includes(store, Some(viewer), status, now).map_err(err)? {
                out.push((event, public(core, now, viewer, status, anonymous)?));
            }
        }
        StreamKind::Hashtag { tag, local } => {
            let tagged = status.tags.iter().any(|t| t == tag);
            let local_ok = !*local || is_local(store, status.account_id);
            if tagged
                && local_ok
                && timelines::tag_stream_includes(store, Some(viewer), status, now).map_err(err)?
            {
                out.push((event, public(core, now, viewer, status, anonymous)?));
            }
        }
        StreamKind::List { list_id } => {
            if timelines::list_stream_includes(store, viewer, *list_id, status, now).map_err(err)? {
                out.push((event, personal(core, now, viewer, status)?));
            }
        }
    }
    Ok(out)
}

/// Payload rendered for the subscriber (home, list and notifications).
fn personal(core: &Core, now: u64, viewer: Id, status: &Status) -> Result<Value, String> {
    render::status(&core.server, &*core.store, now, Some(viewer), status).map_err(err)
}

/// Anonymous payload plus the subscriber's `filtered` array (public streams).
fn public(
    core: &Core,
    now: u64,
    viewer: Id,
    status: &Status,
    anonymous: &mut Option<Value>,
) -> Result<Value, String> {
    if anonymous.is_none() {
        *anonymous = Some(render::status(&core.server, &*core.store, now, None, status).map_err(err)?);
    }
    let base = anonymous.as_ref().expect("rendered above");
    render::public_status(&*core.store, now, viewer, base, status).map_err(err)
}

/// A status deleted (or its reblog wrapper), for every stream that would have
/// carried it. The id is the payload, as upstream sends it.
fn delete_everywhere(core: &Core, status: &Status) {
    let store = &*core.store;
    let now = core.clock.now_ms();
    let id = Arc::<str>::from(status.id.to_string());
    for (conn, sub) in core.subscriptions() {
        let hit = match &sub.kind {
            StreamKind::User => match timelines::home_includes(store, sub.viewer, status, now) {
                Ok(hit) => hit,
                Err(err) => {
                    warn("delete gate", err);
                    false
                }
            },
            // Upstream sends public and hashtag deletes without a viewer
            // check: the id is the whole payload.
            StreamKind::Public { locality, media } => {
                public_selected(store, status, *locality, *media)
            }
            StreamKind::Hashtag { tag, local } => {
                status.visibility == Visibility::Public
                    && !status.is_reblog()
                    && status.tags.iter().any(|t| t == tag)
                    && (!*local || is_local(store, status.account_id))
            }
            StreamKind::List { list_id } => {
                match timelines::list_stream_includes(store, sub.viewer, *list_id, status, now) {
                    Ok(hit) => hit,
                    Err(err) => {
                        warn("delete list gate", err);
                        false
                    }
                }
            }
            StreamKind::UserNotification | StreamKind::Direct => false,
        };
        if hit {
            core.deliver(&conn, Frame::new("delete", sub.names.clone(), id.clone()));
        }
    }
}

/// A notification row, to the recipient's `user` (with notifications in scope)
/// and `user:notification` streams.
fn notification(core: &Core, notification_id: Id) {
    let store = &*core.store;
    let notification = match store.notification_by_id(notification_id) {
        Ok(Some(notification)) => notification,
        Ok(None) => return,
        Err(err) => return warn("notification read", err),
    };
    let now = core.clock.now_ms();
    let recipient = notification.recipient_id;
    for (conn, sub) in core.subscriptions() {
        if sub.viewer != recipient {
            continue;
        }
        let wanted = match sub.kind {
            StreamKind::User => sub.notifications_scope,
            StreamKind::UserNotification => true,
            _ => false,
        };
        if !wanted {
            continue;
        }
        match render::notification(&core.server, store, now, recipient, &notification) {
            Ok(Some(payload)) => deliver_json(core, &conn, &sub, "notification", &payload),
            Ok(None) => {}
            Err(err) => warn("notification render", err),
        }
    }
}

/// A block or mute removes these statuses from the viewer's home and list
/// streams. Unlike a deletion, the statuses still exist: the ids are sent
/// as upstream's `delete`, and the client removes them.
fn retract(core: &Core, viewer: Id, statuses: &[Status]) {
    for (conn, sub) in core.subscriptions() {
        if sub.viewer != viewer || !matches!(sub.kind, StreamKind::User | StreamKind::List { .. }) {
            continue;
        }
        for status in statuses {
            let id = Arc::<str>::from(status.id.to_string());
            core.deliver(&conn, Frame::new("delete", sub.names.clone(), id));
        }
    }
}

/// A conversation thread changed: deliver a `conversation` event on the
/// `direct` stream of the named viewer (Phase 7, closes D18).
fn conversation(core: &Core, viewer: Id, status_id: Id) {
    let now = core.clock.now_ms();
    let payload = match crate::api::mastodon::endpoints::conversations::render_conversation_event(
        &core.server,
        &*core.store,
        viewer,
        status_id,
        now,
    ) {
        Ok(Some(payload)) => payload,
        Ok(None) => return,
        Err(err) => return warn("conversation render", err),
    };
    for (conn, sub) in core.subscriptions() {
        if sub.viewer == viewer && matches!(sub.kind, StreamKind::Direct) {
            deliver_json(core, &conn, &sub, "conversation", &payload);
        }
    }
}

fn deliver_json(core: &Core, conn: &Arc<Connection>, sub: &Subscription, event: &'static str, payload: &Value) {    match serde_json::to_string(payload) {
        Ok(text) => core.deliver(conn, Frame::new(event, sub.names.clone(), Arc::from(text))),
        Err(err) => warn("payload encode", err),
    }
}

fn err(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

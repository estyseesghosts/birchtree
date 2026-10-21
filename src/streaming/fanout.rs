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
use super::hub::{AnnouncementEvent, Connection, Core, Frame, Subscription};
use super::render;
use crate::api::mastodon::{ServerInfo, dto};
use crate::application::announcements::{
    AnnouncementRecord, reaction_views,
};
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
        StreamEvent::NotificationsMerged { recipient_id } => {
            notifications_merged(core, recipient_id)
        }
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

/// A notification request was accepted: the merge signal, to the
/// recipient's `user` (with notifications in scope) and `user:notification`
/// streams (round 3, area C; closes D20).
///
/// Upstream's `UnfilterNotificationsWorker` publishes
/// `{event: :notifications_merged, payload: '1'}` on the recipient's
/// notifications channel (`app/workers/unfilter_notifications_worker.rb:42`);
/// the streaming server forwards it to both subscriptions. The payload is
/// the raw string `1`, like `delete`'s raw id — not a JSON document.
fn notifications_merged(core: &Core, recipient: Id) {
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
        core.deliver(&conn, Frame::new("notifications_merged", sub.names.clone(), Arc::from("1")));
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

// ---------------------------------------------------------------------------
// Announcements (gaps batch, G-B): `announcement`, `announcement.reaction`
// and `announcement.delete` on every connected `user` stream.
// ---------------------------------------------------------------------------
//
// Upstream publishes these to `timeline:{account.id}` for every active
// account with a live subscription — the `user` stream, never
// `user:notification` (the `:notifications` channel) and never the public /
// hashtag / list / direct streams. Dismissals publish nothing (per-user read
// state). Event names and payload shapes follow
// docs.joinmastodon.org/methods/streaming (`announcement` = Announcement
// JSON; `announcement.reaction` = `{name, count, announcement_id}`;
// `announcement.delete` = bare id string).

/// Apply one committed announcement change to every `user` subscription.
pub(crate) fn process_announcement(core: &Core, event: AnnouncementEvent) {
    match event {
        AnnouncementEvent::Published { announcement_id } => {
            announcement_published(core, announcement_id)
        }
        AnnouncementEvent::Reaction {
            announcement_id,
            name,
        } => announcement_reaction(core, announcement_id, &name),
        AnnouncementEvent::Deleted { announcement_id } => {
            announcement_deleted(core, announcement_id)
        }
    }
}

/// Every `user` subscription, regardless of notifications scope:
/// announcements ride the `user` stream like upstream's `timeline:{id}`
/// channel, not the notifications channel.
fn user_subscriptions(core: &Core) -> Vec<(Arc<Connection>, Subscription)> {
    core.subscriptions()
        .into_iter()
        .filter(|(_, sub)| matches!(sub.kind, StreamKind::User))
        .collect()
}

/// A row was published: the Announcement JSON to every `user` stream,
/// rendered per subscriber (`read` + reaction `me` are per-viewer). A
/// missing or unpublished row sends nothing (a delete raced the publish).
fn announcement_published(core: &Core, announcement_id: Id) {
    let store = &*core.store;
    let row = match store.announcement_by_id(announcement_id) {
        Ok(Some(row)) if row.published => row,
        Ok(_) => return,
        Err(err) => return warn("announcement read", err),
    };
    for (conn, sub) in user_subscriptions(core) {
        match announcement_payload(&core.server, store, sub.viewer, &row) {
            Ok(payload) => deliver_json(core, &conn, &sub, "announcement", &payload),
            Err(err) => warn("announcement render", err),
        }
    }
}

/// A reaction was added or removed: `{name, count, announcement_id}` to
/// every `user` stream, with the count recomputed at delivery (like the
/// upstream worker). A missing row sends nothing. A zero count still sends:
/// it tells clients to drop the reaction.
fn announcement_reaction(core: &Core, announcement_id: Id, name: &str) {
    let store = &*core.store;
    match store.announcement_by_id(announcement_id) {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(err) => return warn("announcement read", err),
    }
    let views = match reaction_views(store, announcement_id, Id::new(0)) {
        Ok(views) => views,
        Err(err) => return warn("announcement reactions", err),
    };
    let (count, custom) = views
        .iter()
        .find(|view| view.name == name)
        .map(|view| (view.count, view.custom_emoji_shortcode.clone()))
        .unwrap_or((0, None));
    let payload = announcement_reaction_payload(&core.server, announcement_id, name, count, custom);
    for (conn, sub) in user_subscriptions(core) {
        deliver_json(core, &conn, &sub, "announcement.reaction", &payload);
    }
}

/// A row was unpublished or destroyed: the bare id string to every `user`
/// stream (the row may be gone, like a status `delete`).
fn announcement_deleted(core: &Core, announcement_id: Id) {
    let id = Arc::<str>::from(announcement_id.to_string());
    for (conn, sub) in user_subscriptions(core) {
        core.deliver(
            &conn,
            Frame::new("announcement.delete", sub.names.clone(), id.clone()),
        );
    }
}

/// The `announcement` payload for `viewer`: the same shape as the REST
/// announcement serializer (`endpoints::announcements::render_announcement`,
/// mirrored here because the hub thread is sync). `read` and reaction `me`
/// are per-viewer; unknown mentions/emoji are skipped, never fabricated.
pub fn announcement_payload(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Id,
    row: &AnnouncementRecord,
) -> Result<Value, String> {
    let read = store
        .announcement_muted(row.id, viewer)
        .map_err(err)?;
    let mut seen: Vec<String> = Vec::new();
    for handle in scan_mention_handles(&row.text) {
        if !seen.iter().any(|h| h == &handle) {
            seen.push(handle);
        }
    }
    let mut mentions = Vec::new();
    for handle in seen {
        let (user, domain) = match handle.split_once('@') {
            Some((u, d)) => (u.to_string(), d.to_lowercase()),
            None => (handle.clone(), server.domain.clone()),
        };
        if domain != server.domain {
            continue;
        }
        match store
            .account_by_handle(&user.to_lowercase(), &server.domain)
            .map_err(err)?
        {
            Some(account) => mentions.push(serde_json::json!({
                "id": account.id.to_string(),
                "username": account.username,
                "url": account.actor_uri,
                "acct": account.acct(),
            })),
            None => {}
        }
    }
    let tags: Vec<Value> = extract_tag_names(&row.text)
        .into_iter()
        .map(|tag| {
            serde_json::json!({
                "name": tag,
                "url": server.asset_url(&format!("tags/{tag}")),
            })
        })
        .collect();
    let mut shortcodes: Vec<String> = Vec::new();
    for code in scan_shortcode_tokens(&row.text) {
        if !shortcodes.iter().any(|c| c == &code) {
            shortcodes.push(code);
        }
    }
    let mut emojis = Vec::new();
    for code in shortcodes {
        match store.emoji_by_shortcode(&code).map_err(err)? {
            Some(emoji) if emoji.listed() => {
                emojis.push(dto::custom_emoji_json(server, &emoji, false))
            }
            _ => {}
        }
    }
    let reactions: Vec<Value> = reaction_views(store, row.id, viewer)
        .map_err(err)?
        .iter()
        .map(|r| {
            let mut object = serde_json::Map::new();
            object.insert("name".into(), Value::String(r.name.clone()));
            object.insert("count".into(), Value::from(r.count));
            object.insert("me".into(), Value::Bool(r.me));
            match &r.custom_emoji_shortcode {
                Some(shortcode) => {
                    let partition =
                        crate::application::emoji::emoji_partition(shortcode);
                    object.insert(
                        "url".into(),
                        Value::String(server.asset_url(&format!(
                            "system/custom_emojis/images/{partition}/original/{shortcode}.png"
                        ))),
                    );
                    object.insert(
                        "static_url".into(),
                        Value::String(server.asset_url(&format!(
                            "system/custom_emojis/images/{partition}/static/{shortcode}.png"
                        ))),
                    );
                }
                None => {
                    object.insert("url".into(), Value::Null);
                    object.insert("static_url".into(), Value::Null);
                }
            }
            Value::Object(object)
        })
        .collect();
    let mut object = serde_json::Map::new();
    object.insert("id".into(), Value::String(row.id.to_string()));
    object.insert("content".into(), Value::String(escape_html(&row.text)));
    object.insert("starts_at".into(), opt_time_ms(row.starts_at_ms));
    object.insert("ends_at".into(), opt_time_ms(row.ends_at_ms));
    object.insert("all_day".into(), Value::Bool(row.all_day));
    object.insert("published_at".into(), opt_time_ms(row.published_at_ms));
    object.insert(
        "updated_at".into(),
        Value::String(iso_ms(row.updated_at_ms)),
    );
    object.insert("read".into(), Value::Bool(read));
    object.insert("mentions".into(), Value::Array(mentions));
    object.insert("statuses".into(), Value::Array(Vec::new()));
    object.insert("tags".into(), Value::Array(tags));
    object.insert("emojis".into(), Value::Array(emojis));
    object.insert("reactions".into(), Value::Array(reactions));
    Ok(Value::Object(object))
}

/// The `announcement.reaction` payload: `{name, count, announcement_id}`
/// (the documented shape), plus `url`/`static_url` for custom emoji (the
/// reaction serializer's conditional pair; null otherwise).
pub fn announcement_reaction_payload(
    server: &ServerInfo,
    announcement_id: Id,
    name: &str,
    count: u64,
    custom_emoji_shortcode: Option<String>,
) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("name".into(), Value::String(name.to_string()));
    object.insert("count".into(), Value::from(count));
    object.insert(
        "announcement_id".into(),
        Value::String(announcement_id.to_string()),
    );
    match custom_emoji_shortcode {
        Some(shortcode) => {
            let partition = crate::application::emoji::emoji_partition(&shortcode);
            object.insert(
                "url".into(),
                Value::String(server.asset_url(&format!(
                    "system/custom_emojis/images/{partition}/original/{shortcode}.png"
                ))),
            );
            object.insert(
                "static_url".into(),
                Value::String(server.asset_url(&format!(
                    "system/custom_emojis/images/{partition}/static/{shortcode}.png"
                ))),
            );
        }
        None => {
            object.insert("url".into(), Value::Null);
            object.insert("static_url".into(), Value::Null);
        }
    }
    Value::Object(object)
}

/// `@user` / `@user@domain` tokens (mirrors the endpoint scanner).
fn scan_mention_handles(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'.')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'@' {
                let mut k = j + 1;
                while k < bytes.len()
                    && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'-' || bytes[k] == b'.')
                {
                    k += 1;
                }
                if k > j + 1 {
                    out.push(text[i + 1..k].to_string());
                    i = k;
                    continue;
                }
            }
            if j > i + 1 {
                out.push(text[i + 1..j].to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// `#tag` tokens, lowercased and de-duplicated (mirrors the endpoint).
fn extract_tag_names(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > i + 1 {
                let tag = text[i + 1..j].to_lowercase();
                if !out.iter().any(|t| t == &tag) {
                    out.push(tag);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// `:shortcode:` tokens (mirrors the endpoint).
fn scan_shortcode_tokens(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' && j > i + 1 {
                out.push(text[i + 1..j].to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn iso_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

fn opt_time_ms(ms: Option<u64>) -> Value {
    ms.map(|t| Value::String(iso_ms(t))).unwrap_or(Value::Null)
}

fn err(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

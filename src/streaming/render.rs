//! Payloads for stream frames, rendered through the REST DTOs so a stream
//! carries exactly what the REST surface carries.
//!
//! Upstream renders two ways. Home, list and notification payloads are
//! rendered for the subscriber. Public and hashtag payloads are rendered
//! anonymously (no per-viewer keys), and a logged-in subscriber gets a
//! `filtered` array added (upstream `StreamListener`).

use serde_json::Value;

use crate::api::mastodon::endpoints::notifications::render_notifications;
use crate::api::mastodon::status_view::load_status_views;
use crate::api::mastodon::{ServerInfo, dto};
use crate::api::mastodon::errors::ApiError;
use crate::application::filters::filter_results_unscoped;
use crate::application::ports::Store;
use crate::domain::{Id, Notification, Status};

/// The status payload for `viewer` (`None` for an anonymous render). Matches
/// the REST status DTO, including the viewer's `filtered` annotation.
pub fn status(
    server: &ServerInfo,
    store: &dyn Store,
    now_ms: u64,
    viewer: Option<Id>,
    status: &Status,
) -> Result<Value, ApiError> {
    let mut views = load_status_views(server, store, viewer, vec![status.clone()], now_ms)?;
    let Some(view) = views.first_mut() else {
        return Err(ApiError::not_found());
    };
    if let Some(viewer) = viewer {
        let matches = filter_results_unscoped(store, viewer, std::slice::from_ref(status), now_ms)?;
        view.filtered = matches.get(&status.id).cloned().unwrap_or_default();
    }
    Ok(dto::status_json(
        server,
        viewer,
        now_ms,
        view,
        dto::StatusSourceMode::Content,
    ))
}

/// An anonymous status payload with the logged-in `filtered` array added
/// (upstream's public and hashtag streams).
pub fn public_status(
    store: &dyn Store,
    now_ms: u64,
    viewer: Id,
    anonymous: &Value,
    status: &Status,
) -> Result<Value, ApiError> {
    let matches = filter_results_unscoped(store, viewer, std::slice::from_ref(status), now_ms)?;
    let mut payload = anonymous.clone();
    let entries = matches.get(&status.id).cloned().unwrap_or_default();
    if let Some(object) = payload.as_object_mut() {
        object.insert("filtered".into(), Value::Array(entries));
    }
    Ok(payload)
}

/// A notification payload for `recipient`. `None` when the actor's row is gone
/// (the REST index omits such rows too).
pub fn notification(
    server: &ServerInfo,
    store: &dyn Store,
    now_ms: u64,
    recipient: Id,
    notification: &Notification,
) -> Result<Option<Value>, ApiError> {
    let (rows, _, _) = render_notifications(
        server,
        store,
        now_ms,
        recipient,
        vec![notification.clone()],
    )?;
    Ok(rows.into_iter().next())
}

//! Mastodon notification DTO packer (S10 split of `api::mastodon::dto`).
//!
//! Known gaps (documented, NOT fixed here): the pinned
//! `REST::NotificationSerializer` also emits `filtered` (v2 filter matches),
//! `fallback` (grouped-notification fallback reference), `report` (the
//! embedded report for `admin.report` types) and `event` (relationship-severance
//! payloads). This server renders the Phase 4 subset
//! `{id, type, created_at, group_key, account, status}`; the missing keys are
//! tracked as parity follow-ups, and adding them belongs to a parity slice,
//! not this move.

use serde_json::{Map, Value, json};

use crate::application::ports::filter_json::rfc3339_ms;
use crate::domain::Notification;

/// `REST::NotificationSerializer`: `{id, type, created_at, group_key,
/// account, status}`. `status` is present exactly for the status-bearing
/// types (`mention`, `favourite`, `reblog`, `poll`, `quote`).
///
/// `group_key` uses the serializer's `ungrouped-<id>` fallback: this server
/// does not merge notifications server-side (grouped reads are the v2 API,
/// out of the Phase 4 profile).
pub fn notification_json(
    notification: &Notification,
    account: Value,
    status: Option<Value>,
) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(notification.id.to_string()));
    object.insert("type".into(), json!(notification.r#type.as_str()));
    object.insert(
        "created_at".into(),
        json!(rfc3339_ms(notification.created_at_ms)),
    );
    object.insert(
        "group_key".into(),
        json!(format!("ungrouped-{}", notification.id)),
    );
    object.insert("account".into(), account);
    object.insert("status".into(), status.unwrap_or(Value::Null));
    Value::Object(object)
}

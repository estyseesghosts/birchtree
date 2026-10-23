//! Mastodon REST DTO packers (S10 split).
//!
//! Leaf modules own one serializer family each; this file keeps the small
//! Phase 4 serializers with no natural leaf home and re-exports every
//! pre-existing `dto::X` path unchanged.

pub mod account;
pub mod admin;
pub mod instance;
pub mod media;
pub mod notification;
pub mod poll;
pub mod status;

use serde_json::{Map, Value, json};

use crate::api::mastodon::ServerInfo;
use crate::application::ports::filter_json::rfc3339_ms;
use crate::domain::{List, Marker, MediaAttachment, ScheduledStatus, TrendLink};

pub use self::account::{
    account_json, account_json_with_stats, admin_trend_tag_json, app_json, avatar_static_url,
    avatar_url, credential_account_json, credential_account_json_with_stats, hashtag_json,
    header_static_url, header_url, preferences_json, relationship_json, role_json, token_json,
    trend_tag_json,
};
pub use self::admin::{
    admin_trend_link_json, admin_trend_publisher_json, annual_report_json, annual_reports_json,
};
pub use self::instance::{InstanceStats, instance_v1_json, instance_v2_json};
pub use self::media::{media_attachment_json, media_attachment_json_with_source, media_file_url};
pub use self::notification::notification_json;
pub use self::poll::{poll_json, poll_json_with_ballot};
pub use self::status::{
    QuoteView, StatusSourceMode, StatusView, status_edit_json, status_json, status_source_json,
};
pub use crate::application::ports::filter_json::filter_json;
pub use crate::application::ports::filter_json::filter_result_json;
pub use crate::application::ports::filter_json::filter_v1_json;

// --- Shim-kept small serializers (verbatim bodies from `dto.rs`) ---

/// `REST::ListSerializer`.
pub fn list_json(list: &List) -> Value {
    json!({
        "id": list.id.to_string(),
        "title": list.title,
        "replies_policy": list.replies_policy.as_str(),
        "exclusive": list.exclusive,
    })
}

/// `REST::MarkerSerializer` (one timeline entry of the markers map).
pub fn marker_json(marker: &Marker) -> Value {
    json!({
        "last_read_id": marker.last_read_id.to_string(),
        "version": marker.lock_version,
        "updated_at": rfc3339_ms(marker.updated_at_ms),
    })
}

/// `REST::ScheduledStatusSerializer`. `params` echoes the replayable
/// creation params; `quoted_status_id` is stringified and
/// `quote_approval_policy` is a string (default `nobody`), as in the pinned
/// serializer.
pub fn scheduled_status_json(
    server: &ServerInfo,
    scheduled: &ScheduledStatus,
    media: &[MediaAttachment],
) -> Value {
    let mut params = scheduled.params.clone();
    if let Some(object) = params.as_object_mut() {
        match object.get("quoted_status_id").cloned() {
            Some(Value::Number(id)) => {
                object.insert("quoted_status_id".into(), json!(id.to_string()));
            }
            Some(Value::Null) | None => {
                object.insert("quoted_status_id".into(), Value::Null);
            }
            Some(other) => {
                object.insert("quoted_status_id".into(), other);
            }
        }
        object
            .entry("quote_approval_policy")
            .or_insert_with(|| json!("nobody"));
    }
    let media_json: Vec<Value> = media
        .iter()
        .map(|attachment| media_attachment_json(server, attachment))
        .collect();
    json!({
        "id": scheduled.id.to_string(),
        "scheduled_at": rfc3339_ms(scheduled.scheduled_at_ms),
        "params": params,
        "media_attachments": media_json,
    })
}

/// `REST::CustomEmojiSerializer`. `category`/`featured` are emitted only for
/// the index endpoint (`with_category`), matching the association-loaded
/// conditional in the pinned serializer; payload embeddings omit them.
///
/// URLs come from the ROW's keys, not the shortcode: a replaced emoji
/// commits fresh keys (transactional replace), and the canonical
/// shortcode-derived path would 404 for the new generation. Fresh rows keep
/// the canonical keys, so their output is unchanged.
pub fn custom_emoji_json(
    server: &ServerInfo,
    emoji: &crate::domain::CustomEmoji,
    with_category: bool,
) -> Value {
    let mut object = Map::new();
    object.insert("shortcode".into(), json!(emoji.shortcode));
    object.insert(
        "url".into(),
        json!(server.asset_url(&format!("system/{}", emoji.image_key))),
    );
    object.insert(
        "static_url".into(),
        json!(server.asset_url(&format!("system/{}", emoji.static_image_key))),
    );
    object.insert("visible_in_picker".into(), json!(emoji.visible_in_picker));
    if with_category {
        object.insert(
            "category".into(),
            emoji
                .category
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        object.insert("featured".into(), json!(false));
    }
    Value::Object(object)
}

/// `REST::SearchSerializer`: all four keys always present as arrays
/// (`collections` is always empty — collection objects are out of profile).
pub fn search_json(accounts: Vec<Value>, statuses: Vec<Value>, hashtags: Vec<Value>) -> Value {
    json!({
        "accounts": accounts,
        "statuses": statuses,
        "hashtags": hashtags,
        "collections": [],
    })
}

/// `REST::Trends::LinkSerializer` for a trend link: the preview-card shape
/// plus `history`. Card metadata stays `null` until the preview-card crawler
/// lands (planned gap); an approved link without fetched metadata is served
/// with `null` card fields, never fabricated content.
pub fn trend_link_json(link: &TrendLink) -> Value {
    let opt = |value: &Option<String>| match value {
        Some(text) => json!(text),
        None => Value::Null,
    };
    json!({
        "url": link.url,
        "title": opt(&link.title),
        "description": opt(&link.description),
        "language": Value::Null,
        "type": "link",
        "author_name": Value::Null,
        "author_url": Value::Null,
        "provider_name": opt(&link.provider_name),
        "provider_url": opt(&link.provider_url),
        "html": Value::Null,
        "width": Value::Null,
        "height": Value::Null,
        "image": opt(&link.image_url),
        "image_description": Value::Null,
        "embed_url": Value::Null,
        "blurhash": Value::Null,
        "published_at": Value::Null,
        "authors": [],
        "history": [],
    })
}

//! Mastodon instance DTO packers (S10 split of `api::mastodon::dto`).
//!
//! `REST::V1::InstanceSerializer` and `REST::InstanceSerializer` (API v2
//! shape) plus their shared `InstanceStats` counters and configuration
//! helpers.

use serde_json::{Value, json};

use crate::api::mastodon::ServerInfo;
use crate::api::mastodon::endpoints::admin_accounts;
use crate::domain::Account;
use crate::media::ingest;

use super::account::{
    account_json, avatar_static_url, avatar_url, date_only, header_static_url, header_url,
};

/// Counts used by the instance serializers.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstanceStats {
    pub user_count: u64,
    pub status_count: u64,
    pub domain_count: u64,
    /// Unique sign-in-log accounts over the trailing 4 weeks (round 3, D31).
    pub active_month: u64,
}

/// `REST::V1::InstanceSerializer`.
pub fn instance_v1_json(
    server: &ServerInfo,
    stats: InstanceStats,
    contact_account: Option<&Account>,
) -> Value {
    let contact = contact_account.map(|a| {
        json!({
            "id": a.id.to_string(),
            "username": a.username,
            "acct": a.acct(),
            "display_name": a.display_name,
            "locked": a.locked,
            "bot": a.bot,
            "discoverable": a.discoverable.map(Value::from).unwrap_or(Value::Null),
            "indexable": a.indexable,
            "group": false,
            "created_at": date_only(a.created_at_ms),
            "note": a.note,
            "url": a.actor_uri,
            "uri": a.actor_uri,
            "avatar": avatar_url(server, a),
            "avatar_static": avatar_static_url(server, a),
            "header": header_url(server, a),
            "header_static": header_static_url(server, a),
            "followers_count": 0,
            "following_count": 0,
            "statuses_count": 0,
            "last_status_at": Value::Null,
            "emojis": [],
            "fields": a.fields.iter().map(|field| json!({
                "name": field.name,
                "value": field.value,
                "verified_at": field.verified_at.map(admin_accounts::iso),
            })).collect::<Vec<_>>(),
        })
    });
    json!({
        "uri": server.domain,
        "title": server.site_title,
        "short_description": server.site_description,
        "description": server.site_description,
        "email": server.site_contact_email,
        "version": version_string(),
        "urls": { "streaming_api": server.streaming_url },
        "stats": {
            "user_count": stats.user_count,
            "status_count": stats.status_count,
            "domain_count": stats.domain_count,
        },
        "thumbnail": server.asset_url("preview.png"),
        "languages": ["en"],
        "registrations": server.registrations_open,
        "approval_required": server.registrations_require_approval,
        "invites_enabled": server.invites_enabled,
        "contact_account": contact,
        "rules": [],
        "configuration": v1_configuration(server),
    })
}

/// `REST::InstanceSerializer` (API v2 shape).
pub fn instance_v2_json(
    server: &ServerInfo,
    stats: InstanceStats,
    contact_account: Option<&Account>,
) -> Value {
    let contact = json!({
        "email": server.site_contact_email,
        "account": contact_account.map(|a| account_json(server, a)),
    });
    json!({
        "domain": server.domain,
        "title": server.site_title,
        "version": version_string(),
        "source_url": "https://github.com/mastodon/mastodon",
        "description": server.site_description,
        "usage": { "users": { "active_month": stats.active_month } },
        "thumbnail": { "url": server.asset_url("preview.png") },
        "icon": [],
        "languages": ["en"],
        "configuration": v2_configuration(server),
        "registrations": {
            "enabled": server.registrations_open,
            "approval_required": server.registrations_require_approval,
            "reason_required": server.registrations_require_approval,
            "message": Value::Null,
            "min_age": Value::Null,
            "url": Value::Null,
        },
        "api_versions": { "mastodon": 11 },
        "wrapstodon": Value::Null,
        "contact": contact,
        "rules": [],
    })
}

fn version_string() -> String {
    "4.7.3 (compatible; birchtree 0.1.0)".to_string()
}

fn v1_configuration(server: &ServerInfo) -> Value {
    json!({
        "accounts": { "max_featured_tags": 10 },
        "statuses": {
            "max_characters": server.max_status_chars,
            "max_media_attachments": 4,
            "characters_reserved_per_url": 23,
        },
        "media_attachments": {
            "supported_mime_types": ingest::ACCEPTED_MIME_TYPES,
            "image_size_limit": 16_777_216,
            "image_matrix_limit": 33_177_600,
            "video_size_limit": 103_809_024,
            "video_frame_rate_limit": 120,
            "video_matrix_limit": 8_294_400,
        },
        "polls": {
            "max_options": 4,
            "max_characters_per_option": 50,
            "min_expiration": 300,
            "max_expiration": 2_592_000,
        },
    })
}

fn v2_configuration(server: &ServerInfo) -> Value {
    json!({
        "urls": {
            "streaming": server.streaming_url,
            "status": Value::Null,
            "about": server.asset_url("about"),
            "privacy_policy": server.asset_url("privacy-policy"),
            "terms_of_service": Value::Null,
        },
        "vapid": { "public_key": server.vapid_public_key },
        "accounts": {
            "max_display_name_length": 40,
            "max_note_length": 500,
            "max_avatar_description_length": 150,
            "max_header_description_length": 150,
            "max_featured_tags": 10,
            "max_pinned_statuses": 5,
            "max_profile_fields": 4,
            "profile_field_name_limit": 255,
            "profile_field_value_limit": 255,
        },
        "statuses": {
            "max_characters": server.max_status_chars,
            "max_media_attachments": 4,
            "characters_reserved_per_url": 23,
        },
        "media_attachments": {
            "description_limit": 10_000,
            "image_matrix_limit": 33_177_600,
            "image_size_limit": 16_777_216,
            "supported_mime_types": ingest::ACCEPTED_MIME_TYPES,
            "video_frame_rate_limit": 120,
            "video_matrix_limit": 8_294_400,
            "video_size_limit": 103_809_024,
        },
        "polls": {
            "max_options": 4,
            "max_characters_per_option": 50,
            "min_expiration": 300,
            "max_expiration": 2_592_000,
        },
        "translation": { "enabled": false },
        "timelines_access": {
            "live_feeds": {
                "local": server.local_live_feed_access,
                "remote": server.remote_live_feed_access,
            },
            "hashtag_feeds": {
                "local": server.local_topic_feed_access,
                "remote": server.remote_topic_feed_access,
            },
            "trending_link_feeds": {
                "local": server.local_topic_feed_access,
                "remote": server.remote_topic_feed_access,
            },
        },
        "limited_federation": false,
    })
}

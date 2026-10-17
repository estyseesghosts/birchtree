//! Mastodon REST DTO packers.
//!
//! Field names, types and nullability follow the pinned Mastodon v4.7.3
//! serializers (`app/serializers/rest/*`). Values the Phase 2 server does not
//! yet store (media URLs, profile fields, roles, custom emoji, counts) are
//! emitted as their contract-correct empty/default shapes, never omitted.
//!
//! Media URLs point at the conventional Mastodon asset paths; media serving is
//! a Phase 4 concern, so these are stable placeholders rather than fabricated
//! object data.

use serde_json::{Map, Value, json};

use crate::api::mastodon::ServerInfo;
use crate::domain::{Account, LocalUser, OAuthApplication};

/// Mastodon's default missing avatar/header asset path.
const MISSING_AVATAR: &str = "avatars/original/missing.png";
const MISSING_HEADER: &str = "headers/original/missing.png";

/// Format a millisecond timestamp as a `YYYY-MM-DD` date (Mastodon uses
/// `created_at.midnight.as_json`).
fn date_only(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.date_naive().to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// Base `REST::AccountSerializer` object.
pub fn account_json(server: &ServerInfo, account: &Account) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(account.id.to_string()));
    object.insert("username".into(), json!(account.username));
    object.insert("acct".into(), json!(account.acct()));
    object.insert("display_name".into(), json!(account.display_name));
    object.insert("locked".into(), json!(account.locked));
    object.insert("bot".into(), json!(false));
    object.insert("discoverable".into(), json!(true));
    object.insert("indexable".into(), json!(false));
    object.insert("group".into(), json!(false));
    object.insert("created_at".into(), json!(date_only(account.created_at_ms)));
    object.insert("note".into(), json!(account.note));
    object.insert("url".into(), json!(account.actor_uri));
    object.insert("uri".into(), json!(account.actor_uri));
    object.insert("avatar".into(), json!(server.asset_url(MISSING_AVATAR)));
    object.insert("avatar_static".into(), json!(server.asset_url(MISSING_AVATAR)));
    object.insert("avatar_description".into(), json!(String::new()));
    object.insert("header".into(), json!(server.asset_url(MISSING_HEADER)));
    object.insert("header_static".into(), json!(server.asset_url(MISSING_HEADER)));
    object.insert("header_description".into(), json!(String::new()));
    object.insert("followers_count".into(), json!(0));
    object.insert("following_count".into(), json!(0));
    object.insert("statuses_count".into(), json!(0));
    object.insert("last_status_at".into(), Value::Null);
    object.insert("hide_collections".into(), Value::Null);
    object.insert("show_media".into(), json!(true));
    object.insert("show_media_replies".into(), json!(true));
    object.insert("show_featured".into(), json!(true));
    object.insert("emojis".into(), json!([]));
    object.insert("fields".into(), json!([]));
    object.insert(
        "feature_approval".into(),
        json!({ "automatic": [], "manual": [], "current_user": "missing" }),
    );
    if account.local {
        object.insert("roles".into(), json!([]));
        object.insert("noindex".into(), json!(false));
    }
    if account.suspended {
        object.insert("suspended".into(), json!(true));
    }
    if let Some(moved_to) = account.moved_to {
        // A full nested account is not available in Phase 2; expose the id-only
        // moving signal rather than fabricating a nested object.
        object.insert("moved".into(), json!({ "id": moved_to.to_string() }));
    }
    Value::Object(object)
}

/// `REST::CredentialAccountSerializer`: the account plus `source` and a single
/// `role` (the pinned serializer adds no `state` attribute).
pub fn credential_account_json(
    server: &ServerInfo,
    account: &Account,
    user: Option<&LocalUser>,
) -> Value {
    let mut object = match account_json(server, account) {
        Value::Object(map) => map,
        other => return other,
    };
    object.insert("source".into(), source_json(account, user));
    object.insert("role".into(), json!({
        "id": "-1",
        "name": "User",
        "permissions": "0",
        "color": "",
        "highlighted": false,
        "collection_limit": 0,
    }));
    Value::Object(object)
}

fn source_json(account: &Account, user: Option<&LocalUser>) -> Value {
    let privacy = user
        .and_then(|u| u.default_privacy.clone())
        .unwrap_or_else(|| if account.locked { "private" } else { "public" }.to_string());
    json!({
        "privacy": privacy,
        "sensitive": user.and_then(|u| u.default_sensitive).unwrap_or(false),
        "language": user.and_then(|u| u.default_language.clone()),
        "note": account.note,
        "fields": [],
        "follow_requests_count": 0,
        "hide_collections": Value::Null,
        "discoverable": true,
        "indexable": false,
        "attribution_domains": [],
        "quote_policy": user
            .and_then(|u| u.default_quote_policy.clone())
            .unwrap_or_else(|| "public".to_string()),
    })
}

/// `REST::RelationshipSerializer` for the viewer's relationship to `id`.
///
/// Phase 2 has no relationship records yet, so every flag is the neutral
/// default; the shape is contract-complete.
pub fn relationship_json(id: crate::domain::Id) -> Value {
    json!({
        "id": id.to_string(),
        "following": false,
        "showing_reblogs": false,
        "notifying": false,
        "languages": Value::Null,
        "followed_by": false,
        "blocking": false,
        "blocked_by": false,
        "muting": false,
        "muting_notifications": false,
        "muting_expires_at": Value::Null,
        "requested": false,
        "requested_by": false,
        "domain_blocking": false,
        "endorsed": false,
        "note": "",
    })
}

/// `REST::ApplicationSerializer` (base). `client_secret` is included only at
/// creation time (Mastodon never returns it from `verify_credentials`).
pub fn app_json(app: &OAuthApplication, client_secret: Option<&str>) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(app.id.to_string()));
    object.insert("name".into(), json!(app.name));
    object.insert("website".into(), json!(app.website));
    object.insert("scopes".into(), json!(app.scopes.join(" ")));
    object.insert("redirect_uris".into(), json!(app.redirect_uris));
    object.insert("redirect_uri".into(), json!(app.redirect_uris.join("\n")));
    object.insert("vapid_key".into(), json!(String::new()));
    if let Some(secret) = client_secret {
        object.insert("client_id".into(), json!(app.client_id));
        object.insert("client_secret".into(), json!(secret));
        object.insert("client_secret_expires_at".into(), json!(0));
    }
    Value::Object(object)
}

/// `PATCH/GET preferences` shape (Mastodon `REST::PreferencesSerializer`).
pub fn preferences_json(account: &Account, user: Option<&LocalUser>) -> Value {
    let visibility = user
        .and_then(|u| u.default_privacy.clone())
        .unwrap_or_else(|| if account.locked { "private" } else { "public" }.to_string());
    json!({
        "posting:default:visibility": visibility,
        "posting:default:sensitive": user.and_then(|u| u.default_sensitive).unwrap_or(false),
        "posting:default:language": user.and_then(|u| u.default_language.clone()),
        "posting:default:quote_policy": user
            .and_then(|u| u.default_quote_policy.clone())
            .unwrap_or_else(|| "public".to_string()),
        "reading:expand:media": "default",
        "reading:expand:spoilers": false,
        "reading:autoplay:gifs": false,
    })
}

/// OAuth token success body (`Doorkeeper::OAuth::TokenResponse`). Mastodon's
/// v4.7.3 config issues non-expiring tokens without refresh tokens, so
/// `expires_in` and `refresh_token` are omitted.
pub fn token_json(plaintext: &str, scopes: &[String], created_at_ms: u64) -> Value {
    json!({
        "access_token": plaintext,
        "token_type": "Bearer",
        "scope": scopes.join(" "),
        "created_at": (created_at_ms / 1000) as i64,
    })
}

/// Counts used by the instance serializers.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstanceStats {
    pub user_count: u64,
    pub status_count: u64,
    pub domain_count: u64,
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
            "bot": false,
            "discoverable": true,
            "indexable": false,
            "group": false,
            "created_at": date_only(a.created_at_ms),
            "note": a.note,
            "url": a.actor_uri,
            "uri": a.actor_uri,
            "avatar": server.asset_url(MISSING_AVATAR),
            "avatar_static": server.asset_url(MISSING_AVATAR),
            "header": server.asset_url(MISSING_HEADER),
            "header_static": server.asset_url(MISSING_HEADER),
            "followers_count": 0,
            "following_count": 0,
            "statuses_count": 0,
            "last_status_at": Value::Null,
            "emojis": [],
            "fields": [],
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
        "approval_required": false,
        "invites_enabled": false,
        "contact_account": contact,
        "rules": [],
        "configuration": v1_configuration(),
    })
}

/// `REST::InstanceSerializer` (API v2 shape).
pub fn instance_v2_json(
    server: &ServerInfo,
    _stats: InstanceStats,
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
        "source_url": "https://github.com/example/birchtree",
        "description": server.site_description,
        "usage": { "users": { "active_month": 0 } },
        "thumbnail": { "url": server.asset_url("preview.png") },
        "icon": [],
        "languages": ["en"],
        "configuration": v2_configuration(server),
        "registrations": {
            "enabled": server.registrations_open,
            "approval_required": false,
            "reason_required": false,
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

fn v1_configuration() -> Value {
    json!({
        "accounts": { "max_featured_tags": 10 },
        "statuses": {
            "max_characters": 500,
            "max_media_attachments": 4,
            "characters_reserved_per_url": 23,
        },
        "media_attachments": {
            "supported_mime_types": [],
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
        "vapid": { "public_key": "" },
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
            "max_characters": 500,
            "max_media_attachments": 4,
            "characters_reserved_per_url": 23,
        },
        "media_attachments": {
            "description_limit": 10_000,
            "image_matrix_limit": 33_177_600,
            "image_size_limit": 16_777_216,
            "supported_mime_types": [],
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
            "live_feeds": { "local": "public", "remote": "public" },
            "hashtag_feeds": { "local": "public", "remote": "public" },
            "trending_link_feeds": { "local": "public", "remote": "public" },
        },
        "limited_federation": false,
    })
}

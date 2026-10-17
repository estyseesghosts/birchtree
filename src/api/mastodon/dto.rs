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
use crate::application::ports::StatusViewFlags;
use crate::domain::{
    Account, AccountStats, LocalUser, MediaAttachment, OAuthApplication, Poll, Status,
    StatusApplication,
};

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

/// Format a millisecond timestamp as Mastodon's ISO-8601 with milliseconds.
fn rfc3339_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// Base `REST::AccountSerializer` object.
pub fn account_json(server: &ServerInfo, account: &Account) -> Value {
    account_json_with_stats(server, account, None)
}

/// `REST::AccountSerializer` with real counters from `ACCOUNT_STATS`.
///
/// `stats: None` emits the structural zero counts (used where no stats lookup
/// happened, e.g. the instance contact account).
pub fn account_json_with_stats(
    server: &ServerInfo,
    account: &Account,
    stats: Option<&AccountStats>,
) -> Value {
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
    object.insert("followers_count".into(), json!(stats.map_or(0, |s| s.followers_count)));
    object.insert("following_count".into(), json!(stats.map_or(0, |s| s.following_count)));
    object.insert("statuses_count".into(), json!(stats.map_or(0, |s| s.statuses_count)));
    object.insert(
        "last_status_at".into(),
        match stats.and_then(|s| s.last_status_at_ms) {
            Some(ms) => json!(date_only(ms)),
            None => Value::Null,
        },
    );
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
    credential_account_json_with_stats(server, account, user, None)
}

/// Credential account with real counters (incl. `follow_requests_count`).
pub fn credential_account_json_with_stats(
    server: &ServerInfo,
    account: &Account,
    user: Option<&LocalUser>,
    stats: Option<&AccountStats>,
) -> Value {
    let mut object = match account_json_with_stats(server, account, stats) {
        Value::Object(map) => map,
        other => return other,
    };
    object.insert(
        "source".into(),
        source_json(account, user, stats.map_or(0, |s| s.follow_requests_count)),
    );
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

fn source_json(account: &Account, user: Option<&LocalUser>, follow_requests_count: u64) -> Value {
    let privacy = user
        .and_then(|u| u.default_privacy.clone())
        .unwrap_or_else(|| if account.locked { "private" } else { "public" }.to_string());
    json!({
        "privacy": privacy,
        "sensitive": user.and_then(|u| u.default_sensitive).unwrap_or(false),
        "language": user.and_then(|u| u.default_language.clone()),
        "note": account.note,
        "fields": [],
        "follow_requests_count": follow_requests_count,
        "hide_collections": Value::Null,
        "discoverable": true,
        "indexable": false,
        "attribution_domains": [],
        "quote_policy": user
            .and_then(|u| u.default_quote_policy.clone())
            .unwrap_or_else(|| "public".to_string()),
    })
}

/// `REST::RelationshipSerializer` for the viewer's relationship to a target.
pub fn relationship_json(relationship: &crate::application::ports::Relationship) -> Value {
    json!({
        "id": relationship.target_id.to_string(),
        "following": relationship.following,
        "showing_reblogs": relationship.show_reblogs,
        "notifying": relationship.notifying,
        "languages": Value::Null,
        "followed_by": relationship.followed_by,
        "blocking": relationship.blocking,
        "blocked_by": relationship.blocked_by,
        "muting": relationship.muting,
        "muting_notifications": relationship.muting_notifications,
        "muting_expires_at": relationship
            .muting_expires_at_ms
            .map(rfc3339_ms)
            .map(Value::String)
            .unwrap_or(Value::Null),
        "requested": relationship.requested,
        "requested_by": relationship.requested_by,
        "domain_blocking": relationship.domain_blocking,
        "endorsed": relationship.endorsed,
        "note": relationship.note,
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

// ---------------------------------------------------------------------------
// Status DTO packing (Phase 3).
//
// `StatusView` is the pre-assembled page/row model: the use case or handler
// resolves the nested records (reblog, quote, poll, media, mentions) and the
// per-viewer flags once, then the packer renders them exactly like
// `REST::StatusSerializer` (+ `REST::ShallowStatusSerializer` one level deep
// inside quotes).
// ---------------------------------------------------------------------------

/// Which raw/rendered text field the serializer emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSourceMode {
    /// `content` (rendered HTML) — the normal response body.
    Content,
    /// `text` (raw source) — DELETE responses and the source endpoint.
    Source,
}

/// The quote association of a status, as `REST::QuoteSerializer` renders it.
pub struct QuoteView {
    /// Resolved wire state: `accepted`/`pending`/`rejected`/`revoked` or the
    /// derived `deleted`/`unauthorized`/`blocked_domain`/`blocked_account`/
    /// `muted_account`.
    pub state: String,
    /// The quoted status (shallow-nested), present only when accepted (or
    /// source view) and the viewer may see it.
    pub quoted_status: Option<Box<StatusView>>,
    /// The quoted status id (used for shallow nesting and absent-content).
    pub quoted_status_id: Option<crate::domain::Id>,
}

/// A status fully assembled for one viewer.
pub struct StatusView {
    pub status: Status,
    pub account: Account,
    pub account_stats: Option<AccountStats>,
    pub flags: StatusViewFlags,
    /// Wrapped original when `status` is a reblog.
    pub reblog: Option<Box<StatusView>>,
    pub poll: Option<Poll>,
    /// The viewer's ballot choices on `poll`, if any.
    pub viewer_ballot: Option<Vec<u64>>,
    /// Attached media in the author's order.
    pub media: Vec<MediaAttachment>,
    /// Mentioned accounts in mention order.
    pub mentions: Vec<Account>,
    pub quote: Option<QuoteView>,
    /// `quote_approval.current_user` for this viewer (`automatic`/`denied`;
    /// computed by the caller through the quote policy).
    pub quote_current_user: String,
    /// Render the nested `quote` shallowly ({state, quoted_status_id}) — set
    /// one level deep inside a quoted status.
    pub shallow_quote: bool,
}

/// `REST::MediaAttachmentSerializer`. Local Phase 3 attachments carry no
/// processed bytes yet, so `url`/`preview_url` are `null` (contract-correct
/// "not processed" shape) until Phase 4.
pub fn media_attachment_json(media: &MediaAttachment) -> Value {
    json!({
        "id": media.id.to_string(),
        "type": media.r#type,
        "url": media.url,
        "preview_url": Value::Null,
        "remote_url": Value::Null,
        "preview_remote_url": Value::Null,
        "text_url": Value::Null,
        "meta": Value::Null,
        "description": media.description,
        "blurhash": media.blurhash.clone().unwrap_or_default(),
    })
}

/// `REST::PollSerializer` without viewer ballot context.
/// Poll-level counts are serialized unmasked even under `hide_totals`; only
/// per-option `votes_count` is nulled pre-expiry.
pub fn poll_json(poll: &Poll, viewer: Option<crate::domain::Id>, now_ms: u64) -> Value {
    poll_json_with_ballot(poll, viewer, None, now_ms)
}

/// `REST::PollSerializer` with the viewer's ballot applied
/// (`voted`/`own_votes`; the poll author is considered a voter without a
/// ballot, matching Mastodon).
pub fn poll_json_with_ballot(
    poll: &Poll,
    viewer: Option<crate::domain::Id>,
    ballot: Option<&[u64]>,
    now_ms: u64,
) -> Value {
    let expired = poll.is_expired_at(now_ms);
    let options: Vec<Value> = poll
        .options
        .iter()
        .map(|option| {
            json!({
                "title": option.title,
                "votes_count": if poll.show_totals_at(now_ms) {
                    json!(option.votes_count)
                } else {
                    Value::Null
                },
            })
        })
        .collect();
    let is_author = viewer.is_some_and(|v| v == poll.account_id);
    let own: Vec<u64> = ballot.map(<[u64]>::to_vec).unwrap_or_default();
    let voted = is_author || !own.is_empty();
    let own_votes = if own.is_empty() && !is_author {
        Value::Null
    } else {
        json!(own)
    };
    json!({
        "id": poll.id.to_string(),
        "expires_at": poll
            .expires_at_ms
            .map(rfc3339_ms)
            .map(Value::String)
            .unwrap_or(Value::Null),
        "expired": expired,
        "multiple": poll.multiple,
        "votes_count": poll.votes_count,
        "voters_count": poll.voters_count,
        "options": options,
        "emojis": [],
        "voted": voted,
        "own_votes": own_votes,
    })
}

/// `REST::StatusSerializer` (and, one nesting level deep,
/// `REST::ShallowStatusSerializer`).
pub fn status_json(
    server: &ServerInfo,
    viewer: Option<crate::domain::Id>,
    now_ms: u64,
    view: &StatusView,
    source: StatusSourceMode,
) -> Value {
    let status = &view.status;
    let mut object = Map::new();
    object.insert("id".into(), json!(status.id.to_string()));
    object.insert("created_at".into(), json!(rfc3339_ms(status.created_at_ms)));
    object.insert(
        "in_reply_to_id".into(),
        json!(status.in_reply_to_id.map(|id| id.to_string())),
    );
    object.insert(
        "in_reply_to_account_id".into(),
        json!(status.in_reply_to_account_id.map(|id| id.to_string())),
    );
    object.insert("sensitive".into(), json!(status.sensitive));
    object.insert(
        "spoiler_text".into(),
        json!(status.spoiler_text.clone().unwrap_or_default()),
    );
    object.insert("visibility".into(), json!(status.visibility.as_str()));
    object.insert("language".into(), json!(status.language));
    object.insert("uri".into(), json!(status.uri));
    object.insert("url".into(), json!(status.url));
    object.insert("replies_count".into(), json!(status.replies_count));
    object.insert("reblogs_count".into(), json!(status.reblogs_count));
    object.insert("favourites_count".into(), json!(status.favourites_count));
    object.insert("quotes_count".into(), json!(status.quotes_count));
    object.insert(
        "edited_at".into(),
        json!(status.edited_at_ms.map(rfc3339_ms)),
    );

    // Per-viewer fields (Mastodon `if :current_user?`).
    if viewer.is_some() {
        object.insert("favourited".into(), json!(view.flags.favourited));
        object.insert("reblogged".into(), json!(view.flags.reblogged));
        // Conversation mutes are out of profile; the field stays contract-
        // complete with its neutral value.
        object.insert("muted".into(), json!(false));
        object.insert("bookmarked".into(), json!(view.flags.bookmarked));
        object.insert("filtered".into(), json!([]));
    }

    // `pinned` only when pinnable (author, not a reblog, pinnable visibility).
    let pinnable = viewer.is_some_and(|v| v == status.account_id)
        && !status.is_reblog()
        && status.visibility.is_rebloggable();
    if pinnable {
        object.insert("pinned".into(), json!(view.flags.pinned));
    }

    match source {
        StatusSourceMode::Content => {
            object.insert("content".into(), json!(status.content_html));
        }
        StatusSourceMode::Source => {
            object.insert("text".into(), json!(status.source_text));
        }
    }

    object.insert(
        "reblog".into(),
        view.reblog
            .as_deref()
            .map(|inner| status_json(server, viewer, now_ms, inner, StatusSourceMode::Content))
            .unwrap_or(Value::Null),
    );
    object.insert(
        "application".into(),
        view.status
            .application
            .as_ref()
            .map(application_json)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "account".into(),
        account_json_with_stats(server, &view.account, view.account_stats.as_ref()),
    );
    let media: Vec<Value> = view.media.iter().map(media_attachment_json).collect();
    object.insert("media_attachments".into(), json!(media));
    let mentions: Vec<Value> = view
        .mentions
        .iter()
        .map(|account| mention_json(server, account))
        .collect();
    object.insert("mentions".into(), json!(mentions));
    let tags: Vec<Value> = view
        .status
        .tags
        .iter()
        .map(|tag| tag_json(server, tag, viewer.is_some()))
        .collect();
    object.insert("tags".into(), json!(tags));
    object.insert("emojis".into(), json!([]));
    object.insert("tagged_collections".into(), json!([]));

    object.insert(
        "quote".into(),
        quote_json(server, viewer, now_ms, view),
    );
    object.insert("card".into(), Value::Null);
    object.insert(
        "poll".into(),
        view.poll
            .as_ref()
            .map(|poll| poll_json_with_ballot(poll, viewer, view.viewer_ballot.as_deref(), now_ms))
            .unwrap_or(Value::Null),
    );
    object.insert("quote_approval".into(), quote_approval_json(status, viewer, view));

    Value::Object(object)
}

/// Nested status application (`REST::StatusSerializer::ApplicationSerializer`).
fn application_json(application: &StatusApplication) -> Value {
    json!({
        "name": application.name,
        "website": application.website,
    })
}

/// Nested mention (`REST::StatusSerializer::MentionSerializer`).
fn mention_json(server: &ServerInfo, account: &Account) -> Value {
    let url = if account.local {
        format!("{}/@{}", server.public_url.trim_end_matches('/'), account.username)
    } else {
        account.actor_uri.clone()
    };
    json!({
        "id": account.id.to_string(),
        "username": account.username,
        "url": url,
        "acct": account.acct(),
    })
}

/// Nested tag (`REST::TagSerializer`); tag rows (history/following) arrive
/// with the Phase 7 tag surface, so `id` is the tag name here (recorded in
/// the endpoint manifest).
fn tag_json(server: &ServerInfo, tag: &str, viewer_present: bool) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(tag));
    object.insert("name".into(), json!(tag));
    object.insert(
        "url".into(),
        json!(format!(
            "{}/tags/{}",
            server.public_url.trim_end_matches('/'),
            tag
        )),
    );
    object.insert("history".into(), json!([]));
    if viewer_present {
        object.insert("following".into(), json!(false));
        object.insert("featuring".into(), json!(false));
    }
    Value::Object(object)
}

/// The `quote` association: full `REST::QuoteSerializer` at the top level,
/// shallow (`state` + `quoted_status_id`) one level deep inside a quote.
fn quote_json(
    server: &ServerInfo,
    viewer: Option<crate::domain::Id>,
    now_ms: u64,
    view: &StatusView,
) -> Value {
    let Some(quote) = view.quote.as_ref() else {
        return Value::Null;
    };
    if view.shallow_quote {
        return json!({
            "state": quote.state,
            "quoted_status_id": quote.quoted_status_id.map(|id| id.to_string()),
        });
    }
    json!({
        "state": quote.state,
        "quoted_status": quote
            .quoted_status
            .as_deref()
            .map(|inner| status_json(server, viewer, now_ms, inner, StatusSourceMode::Content))
            .unwrap_or(Value::Null),
    })
}

/// `quote_approval`: `{ automatic: [...keys], manual: [...], current_user }`.
/// Locally authored posts only ever populate the automatic half; `manual` is
/// always empty (matches the pinned serializer for API-created statuses).
fn quote_approval_json(
    status: &Status,
    viewer: Option<crate::domain::Id>,
    view: &StatusView,
) -> Value {
    let automatic: Vec<&str> = match status.quote_policy {
        crate::domain::QuotePolicy::Public => vec!["public"],
        crate::domain::QuotePolicy::Followers => vec!["followers"],
        crate::domain::QuotePolicy::Nobody => vec![],
    };
    let current_user = if viewer.is_some() {
        view.quote_current_user.clone()
    } else {
        // `quote_policy_for_account(nil)` -> :denied.
        "denied".to_string()
    };
    json!({
        "automatic": automatic,
        "manual": [],
        "current_user": current_user,
    })
}

/// `REST::StatusSourceSerializer`: `{ id, text, spoiler_text }`.
pub fn status_source_json(status: &Status) -> Value {
    json!({
        "id": status.id.to_string(),
        "text": status.source_text,
        "spoiler_text": status.spoiler_text.clone().unwrap_or_default(),
    })
}

/// `REST::StatusEditSerializer` (edit history entries).
pub fn status_edit_json(
    server: &ServerInfo,
    version: &crate::domain::StatusVersion,
    account: &Account,
    media: &[MediaAttachment],
    poll_options: Option<Vec<String>>,
) -> Value {
    let media_json: Vec<Value> = media.iter().map(media_attachment_json).collect();
    json!({
        "account": account_json(server, account),
        "content": version.content_html,
        "spoiler_text": version.spoiler_text,
        "sensitive": version.sensitive,
        "created_at": rfc3339_ms(version.created_at_ms),
        "media_attachments": media_json,
        "emojis": [],
        "poll": poll_options.map(|titles| json!({
            "options": titles.into_iter().map(|title| json!({ "title": title })).collect::<Vec<_>>(),
        })),
    })
}

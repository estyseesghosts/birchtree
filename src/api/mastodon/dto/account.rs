//! Mastodon account DTO packers (S10 split of `api::mastodon::dto`).
//!
//! Account, credential-account, relationship, application, preferences,
//! token, trend-tag and hashtag serializers. Field names, types and
//! nullability follow the pinned Mastodon v4.7.3 serializers
//! (`app/serializers/rest/*`).

use serde_json::{Map, Value, json};

use crate::api::mastodon::ServerInfo;
use crate::api::mastodon::endpoints::admin_accounts;
use crate::application::license_policy;
use crate::application::ports::Relationship;
use crate::application::ports::filter_json::rfc3339_ms;
use crate::application::trends::TagHistoryDay;
use crate::domain::{Account, AccountStats, Id, LocalUser, OAuthApplication, TrendTagFlag};

use super::media::media_file_url;

/// Mastodon's default missing avatar/header asset path.
const MISSING_AVATAR: &str = "avatars/original/missing.png";
const MISSING_HEADER: &str = "headers/original/missing.png";

/// Format a millisecond timestamp as a `YYYY-MM-DD` date (Mastodon uses
/// `created_at.midnight.as_json`).
pub(crate) fn date_only(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.date_naive().to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// Shared tag URL builder for the three tag shapes in this file and the
/// status-nested tag (`status.rs`).
///
/// `trend_tag_json` and the status-nested `tag_json` already built this exact
/// expression; `hashtag_json` keeps its own `asset_url` construction verbatim
/// (different trailing-slash behavior), so it does not use this helper.
pub(crate) fn tag_url(server: &ServerInfo, name: &str) -> String {
    format!("{}/tags/{}", server.public_url.trim_end_matches('/'), name)
}

/// Avatar URL for an account: the staged blob when one is referenced, else
/// the conventional missing-avatar asset.
pub fn avatar_url(server: &ServerInfo, account: &Account) -> String {
    match account.avatar_key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => media_file_url(server, key),
        None => server.asset_url(MISSING_AVATAR),
    }
}

/// Avatar-static URL: birchtree serves the same bytes for both styles (no
/// Paperclip static-variant transcode — documented in the endpoint
/// manifest), so this mirrors [`avatar_url`].
pub fn avatar_static_url(server: &ServerInfo, account: &Account) -> String {
    avatar_url(server, account)
}

/// Header URL for an account, same fallback rule as [`avatar_url`].
pub fn header_url(server: &ServerInfo, account: &Account) -> String {
    match account.header_key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => media_file_url(server, key),
        None => server.asset_url(MISSING_HEADER),
    }
}

/// Header-static URL: same bytes as [`header_url`] (no static variant).
pub fn header_static_url(server: &ServerInfo, account: &Account) -> String {
    header_url(server, account)
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
    object.insert("bot".into(), json!(account.bot));
    object.insert(
        "discoverable".into(),
        account.discoverable.map(Value::from).unwrap_or(Value::Null),
    );
    object.insert("indexable".into(), json!(account.indexable));
    object.insert("group".into(), json!(false));
    object.insert("created_at".into(), json!(date_only(account.created_at_ms)));
    object.insert("note".into(), json!(account.note));
    object.insert("url".into(), json!(account.actor_uri));
    object.insert("uri".into(), json!(account.actor_uri));
    object.insert("avatar".into(), json!(avatar_url(server, account)));
    object.insert(
        "avatar_static".into(),
        json!(avatar_static_url(server, account)),
    );
    object.insert(
        "avatar_description".into(),
        json!(account.avatar_description.clone()),
    );
    object.insert("header".into(), json!(header_url(server, account)));
    object.insert(
        "header_static".into(),
        json!(header_static_url(server, account)),
    );
    object.insert(
        "header_description".into(),
        json!(account.header_description.clone()),
    );
    object.insert(
        "followers_count".into(),
        json!(stats.map_or(0, |s| s.followers_count)),
    );
    object.insert(
        "following_count".into(),
        json!(stats.map_or(0, |s| s.following_count)),
    );
    object.insert(
        "statuses_count".into(),
        json!(stats.map_or(0, |s| s.statuses_count)),
    );
    object.insert(
        "last_status_at".into(),
        match stats.and_then(|s| s.last_status_at_ms) {
            Some(ms) => json!(date_only(ms)),
            None => Value::Null,
        },
    );
    object.insert(
        "hide_collections".into(),
        account
            .hide_collections
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    object.insert("show_media".into(), json!(true));
    object.insert("show_media_replies".into(), json!(true));
    object.insert("show_featured".into(), json!(true));
    object.insert("emojis".into(), json!([]));
    object.insert(
        "fields".into(),
        json!(
            account
                .fields
                .iter()
                .map(|field| json!({
                    "name": field.name,
                    "value": field.value,
                    "verified_at": field.verified_at.map(admin_accounts::iso),
                }))
                .collect::<Vec<_>>()
        ),
    );
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
    // Core extension CORE-L: `preferredLicense` renders only when the policy
    // is enabled (`off` omits the key); enabled + `None` renders `null`.
    if let Some(value) =
        license_policy::license_wire_value(&server.license_policy, account.default_license)
    {
        object.insert("preferredLicense".into(), value);
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
    object.insert("role".into(), role_json(user));
    Value::Object(object)
}

/// `REST::RoleSerializer` for the credential account (`role_serializer.rb`
/// at the pin: `{id, name, permissions, color, highlighted,
/// collection_limit}`, `permissions` = `computed_permissions.to_s`).
///
/// Non-staff viewers keep the historical `User` shape (no role rows exist for
/// them). Staff render their real collapsed role (`user.rs` `UserRole`):
/// names match the seeded `config/roles.yml` `Moderator`/`Admin` entries and
/// `permissions` is the OR of the upstream flag bits that role carries there
/// (`user_role.rb:20-44` — e.g. moderator = `view_dashboard | view_audit_log
/// | view_feeds | manage_users | manage_reports | manage_taxonomies` =
/// `1049884`, the OR of the positions below; the previous comment's `1049888`
/// was an arithmetic slip, flagged by G-E and corrected here). Ids are
/// upstream-style numerics (`0` user / `1` moderator / `2` admin): birchtree
/// collapses roles to an enum with no row ids, so the ids are these canonical
/// ordinals, never `-1`.
pub fn role_json(user: Option<&LocalUser>) -> Value {
    use crate::domain::user::{ADMIN_PERMISSIONS, MODERATOR_PERMISSIONS, UserRole as DomainRole};
    let (id, name, permissions, highlighted) = match user.map(|u| u.role).unwrap_or_default() {
        DomainRole::Admin => (2u64, "Admin", ADMIN_PERMISSIONS, true),
        DomainRole::Moderator => (1u64, "Moderator", MODERATOR_PERMISSIONS, true),
        DomainRole::None => (0u64, "User", 0u64, false),
    };
    json!({
        "id": id,
        "name": name,
        "permissions": permissions.to_string(),
        "color": "",
        "highlighted": highlighted,
        "collection_limit": 0,
    })
}

fn source_json(account: &Account, user: Option<&LocalUser>, follow_requests_count: u64) -> Value {
    let privacy = user
        .and_then(|u| u.default_privacy.clone())
        .unwrap_or_else(|| if account.locked { "private" } else { "public" }.to_string());
    // `language` is the real `setting_default_language` (upstream uses
    // `preferred_posting_language` only in preferences, not here).
    json!({
        "privacy": privacy,
        "sensitive": user.and_then(|u| u.default_sensitive).unwrap_or(false),
        "language": user.and_then(|u| u.default_language.clone()),
        "note": account.note,
        "fields": account
            .fields
            .iter()
            .map(|field| json!({
                "name": field.name,
                "value": field.value,
                "verified_at": field.verified_at.map(admin_accounts::iso),
            }))
            .collect::<Vec<_>>(),
        "follow_requests_count": follow_requests_count,
        "hide_collections": account.hide_collections.map(Value::from).unwrap_or(Value::Null),
        "discoverable": account.discoverable.map(Value::from).unwrap_or(Value::Null),
        "indexable": account.indexable,
        "attribution_domains": account.attribution_domains,
        "quote_policy": user
            .and_then(|u| u.default_quote_policy.clone())
            .unwrap_or_else(|| "public".to_string()),
    })
}

/// `REST::RelationshipSerializer` for the viewer's relationship to a target.
pub fn relationship_json(relationship: &Relationship) -> Value {
    json!({
        "id": relationship.target_id.to_string(),
        "following": relationship.following,
        "showing_reblogs": relationship.show_reblogs,
        "notifying": relationship.notifying,
        "languages": relationship
            .languages
            .clone()
            .map(|l| l.into_iter().map(Value::String).collect::<Vec<_>>())
            .map(Value::from)
            .unwrap_or(Value::Null),
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
///
/// `posting:default:language` is `preferred_posting_language`
/// (`has_settings.rb:22-24`: `valid_locale_cascade(default_language, locale,
/// I18n.locale)`), i.e. the posting default with the registration locale as
/// the cascade fallback — not the raw `default_language` setting. The
/// `reading:*` keys have no stored settings yet (upstream `web.display_media`
/// / `web.expand_content_warnings` / `web.auto_play`, all under the web
/// settings namespace): they render upstream's column defaults
/// (`display_media` default `'default'`, booleans `false`) until per-user
/// reading settings land (named follow-up: `LocalUser` +
/// `UserSettingsUpdate` + store fields).
pub fn preferences_json(account: &Account, user: Option<&LocalUser>) -> Value {
    let visibility = user
        .and_then(|u| u.default_privacy.clone())
        .unwrap_or_else(|| if account.locked { "private" } else { "public" }.to_string());
    // `preferred_posting_language` cascade minus the server locale (no server
    // locale setting exists here): first non-blank of `default_language`,
    // the registration `locale`.
    let posting_language = user
        .and_then(|u| {
            u.default_language
                .clone()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| u.locale.clone().filter(|v| !v.trim().is_empty()))
        })
        .map(Value::String)
        .unwrap_or(Value::Null);
    json!({
        "posting:default:visibility": visibility,
        "posting:default:sensitive": user.and_then(|u| u.default_sensitive).unwrap_or(false),
        "posting:default:language": posting_language,
        "posting:default:quote_policy": user
            .and_then(|u| u.default_quote_policy.clone())
            .unwrap_or_else(|| "public".to_string()),
        "reading:expand:media": user
            .and_then(|u| u.web_display_media.clone())
            .unwrap_or_else(|| "default".to_string()),
        "reading:expand:spoilers": user.and_then(|u| u.web_expand_spoilers).unwrap_or(false),
        "reading:autoplay:gifs": user.and_then(|u| u.web_auto_play_gifs).unwrap_or(false),
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

/// `REST::TagSerializer` for a trend tag: the flag row id is the id
/// (upstream's numeric Tag row id); `history` is seven real per-day buckets
/// (`{day, uses, accounts}`, strings, newest first — upstream
/// `Trends::History::Day#as_json`); `following`/`featuring` appear for
/// authenticated viewers with real values from the tag-follow and
/// featured-tag stores.
pub fn trend_tag_json(
    server: &ServerInfo,
    flag: &TrendTagFlag,
    viewer: Option<Id>,
    history: &[TagHistoryDay],
    following: bool,
    featuring: bool,
) -> Value {
    let mut object = Map::new();
    // Upstream `REST::TagSerializer#id` is the numeric Tag row id (as a
    // string), not the name; the flag row plays that role here.
    object.insert("id".into(), json!(flag.id.to_string()));
    object.insert("name".into(), json!(flag.name));
    object.insert("url".into(), json!(tag_url(server, &flag.name)));
    object.insert(
        "history".into(),
        json!(
            history
                .iter()
                .map(|day| serde_json::json!({
                    "day": day.day_sec.to_string(),
                    "uses": day.uses.to_string(),
                    "accounts": day.accounts.to_string(),
                }))
                .collect::<Vec<_>>()
        ),
    );
    if viewer.is_some() {
        object.insert("following".into(), json!(following));
        object.insert("featuring".into(), json!(featuring));
    }
    Value::Object(object)
}

/// `REST::Admin::TagSerializer` projection of a trend tag: the public shape
/// plus `trendable` (`None` renders `null` = pending review) and
/// `requires_review`.
pub fn admin_trend_tag_json(
    server: &ServerInfo,
    flag: &TrendTagFlag,
    viewer: Option<Id>,
    history: &[TagHistoryDay],
    following: bool,
    featuring: bool,
) -> Value {
    let mut object = match trend_tag_json(server, flag, viewer, history, following, featuring) {
        Value::Object(map) => map,
        other => return other,
    };
    object.insert("trendable".into(), json!(flag.allowed));
    object.insert(
        "requires_review".into(),
        json!(flag.reviewed_at_ms.is_none()),
    );
    Value::Object(object)
}

/// `REST::TagSerializer` for search results (`history` is contract-shaped;
/// this server keeps no per-tag usage history yet, and tag follow/feature is
/// Phase 7).
pub fn hashtag_json(server: &ServerInfo, tag: &str, viewer: Option<crate::domain::Id>) -> Value {
    let _ = viewer;
    json!({
        "id": tag,
        "name": tag,
        "url": server.asset_url(&format!("tags/{tag}")),
        "history": [],
        "following": false,
        "featuring": false,
    })
}

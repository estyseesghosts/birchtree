//! Mastodon status DTO packers (S10 split of `api::mastodon::dto`).
//!
//! `StatusView` is the pre-assembled page/row model: the use case or handler
//! resolves the nested records (reblog, quote, poll, media, mentions) and the
//! per-viewer flags once, then the packer renders them exactly like
//! `REST::StatusSerializer` (+ `REST::ShallowStatusSerializer` one level deep
//! inside quotes).
//!
//! `StatusView` / `QuoteView` live here (not in a shared model file) because
//! this renderer is their only consumer: `status_view.rs` assembles them and
//! `status_json` below consumes them.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::api::mastodon::ServerInfo;
use crate::application::ports::StatusViewFlags;
use crate::application::ports::filter_json::rfc3339_ms;
use crate::domain::{
    Account, AccountStats, Id, MediaAttachment, Place, Poll, Status, StatusApplication,
    StatusVersion,
};

use super::account::{account_json, account_json_with_stats, tag_url};
use super::media::{media_attachment_json, media_attachment_json_with_source};
use super::poll::poll_json_with_ballot;

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
    pub quoted_status_id: Option<Id>,
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
    /// `REST::FilterResultSerializer` entries matching this status for the
    /// viewer (filter v2). Timelines/notifications fill this per page; the
    /// status DTO never drops or reorders a status because of it (Mastodon
    /// leaves `hide` enforcement to the client).
    pub filtered: Vec<Value>,
    /// Custom emoji referenced by the status text (batch-loaded per page).
    pub emojis: Vec<Value>,
    /// Remote origin URLs by media id (batch-loaded per page from the
    /// stored remote-media source; absent = local media). Rendered as
    /// `remote_url`/`preview_remote_url` (adversarial-2 D1); never set
    /// for local media. Mastodon exposes the origin URL regardless of
    /// visibility, so there is no visibility gate here either.
    pub remote_media_sources: HashMap<Id, String>,
    /// Core extension CORE-P: the attached place (batched per page;
    /// dangling ids read as no place).
    pub place: Option<Place>,
}

/// `REST::StatusSerializer` (and, one nesting level deep,
/// `REST::ShallowStatusSerializer`).
pub fn status_json(
    server: &ServerInfo,
    viewer: Option<Id>,
    now_ms: u64,
    view: &StatusView,
    source: StatusSourceMode,
) -> Value {
    let mut object = Map::new();
    pack_base(server, viewer, now_ms, view, source, &mut object);
    pack_viewer_flags(viewer, view, &mut object);
    pack_quote(server, viewer, now_ms, view, &mut object);
    Value::Object(object)
}

/// Viewer-independent keys: scalars, content/text, reblog recursion, nested
/// account/media/mentions/tags/emojis, location and the null card.
fn pack_base(
    server: &ServerInfo,
    viewer: Option<Id>,
    now_ms: u64,
    view: &StatusView,
    source: StatusSourceMode,
    object: &mut Map<String, Value>,
) {
    let status = &view.status;
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
    let media: Vec<Value> = view
        .media
        .iter()
        .map(|attachment| {
            let source = view
                .remote_media_sources
                .get(&attachment.id)
                .map(String::as_str);
            media_attachment_json_with_source(server, attachment, source)
        })
        .collect();
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
    object.insert("emojis".into(), json!(view.emojis.clone()));
    object.insert("tagged_collections".into(), json!([]));
    // Core extension CORE-P: `location` renders only when the policy is
    // enabled (`off` omits the key); enabled + placeless renders nothing.
    if server.place_policy != "off"
        && let Some(place) = &view.place
    {
        object.insert(
            "location".into(),
            json!({
                "id": place.id.to_string(),
                "name": place.name,
                "longitude": place.lon,
                "latitude": place.lat,
                "country": place.country,
            }),
        );
    }
    object.insert("card".into(), Value::Null);
}

/// Per-viewer keys (Mastodon `if :current_user?`) plus the pinnable `pinned`
/// key.
fn pack_viewer_flags(viewer: Option<Id>, view: &StatusView, object: &mut Map<String, Value>) {
    let status = &view.status;
    // Per-viewer fields (Mastodon `if :current_user?`).
    if viewer.is_some() {
        object.insert("favourited".into(), json!(view.flags.favourited));
        object.insert("reblogged".into(), json!(view.flags.reblogged));
        // Conversation mutes ride the status DTO (Phase 7): the mute map is
        // pre-fetched per page; missing entries read neutral `false`.
        object.insert("muted".into(), json!(view.flags.muted));
        object.insert("bookmarked".into(), json!(view.flags.bookmarked));
        object.insert("filtered".into(), json!(view.filtered.clone()));
    }

    // `pinned` only when pinnable (author, not a reblog, pinnable visibility).
    let pinnable = viewer.is_some_and(|v| v == status.account_id)
        && !status.is_reblog()
        && status.visibility.is_rebloggable();
    if pinnable {
        object.insert("pinned".into(), json!(view.flags.pinned));
    }
}

/// Quote association, poll and quote approval keys.
fn pack_quote(
    server: &ServerInfo,
    viewer: Option<Id>,
    now_ms: u64,
    view: &StatusView,
    object: &mut Map<String, Value>,
) {
    object.insert("quote".into(), quote_json(server, viewer, now_ms, view));
    object.insert(
        "poll".into(),
        view.poll
            .as_ref()
            .map(|poll| poll_json_with_ballot(poll, viewer, view.viewer_ballot.as_deref(), now_ms))
            .unwrap_or(Value::Null),
    );
    object.insert(
        "quote_approval".into(),
        quote_approval_json(&view.status, viewer, view),
    );
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
        format!(
            "{}/@{}",
            server.public_url.trim_end_matches('/'),
            account.username
        )
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
    object.insert("url".into(), json!(tag_url(server, tag)));
    object.insert("history".into(), json!([]));
    if viewer_present {
        object.insert("following".into(), json!(false));
        object.insert("featuring".into(), json!(false));
    }
    Value::Object(object)
}

/// The `quote` association: full `REST::QuoteSerializer` at the top level,
/// shallow (`state` + `quoted_status_id`) one level deep inside a quote.
fn quote_json(server: &ServerInfo, viewer: Option<Id>, now_ms: u64, view: &StatusView) -> Value {
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
fn quote_approval_json(status: &Status, viewer: Option<Id>, view: &StatusView) -> Value {
    use crate::domain::QuotePolicy;
    let automatic: Vec<&str> = match status.quote_policy {
        QuotePolicy::Public => vec!["public"],
        QuotePolicy::Followers => vec!["followers"],
        QuotePolicy::Nobody => vec![],
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

/// `REST::StatusSourceSerializer`: `{id, text, spoiler_text}`.
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
    version: &StatusVersion,
    account: &Account,
    media: &[MediaAttachment],
    poll_options: Option<Vec<String>>,
) -> Value {
    let media_json: Vec<Value> = media
        .iter()
        .map(|attachment| media_attachment_json(server, attachment))
        .collect();
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

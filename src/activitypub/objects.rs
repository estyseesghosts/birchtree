//! Remote statuses as the crate's `Object`: dereferenced by URI (fetched and
//! verified by the crate, then stored through the Note ingest path), so an
//! announced, quoted or parent status is handled by the same code as a Create.

use activitypub_federation::{
    config::Data,
    protocol::verification::verify_domains_match,
    traits::Object,
};
use async_trait::async_trait;
use url::Url;

use crate::activitypub::actor::ActorRecord;
use crate::activitypub::activities::{note_input_for, question_input_for};
use crate::activitypub::error::FedError;
use crate::activitypub::outgoing;
use crate::activitypub::wire::NoteOrQuestion;
use crate::activitypub::FedContext;
use crate::application::federation_inbound::ingest_note;
use crate::domain::Status;

/// A status known here (local or remote), as the crate's object.
#[derive(Debug, Clone)]
pub struct NoteRef {
    pub status: Status,
}

#[async_trait]
impl Object for NoteRef {
    type DataType = FedContext;
    type Kind = NoteOrQuestion;
    type Error = FedError;

    async fn read_from_id(object_id: Url, data: &Data<FedContext>) -> Result<Option<Self>, FedError> {
        let store = &*data.app.store;
        Ok(store.status_by_uri(object_id.as_str())?.map(|status| NoteRef { status }))
    }

    async fn into_json(self, data: &Data<FedContext>) -> Result<NoteOrQuestion, FedError> {
        let store = &*data.app.store;
        let now = data.app.now_ms();
        let poll = store.poll_by_status(self.status.id)?;
        match poll {
            Some(poll) => {
                let value = outgoing::question_for_status(store, &data.public_url, &self.status, &poll, now)?;
                serde_json::from_value(value)
                    .map(|q| NoteOrQuestion::Question(Box::new(q)))
                    .map_err(|e| FedError::Refused(format!("question does not serialize: {e}")))
            }
            None => {
                let value = outgoing::note_for_status(store, &data.public_url, &self.status, now)?;
                serde_json::from_value(value)
                    .map(|n| NoteOrQuestion::Note(Box::new(n)))
                    .map_err(|e| FedError::Refused(format!("note does not serialize: {e}")))
            }
        }
    }

    async fn verify(json: &NoteOrQuestion, expected_domain: &Url, _data: &Data<FedContext>) -> Result<(), FedError> {
        match json {
            NoteOrQuestion::Note(note) => {
                verify_domains_match(&note.id, expected_domain)?;
                verify_domains_match(&note.id, &note.attributed_to)?;
            }
            NoteOrQuestion::Question(question) => {
                verify_domains_match(&question.id, expected_domain)?;
                verify_domains_match(&question.id, &question.attributed_to)?;
            }
        }
        Ok(())
    }

    async fn from_json(json: NoteOrQuestion, data: &Data<FedContext>) -> Result<Self, FedError> {
        let now = data.app.now_ms();
        match json {
            NoteOrQuestion::Note(note) => {
                // A dereferenced object whose Delete arrived first stays gone:
                // report it deleted (terminal for the fetcher) instead of
                // resurrecting it.
                if data.app.store.remote_tombstone(note.id.as_str())?.is_some() {
                    return Err(FedError::Protocol(activitypub_federation::error::Error::ObjectDeleted(
                        note.id.clone(),
                    )));
                }
                let author = crate::activitypub::objects::author_of(data, &note.attributed_to).await?;
                let sensitized = data
                    .app
                    .store
                    .account_by_id(author)?
                    .is_some_and(|account| account.sensitized);
                let input = note_input_for(&note, author, sensitized);
                let ingested = ingest_note(&*data.app.store, input, None, now)?;
                Ok(NoteRef { status: ingested.status })
            }
            NoteOrQuestion::Question(question) => {
                if data.app.store.remote_tombstone(question.id.as_str())?.is_some() {
                    return Err(FedError::Protocol(activitypub_federation::error::Error::ObjectDeleted(
                        question.id.clone(),
                    )));
                }
                let author = crate::activitypub::objects::author_of(data, &question.attributed_to).await?;
                let sensitized = data
                    .app
                    .store
                    .account_by_id(author)?
                    .is_some_and(|account| account.sensitized);
                let (input, poll) = question_input_for(&question, author, sensitized, now);
                let ingested = ingest_note(&*data.app.store, input, Some(poll), now)?;
                Ok(NoteRef { status: ingested.status })
            }
        }
    }
}

/// The local account id of a remote author, dereferencing (and storing) the actor if needed.
pub async fn author_of(data: &Data<FedContext>, actor: &Url) -> Result<crate::domain::Id, FedError> {
    let account = activitypub_federation::fetch::object_id::ObjectId::<ActorRecord>::from(actor.clone())
        .dereference(data)
        .await?;
    Ok(account.account.id)
}

// ---------------------------------------------------------------------------
// Federation tail (G-D): remote emoji, Note/featured collections, remote
// counts as hints, WebFinger shape helpers, remote oEmbed fetch + sanitize.
// ---------------------------------------------------------------------------

use crate::domain::CustomEmoji;

/// A remote custom emoji as carried in a Note's (or actor's) `tag` array
/// (Mastodon `EmojiTag`: `{"type":"Emoji","name":":shortcode:","icon":{...}}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEmoji {
    pub shortcode: String,
    pub url: String,
}

/// At most this many remote emoji are read from one object (untrusted input
/// bound; Mastodon renders a bounded set per status).
pub const MAX_REMOTE_EMOJIS_PER_OBJECT: usize = 50;
/// Longest accepted emoji image URL (untrusted input bound).
pub const MAX_REMOTE_EMOJI_URL_LEN: usize = 2048;

/// Read the remote emoji of an ActivityStreams object (`Note`, `Question`,
/// `Person`) from its raw JSON `tag` array. The typed [`crate::activitypub::wire::Tag`]
/// drops the `icon` payload, so this scans the raw value instead.
///
/// Malformed entries are skipped (never a refusal: one bad tag must not drop
/// the status). Shortcodes must pass [`CustomEmoji::shortcode_valid`]; URLs
/// must be `http(s)` without credentials and within the length cap.
pub fn parse_remote_emojis(raw_object: &serde_json::Value) -> Vec<RemoteEmoji> {
    let tags: Vec<&serde_json::Value> = match raw_object.get("tag") {
        Some(serde_json::Value::Array(items)) => items.iter().collect(),
        Some(single @ serde_json::Value::Object(_)) => vec![single],
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for tag in tags {
        if out.len() >= MAX_REMOTE_EMOJIS_PER_OBJECT {
            break;
        }
        if tag.get("type").and_then(|t| t.as_str()) != Some("Emoji") {
            continue;
        }
        let Some(name) = tag.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let shortcode = name.trim_matches(':');
        if !CustomEmoji::shortcode_valid(shortcode) || !seen.insert(shortcode.to_string()) {
            continue;
        }
        let Some(url) = tag.get("icon").and_then(icon_url) else {
            continue;
        };
        if url.len() > MAX_REMOTE_EMOJI_URL_LEN {
            continue;
        }
        let Ok(parsed) = Url::parse(&url) else { continue };
        match parsed.scheme() {
            "http" | "https" => {}
            _ => continue,
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            continue;
        }
        if parsed.host_str().is_none() {
            continue;
        }
        out.push(RemoteEmoji { shortcode: shortcode.to_string(), url });
    }
    out
}

/// The image URL of an `Emoji` tag's `icon`: a bare URL string, an object
/// with a `url` (string, object-with-`href`, or array), or an array of those.
fn icon_url(icon: &serde_json::Value) -> Option<String> {
    match icon {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => items.iter().find_map(icon_url),
        serde_json::Value::Object(map) => {
            if let Some(url) = map.get("url").and_then(|u| match u {
                serde_json::Value::String(s) => Some(s.clone()),
                other => icon_url(other),
            }) {
                return Some(url);
            }
            map.get("href").and_then(|h| h.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

/// A remote emoji as a Mastodon `emojis`-array entity: remote images are
/// never picker-visible and never listed (upstream `CustomEmoji.listed` is
/// local-only), so they ride the status/account payload only.
pub fn remote_emoji_entity(shortcode: &str, url: &str) -> serde_json::Value {
    serde_json::json!({
        "shortcode": shortcode,
        "url": url,
        "static_url": url,
        "visible_in_picker": false,
    })
}

/// The `replies` collection of a served Note: `{id, type: Collection,
/// totalItems, first}`. The pages behind `first` are the context
/// descendants (parent-merge snippet wires this into `outgoing::note_inner`
/// and serves pages from local replies).
pub fn note_replies_value(status_uri: &str, total_items: u64) -> serde_json::Value {
    let id = format!("{status_uri}/replies");
    serde_json::json!({
        "id": id,
        "type": "Collection",
        "totalItems": total_items,
        "first": format!("{id}?page=true"),
    })
}

/// The `featuredTags` collection URI advertised on a served actor document
/// (upstream `PersonSerializer#featured_tags`: the tagged collection).
pub fn featured_tags_collection_uri(actor_uri: &str) -> String {
    format!("{}/collections/featured", actor_uri.trim_end_matches('/'))
}

/// A `featuredTags` collection document over this account's featured tags
/// (`(name, url)` pairs, newest first). Served at
/// [`featured_tags_collection_uri`] (parent-merge snippet: route in
/// `activitypub::http`, tag URLs `{public_url}/tags/{name}`).
pub fn featured_tags_collection_doc(
    actor_uri: &str,
    public_url: &str,
    tags: &[(String, u64)],
) -> serde_json::Value {
    let id = featured_tags_collection_uri(actor_uri);
    let base = public_url.trim_end_matches('/');
    let items: Vec<serde_json::Value> = tags
        .iter()
        .map(|(name, _)| {
            serde_json::json!({
                "type": "Hashtag",
                "href": format!("{base}/tags/{name}"),
                "name": format!("#{name}"),
            })
        })
        .collect();
    serde_json::json!({
        "@context": crate::activitypub::outgoing::context(),
        "id": id,
        "type": "OrderedCollection",
        "totalItems": items.len(),
        "orderedItems": items,
    })
}

/// Remote counters as untrusted hints: what a remote actor document or
/// `Note` claims (`followers`/`following`/`statusesCount`,
/// `repliesCount`/`likes`/`shares`). A hint NEVER overwrites a locally
/// measured counter; absurd values (past [`MAX_REMOTE_COUNT_HINT`]) are
/// dropped, not clamped into authority.
pub const MAX_REMOTE_COUNT_HINT: u64 = 100_000_000;

/// One validated set of remote counter hints (`None` = the origin's claim
/// was absurd on every field).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoteCountsHint {
    pub followers: Option<u64>,
    pub following: Option<u64>,
    pub statuses: Option<u64>,
}

/// Keep a remote counter claim only when it fits the hint bound.
pub fn hint_count(value: u64) -> Option<u64> {
    (value <= MAX_REMOTE_COUNT_HINT).then_some(value)
}

/// Validate one set of remote counter claims.
pub fn validate_remote_counts(
    followers: Option<u64>,
    following: Option<u64>,
    statuses: Option<u64>,
) -> RemoteCountsHint {
    RemoteCountsHint {
        followers: followers.and_then(hint_count),
        following: following.and_then(hint_count),
        statuses: statuses.and_then(hint_count),
    }
}

/// Counters to render for an account: local accounts always render locally
/// measured stats; remote accounts fill a field from the hint ONLY while the
/// local measurement is zero (no local evidence yet). A nonzero local count
/// always wins over any hint.
pub fn display_counts(
    local_account: bool,
    local: crate::domain::AccountStats,
    hint: RemoteCountsHint,
) -> crate::domain::AccountStats {
    if local_account {
        return local;
    }
    let fill = |measured: u64, hinted: Option<u64>| {
        if measured > 0 { measured } else { hinted.unwrap_or(0) }
    };
    crate::domain::AccountStats {
        followers_count: fill(local.followers_count, hint.followers),
        following_count: fill(local.following_count, hint.following),
        statuses_count: fill(local.statuses_count, hint.statuses),
        last_status_at_ms: local.last_status_at_ms,
        follow_requests_count: local.follow_requests_count,
    }
}

// ---------------------------------------------------------------------------
// Live WebFinger shape helpers (the resolver already exists as
// `activitypub::resolve::resolve_handle`; the lookup route needs the shape
// below: resolve unknown remote handles live, 404 only when truly missing).
// ---------------------------------------------------------------------------

/// Split a lookup `acct` (or WebFinger `resource`) into `(username, domain)`.
/// Accepts `acct:user@domain`, `user@domain` and a single leading `@`.
/// Returns `None` for a bare local name (no `@domain`).
pub fn split_remote_acct(acct: &str) -> Option<(String, String)> {
    let stripped = acct.strip_prefix("acct:").unwrap_or(acct);
    let stripped = stripped.strip_prefix('@').unwrap_or(stripped);
    let (user, domain) = stripped.split_once('@')?;
    if user.is_empty() || domain.is_empty() {
        return None;
    }
    Some((user.to_string(), domain.to_ascii_lowercase()))
}

/// True when `domain` is a remote handle this server should resolve over live
/// WebFinger (unknown remote), rather than answer from the local handle index.
pub fn lookup_should_resolve_live(domain: &str, local_domain: &str) -> bool {
    !domain.eq_ignore_ascii_case(local_domain)
}

// ---------------------------------------------------------------------------
// Remote oEmbed fetch + sanitize (`GET /api/web/embeds/:id` for remote
// statuses; the route itself lives in `endpoints::web`, parent-merged).
// ---------------------------------------------------------------------------

/// Largest accepted remote oEmbed document (SSRF/size/time/redirect rules
/// still apply at the caller's HTTP client; this caps the buffered body).
pub const MAX_OEMBED_BYTES: u64 = 64 * 1024;

/// A fetched, sanitized remote oEmbed document: the pinned `OEmbedSerializer`
/// projection (`type`/`version` constants, author/provider facts, `cache_age`
/// 86400, safe `html`, `width` 400 default / `?width=` override, `height`
/// only when asked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEmbed {
    pub author_name: String,
    pub author_url: String,
    pub provider_name: String,
    pub provider_url: String,
    pub html: String,
    pub width: i64,
    pub height: Option<i64>,
}

/// Parse + sanitize a raw remote oEmbed document (`None` = not an oEmbed
/// `rich`/`video`/`photo` document with usable `html`). `width`/`height`
/// overrides come from the embeds request query (`?width=`/`?height=`).
pub fn parse_remote_embed(
    raw: &serde_json::Value,
    width_override: Option<i64>,
    height_override: Option<i64>,
) -> Option<RemoteEmbed> {
    let kind = raw.get("type").and_then(|t| t.as_str())?;
    if !matches!(kind, "rich" | "video" | "photo" | "link") {
        return None;
    }
    let html = raw.get("html").and_then(|h| h.as_str()).unwrap_or_default();
    if html.is_empty() || html.len() as u64 > MAX_OEMBED_BYTES {
        return None;
    }
    let width = width_override
        .filter(|w| (1..=2048).contains(w))
        .or_else(|| raw.get("width").and_then(|w| w.as_i64()).filter(|w| (1..=2048).contains(w)))
        .unwrap_or(400);
    let height = height_override
        .filter(|h| (1..=2048).contains(h))
        .or_else(|| raw.get("height").and_then(|h| h.as_i64()).filter(|h| (1..=2048).contains(h)));
    Some(RemoteEmbed {
        author_name: raw.get("author_name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        author_url: raw.get("author_url").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        provider_name: raw.get("provider_name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        provider_url: raw.get("provider_url").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        html: sanitize_embed_html(html),
        width,
        height,
    })
}

impl RemoteEmbed {
    /// Render the pinned oEmbed JSON body.
    pub fn into_json(self) -> serde_json::Value {
        serde_json::json!({
            "type": "rich",
            "version": "1.0",
            "author_name": self.author_name,
            "author_url": self.author_url,
            "provider_name": self.provider_name,
            "provider_url": self.provider_url,
            "cache_age": 86400,
            "html": self.html,
            "width": self.width,
            "height": self.height,
        })
    }
}

/// Fetch error of a remote oEmbed document: terminal (the embeds route
/// answers its fetch-failure 404) or transient (the caller may retry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedFetchError {
    Terminal(String),
    Transient(String),
}

/// Fetch a remote oEmbed document through an injected fetcher (fakes at the
/// HTTP-client boundary in tests; the route passes the policy-checked
/// client). `fetcher` returns `(status, body)`; non-2xx is terminal (a
/// missing remote embed never retries in a loop), oversize bodies are
/// terminal, transport failures are transient. The returned JSON is parsed +
/// sanitized by [`parse_remote_embed`]; unparseable bodies are terminal.
pub async fn fetch_remote_embed<F, Fut, E>(
    fetcher: F,
    embed_url: &str,
    width_override: Option<i64>,
    height_override: Option<i64>,
) -> Result<RemoteEmbed, EmbedFetchError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(u16, Vec<u8>), E>>,
    E: std::fmt::Display,
{
    if embed_url.len() > 2048 {
        return Err(EmbedFetchError::Terminal("embed url is too long".into()));
    }
    let (status, body) = fetcher(embed_url.to_string())
        .await
        .map_err(|e| EmbedFetchError::Transient(e.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(EmbedFetchError::Terminal(format!("remote embed answered {status}")));
    }
    if body.len() as u64 > MAX_OEMBED_BYTES {
        return Err(EmbedFetchError::Terminal("remote embed is larger than the limit".into()));
    }
    let raw: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| EmbedFetchError::Terminal(e.to_string()))?;
    parse_remote_embed(&raw, width_override, height_override)
        .ok_or_else(|| EmbedFetchError::Terminal("remote embed is not a usable oEmbed document".into()))
}

/// Sanitize remote embed HTML: untrusted markup rendered in a client WebView.
/// Drops `script`/`style`/`iframe`/`object`/`embed`/`form`/`link`/`meta`
/// elements with their content, HTML comments, `on*` event-handler
/// attributes and `javascript:`/`data:`/`vbscript:` URLs. Everything else
/// passes through byte-identical (this is a safety filter, not a renderer).
pub fn sanitize_embed_html(html: &str) -> String {
    const DROP_ELEMENTS: [&str; 8] = ["script", "style", "iframe", "object", "embed", "form", "link", "meta"];
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    let mut drop_depth: usize = 0;
    let mut drop_tag: String = String::new();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // HTML comments are never rendered.
            if html[i..].starts_with("<!--") {
                if let Some(end) = html[i..].find("-->") {
                    i += end + 3;
                    continue;
                }
                break;
            }
            let Some(tag_end) = html[i..].find('>') else {
                if drop_depth == 0 {
                    out.push_str(&html[i..]);
                }
                break;
            };
            let tag_text = &html[i + 1..i + tag_end];
            let is_close = tag_text.starts_with('/');
            let name: String = tag_text
                .trim_start_matches('/')
                .split([' ', '\t', '\n', '\r', '/'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let self_closing = tag_text.trim_end().ends_with('/');
            if drop_depth > 0 {
                if !is_close && name == drop_tag && !self_closing {
                    drop_depth += 1;
                } else if is_close && name == drop_tag && drop_depth > 0 {
                    drop_depth -= 1;
                    if drop_depth == 0 {
                        drop_tag.clear();
                    }
                }
                i += tag_end + 1;
                continue;
            }
            if !is_close && DROP_ELEMENTS.contains(&name.as_str()) && !self_closing {
                drop_depth = 1;
                drop_tag = name;
                i += tag_end + 1;
                continue;
            }
            if !is_close && DROP_ELEMENTS.contains(&name.as_str()) {
                i += tag_end + 1;
                continue;
            }
            if is_close {
                out.push('<');
                out.push_str(tag_text);
                out.push('>');
            } else {
                out.push('<');
                out.push_str(&sanitize_tag_attributes(tag_text));
                out.push('>');
            }
            i += tag_end + 1;
        } else {
            // Text run to the next tag, kept byte-identical (UTF-8 safe).
            let next = html[i..].find('<').map(|k| i + k).unwrap_or(bytes.len());
            if drop_depth == 0 {
                out.push_str(&html[i..next]);
            }
            i = next;
        }
    }
    out
}

/// Strip event-handler attributes and dangerous URL schemes from one open tag.
fn sanitize_tag_attributes(tag_text: &str) -> String {
    let mut out = String::new();
    let bytes = tag_text.as_bytes();
    let mut i = 0usize;
    // Tag name first, verbatim.
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
        out.push(bytes[i] as char);
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            out.push(bytes[i] as char);
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = tag_text[name_start..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value: Option<String> = None;
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let value_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                value = Some(tag_text[value_start..i].to_string());
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = Some(tag_text[value_start..i].to_string());
            }
        }
        // Event handlers (`onclick`, ...) never survive.
        if name.to_ascii_lowercase().starts_with("on") && name.len() > 2 {
            continue;
        }
        // URL attributes with a dangerous scheme are dropped with their value.
        if matches!(name.to_ascii_lowercase().as_str(), "href" | "src" | "action" | "xlink:href" | "cite" | "data")
            && let Some(ref v) = value
            && let Some(scheme) = v.split(':').next()
            && matches!(
                scheme.trim().to_ascii_lowercase().as_str(),
                "javascript" | "data" | "vbscript"
            )
            && v.contains(':')
        {
            continue;
        }
        out.push(' ');
        out.push_str(&name);
        if let Some(v) = value {
            out.push_str("=\"");
            out.push_str(&v.replace('"', "&quot;"));
            out.push('"');
        }
    }
    out
}

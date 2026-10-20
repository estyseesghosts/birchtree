//! Wire shapes for the objects and activities of the Phase 6 set that the
//! crate does not define: `Note`, the tags and attachments inside it, the
//! `QuoteRequest` activity, and the collection pages of a thread. The crate's
//! helpers (`deserialize_one_or_many`) are used for the one-or-many fields.
//!
//! Field names follow Mastodon 4.7.3 (`app/serializers/activitypub/*`).

use std::collections::BTreeMap;

use activitypub_federation::kinds::object::NoteType;
use activitypub_federation::protocol::helpers::deserialize_one_or_many;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::domain::Visibility;

/// The ActivityStreams public collection.
pub const PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";

/// A `Note`: a status on the wire.
///
/// A vote on a poll is also a `Note`: it carries `name` (the chosen option's
/// title) and `inReplyTo` (the poll's URI) with empty `content`. The inbox
/// detects that shape before ingesting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    #[serde(rename = "type")]
    pub kind: NoteType,
    pub id: Url,
    pub attributed_to: Url,
    #[serde(default)]
    pub content: String,
    /// The chosen option's title, on a vote `Note` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<Url>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub to: Vec<Url>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub cc: Vec<Url>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub tag: Vec<Tag>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub attachment: Vec<Attachment>,
    /// The quoted status (FEP-044f). `quoteUri` and `_misskey_quote` are read too.
    #[serde(default, alias = "quoteUri", alias = "_misskey_quote", skip_serializing_if = "Option::is_none")]
    pub quote: Option<Url>,
    /// The authorization URI, present once the quote is accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_authorization: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_map: Option<BTreeMap<String, String>>,
    /// The human-readable page of the status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Url>,
    /// Replies, as a collection URI or an inline collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replies: Option<serde_json::Value>,
}

impl Note {
    /// The status language: the single `contentMap` key, if there is one, in
    /// BCP47 canonical case. Upstream maps the downcased key through its
    /// locale table (`NORMALIZED_LOCALE_NAMES`, falling back to the raw key);
    /// without that catalog this canonicalizes the standard shapes directly
    /// (`EN` → `en`, `pt-br` → `pt-BR`), which agrees with the table on every
    /// standard tag and keeps unknown tags legible.
    pub fn language(&self) -> Option<String> {
        self.content_map.as_ref().and_then(|m| {
            if m.len() == 1 {
                m.keys().next().map(|key| canonical_language_tag(key))
            } else {
                None
            }
        })
    }

    /// The `actor` URIs of the Note's mentions, from its `Mention` tags.
    pub fn mention_uris(&self) -> Vec<String> {
        self.tag
            .iter()
            .filter(|t| t.kind == "Mention")
            .filter_map(|t| t.href.as_ref().map(|u| u.to_string()))
            .collect()
    }

    /// The hashtag names, without the `#`, normalized the way birchtree
    /// stores them (D25: NFKC-subset + lowercase + ASCII folding, so a
    /// remote `#café` lands on the same `cafe` tag as a local one),
    /// deduplicated preserving order.
    pub fn hashtags(&self) -> Vec<String> {
        crate::application::statuses::normalize_tag_list(
            &self
                .tag
                .iter()
                .filter(|t| t.kind == "Hashtag")
                .filter_map(|t| t.name.clone())
                .collect::<Vec<_>>(),
        )
    }

    /// The Note's visibility from its audience (`PostStatusService` / `ActivityPub::Parser::StatusParser#visibility`):
    /// public in `to`, unlisted in `cc`, followers-only when `to` names the author's
    /// followers collection exactly, otherwise direct. The expected collection
    /// is the author's own `{attributedTo}/followers` (see [`followers_uri_of`]).
    pub fn visibility(&self) -> Visibility {
        let public = |list: &[Url]| list.iter().any(|u| u.as_str() == PUBLIC);
        if public(&self.to) {
            Visibility::Public
        } else if public(&self.cc) {
            Visibility::Unlisted
        } else if {
            let expected = followers_uri_of(&self.attributed_to);
            self.to.iter().any(|u| u.as_str() == expected)
        } {
            Visibility::Private
        } else {
            Visibility::Direct
        }
    }
}

/// The followers collection URI for `actor` under the Mastodon-family
/// convention (`{actor_uri}/followers`): what this server advertises in its
/// actor documents ([`crate::activitypub::outgoing::followers_uri`]) and
/// what remote Mastodon-family servers advertise in theirs. Audience checks
/// compare against this exact string (upstream compares against the author's
/// `followers_collection`, `StatusParser#visibility`), never a bare
/// `ends_with("/followers")`, so `https://evil.example/users/alice/followers`
/// does not pass for `alice`.
pub fn followers_uri_of(actor: &Url) -> String {
    format!("{}/followers", actor.as_str().trim_end_matches('/'))
}

/// BCP47 canonical case for a language tag: the language subtag lowercase,
/// a two-letter region uppercase, a four-letter script titlecase, everything
/// else lowercase (`EN` → `en`, `pt-br` → `pt-BR`, `zh-hans-cn` → `zh-Hans-CN`).
pub fn canonical_language_tag(tag: &str) -> String {
    let mut parts = tag.split(['-', '_']);
    let mut out = String::new();
    if let Some(language) = parts.next() {
        out.push_str(&language.to_lowercase());
    }
    for part in parts {
        out.push('-');
        if part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()) {
            out.push_str(&part.to_uppercase());
        } else if part.len() == 4 && part.bytes().all(|b| b.is_ascii_alphabetic()) {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.push_str(&first.to_uppercase().to_string());
                out.push_str(&chars.as_str().to_lowercase());
            }
        } else {
            out.push_str(&part.to_lowercase());
        }
    }
    out
}

/// A tag: a `Mention` (with `href`), a `Hashtag` (with `name`) or an `Emoji`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Emoji image (`icon.url`; upstream `CustomEmojiParser` reads
    /// `icon.url` for the image and `id` for the emoji URI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<EmojiIcon>,
}

/// An `Emoji` tag's image. Only `url` is read; the tag `id` (emoji URI)
/// is not needed because rows are keyed `shortcode@domain`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmojiIcon {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Url>,
}

/// An attachment: a `Document` with its `mediaType` and `url`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, rename = "mediaType", skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blurhash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// The `type` of a `Question`: a literal string, checked on parse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuestionType;

impl Serialize for QuestionType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("Question")
    }
}

impl<'de> Deserialize<'de> for QuestionType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "Question" {
            Ok(QuestionType)
        } else {
            Err(serde::de::Error::custom(format!("expected Question, got {s}")))
        }
    }
}

/// One option of a `Question`: its title and its tally.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replies: Option<QuestionReplies>,
}

/// The tally of one `Question` option.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionReplies {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub total_items: u64,
}

/// A `Question`: a poll on the wire (Mastodon 4.7.3
/// `app/serializers/activitypub/note_serializer.rb`, Misskey's `Question`).
///
/// `oneOf` is a single-choice poll, `anyOf` a multiple-choice one. The expiry
/// is `endTime`, or `closed` once expired; both sides also emit and read
/// `votersCount`. `closed` is read liberally (a time, a boolean, millis),
/// because origins disagree on its shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    #[serde(rename = "type")]
    pub kind: QuestionType,
    pub id: Url,
    pub attributed_to: Url,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Url>,
    /// A poll that answers another status (rare, but Mastodon allows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<Url>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub to: Vec<Url>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub cc: Vec<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<QuestionOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<QuestionOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voters_count: Option<u64>,
}

impl Question {
    /// The poll's options: `anyOf` wins when present (Mastodon's `PollParser`).
    pub fn options(&self) -> &[QuestionOption] {
        self.any_of.as_deref().or(self.one_of.as_deref()).unwrap_or(&[])
    }

    /// True for an `anyOf` poll.
    pub fn multiple(&self) -> bool {
        self.any_of.is_some()
    }

    /// The expiry in ms, if either time field parses. A `closed: true` with no
    /// time carries no instant; the caller falls back to the publish time.
    pub fn expiry_ms(&self) -> Option<u64> {
        self.end_time
            .as_ref()
            .and_then(parse_time_value)
            .or_else(|| match self.closed.as_ref() {
                Some(serde_json::Value::Bool(true)) => None,
                Some(v) => parse_time_value(v),
                None => None,
            })
    }

    /// True when the `closed` field says the poll already ended. `closed`
    /// carries the end time on Akkoma, Misskey and pinetree — a time on its
    /// own only means "ended" when it is past — while a boolean `closed`
    /// is taken literally.
    pub fn closed_flag(&self, now_ms: u64) -> bool {
        match self.closed.as_ref() {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(v) => parse_time_value(v).is_some_and(|t| t <= now_ms),
            None => false,
        }
    }
}

/// A time as an RFC 3339 string or millis since the epoch.
pub fn parse_time_value(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.timestamp_millis().max(0) as u64),
        serde_json::Value::Number(n) => n.as_i64().and_then(|m| u64::try_from(m).ok()),
        _ => None,
    }
}

/// The `type` of a `QuoteRequest`: a literal string, checked on parse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuoteRequestType;

impl Serialize for QuoteRequestType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("QuoteRequest")
    }
}

impl<'de> Deserialize<'de> for QuoteRequestType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "QuoteRequest" {
            Ok(QuoteRequestType)
        } else {
            Err(serde::de::Error::custom(format!("expected QuoteRequest, got {s}")))
        }
    }
}

/// A `QuoteRequest`: the quoting author asks the quoted author's approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteRequest {
    #[serde(rename = "type")]
    pub kind: QuoteRequestType,
    pub id: Url,
    pub actor: Url,
    /// The quoted status.
    pub object: Url,
    /// The quoting status, inline or by URI.
    pub instrument: Instrument,
}

/// The `instrument` of a `QuoteRequest`: the quoting status itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Instrument {
    Note(Box<Note>),
    Question(Box<Question>),
    Id(Url),
}

impl Instrument {
    pub fn uri(&self) -> &Url {
        match self {
            Instrument::Note(note) => &note.id,
            Instrument::Question(question) => &question.id,
            Instrument::Id(url) => url,
        }
    }
}

/// A reference to an object: its URI, or an object that carries an `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ObjectRef {
    Id(Url),
    Object { id: Url },
}

impl ObjectRef {
    pub fn uri(&self) -> &Url {
        match self {
            ObjectRef::Id(url) => url,
            ObjectRef::Object { id } => id,
        }
    }
}

/// A status fetched over federation: a `Note`, or a `Question` when it
/// carries a poll. The untagged order matters: a `Question` body must not
/// parse as a `Note` (its `type` is strict), and a bare URI is an id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NoteOrQuestion {
    Note(Box<Note>),
    Question(Box<Question>),
}

/// One page of a `replies` collection: its `items`, and the next page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub items: Vec<Url>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub ordered_items: Vec<Url>,
}

impl CollectionPage {
    /// The URI of the page's next page, from `next` (a URI or an object with `id`).
    pub fn next_uri(&self) -> Option<Url> {
        match self.next.as_ref()? {
            serde_json::Value::String(s) => Url::parse(s).ok(),
            serde_json::Value::Object(o) => o.get("id")?.as_str().and_then(|s| Url::parse(s).ok()),
            _ => None,
        }
    }
}

/// `one_or_many` for URLs (an audience field).
pub fn one_or_many_urls<'de, D>(deserializer: D) -> Result<Vec<Url>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_one_or_many(deserializer)
}

/// `one_or_many` for object references (a Flag's `object`).
pub fn one_or_many_refs<'de, D>(deserializer: D) -> Result<Vec<ObjectRef>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_one_or_many(deserializer)
}

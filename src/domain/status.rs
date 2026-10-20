//! Statuses and their edit-history snapshots.
//!
//! Field names mirror the persistence/DTO contract (Mastodon v4.7.3
//! `REST::StatusSerializer` semantics). Counts live on the record and are
//! updated inside the same transaction as the interaction that changes them,
//! so a read never observes a counter that disagrees with its rows.

use serde::{Deserialize, Serialize};

use crate::domain::{Id, QuotePolicy, Visibility};

/// Application (OAuth client) metadata attached to a status at creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusApplication {
    pub name: String,
    pub website: Option<String>,
}

/// A status (post). Reblogs and quotes are separate records referencing this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub id: Id,
    pub account_id: Id,
    /// Canonical ActivityPub object URI.
    pub uri: String,
    /// Local web URL of the status.
    pub url: String,
    /// Raw source text as authored (pre-rendering).
    pub source_text: String,
    /// Sanitized/rendered HTML content.
    pub content_html: String,
    /// Creation time (ms since epoch).
    pub created_at_ms: u64,
    /// Last edit time; `None` while never edited.
    pub edited_at_ms: Option<u64>,
    /// Tombstone marker; a deleted status is excluded from every read surface.
    pub deleted_at_ms: Option<u64>,
    pub language: Option<String>,
    pub visibility: Visibility,
    pub sensitive: bool,
    pub spoiler_text: Option<String>,
    /// Direct parent status id for replies.
    pub in_reply_to_id: Option<Id>,
    /// Author of the direct parent (denormalized for direct-visibility rules).
    pub in_reply_to_account_id: Option<Id>,
    /// Original status id when this record is a reblog.
    pub reblog_of_id: Option<Id>,
    /// Attached media ids, in the author-chosen order.
    pub media_ids: Vec<Id>,
    /// Core extension CORE-P: attached place id (`None` = no place).
    /// Rides inline in the `statuses` JSON blob (schema v18); pre-v18 rows
    /// decode as `None`.
    #[serde(default)]
    pub place_id: Option<Id>,
    pub poll_id: Option<Id>,
    /// Who may quote this status (`public`/`followers`/`nobody`; forced to
    /// `nobody` for private/direct).
    pub quote_policy: QuotePolicy,
    /// Mentioned accounts, in mention order (serialized as `mentions`).
    pub mentions: Vec<Id>,
    /// Silent mentions (quote access grants); never serialized, used for
    /// direct-visibility authorization only.
    pub silent_mentions: Vec<Id>,
    /// Lowercased hashtags without the `#` prefix.
    pub tags: Vec<String>,
    /// Posting application metadata (client name/website).
    pub application: Option<StatusApplication>,
    pub replies_count: u64,
    pub reblogs_count: u64,
    pub favourites_count: u64,
    /// Accepted quotes of this status.
    pub quotes_count: u64,
}

impl Status {
    /// True when the record is a tombstone.
    pub fn is_deleted(&self) -> bool {
        self.deleted_at_ms.is_some()
    }

    /// True when this record is a reblog wrapper.
    pub fn is_reblog(&self) -> bool {
        self.reblog_of_id.is_some()
    }
}

/// One edit-history snapshot (`REST::StatusEditSerializer` source data).
///
/// Version `0` is the pre-edit snapshot taken at first edit (state as of
/// `created_at_ms`); later versions are appended per edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusVersion {
    pub status_id: Id,
    pub version: u64,
    pub account_id: Id,
    pub content_html: String,
    pub source_text: String,
    pub spoiler_text: Option<String>,
    pub sensitive: bool,
    pub media_ids: Vec<Id>,
    /// Poll option titles at snapshot time (for the edit serializer's
    /// `poll: { options: [...] }`).
    pub poll_options: Option<Vec<String>>,
    /// When this version was recorded (for version 0: the creation time).
    pub created_at_ms: u64,
}

//! Media attachment metadata.
//!
//! Attachment *relations* (ids, order, ownership) are Phase 3; byte upload,
//! processing lifecycle and serving are Phase 4. A status may only reference
//! attachments the author owns that are not yet attached elsewhere.
//!
//! Byte layout mirrors Mastodon's storage: an attachment owns an `original`
//! blob (its `file_key`), an optional `small` preview (images; `preview_key`)
//! and an optional operator-supplied cover image for video/audio
//! (`thumbnail_key`). File/object storage cannot participate in a `redb`
//! transaction, so bytes are staged *before* the metadata row commits
//! (`birchtree.md` §5).

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// Processing state of an attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaState {
    Pending,
    Processing,
    Ready,
    Failed,
}

/// Focus point (`focus` request param, stored inside `meta` like Mastodon).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FocusPoint {
    pub x: f32,
    pub y: f32,
}

/// A media attachment record (metadata; bytes live in the blob store).
///
/// Fields added in Phase 4 carry `#[serde(default)]` so Phase 3 rows decode
/// unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub id: Id,
    pub account_id: Id,
    /// One of `unknown`/`image`/`gifv`/`video`/`audio` (wire `type`).
    pub r#type: String,
    /// Public URL; `None` while not processed (serialized as `null`).
    pub url: Option<String>,
    pub state: MediaState,
    #[serde(default)]
    pub description: String,
    pub blurhash: Option<String>,
    /// Status the attachment is attached to; `None` while unattached.
    #[serde(default)]
    pub status_id: Option<Id>,
    /// Blob key of the original file (`000/000/042/original/<hex>.<ext>`);
    /// empty while unprocessed.
    #[serde(default)]
    pub file_key: String,
    /// Blob key of the generated `small` preview (images).
    #[serde(default)]
    pub preview_key: Option<String>,
    #[serde(default)]
    pub preview_width: Option<u32>,
    #[serde(default)]
    pub preview_height: Option<u32>,
    /// Blob key of an operator-supplied cover image (video/audio).
    #[serde(default)]
    pub thumbnail_key: Option<String>,
    /// Detected content type (`image/jpeg`, `video/mp4`, ...).
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub frame_rate: Option<String>,
    #[serde(default)]
    pub bitrate: Option<u64>,
    #[serde(default)]
    pub focus: Option<FocusPoint>,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
    /// Processing failure reason (surfaced as the Mastodon processing error).
    #[serde(default)]
    pub error: Option<String>,
    /// Core extension CORE-L: Pixelfed license id (1,5,6,11-16);
    /// `None` = all rights reserved / unspecified. Rides inline in the
    /// `media` JSON blob (schema v17); pre-v17 rows decode as `None`.
    #[serde(default)]
    pub license: Option<u8>,
}

impl MediaAttachment {
    /// True while the attachment still has no usable bytes (`queued`,
    /// `in_progress` or `failed` in Mastodon's model).
    pub fn not_processed(&self) -> bool {
        self.state != MediaState::Ready
    }

    /// Mastodon file-size limit class: images/GIFs 16 MB, video/audio larger
    /// formats 99 MB (`MediaAttachment::IMAGE_LIMIT` / `VIDEO_LIMIT`).
    pub fn larger_media_format(&self) -> bool {
        matches!(self.r#type.as_str(), "video" | "audio" | "gifv")
    }
}

//! Media attachment metadata.
//!
//! Attachment *relations* (ids, order, ownership) are Phase 3; byte upload,
//! processing lifecycle and serving are Phase 4. A status may only reference
//! attachments the author owns that are not yet attached elsewhere.

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

/// A media attachment record (metadata only; bytes live in the blob store).
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
}

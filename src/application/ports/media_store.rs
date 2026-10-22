use std::collections::HashMap;

use crate::domain::{Id, MediaAttachment, Result};

/// Media attachment metadata (bytes/lifecycle are Phase 4).
#[derive(Debug, Clone)]
pub struct NewMedia {
    pub account_id: Id,
    /// One of `unknown`/`image`/`gifv`/`video`/`audio`.
    pub r#type: String,
    pub description: String,
    /// Core extension CORE-L: Pixelfed license id (validated before the
    /// store call; `None` = all rights reserved). Already policy-gated by
    /// the caller (`off` stores `None`).
    pub license: Option<u8>,
}

/// An uploaded attachment whose bytes are already staged in the blob store.
///
/// The store commits metadata + the `ProcessMedia` job in ONE transaction; the
/// job finishes processing after commit (`birchtree.md` §5).
#[derive(Debug, Clone)]
pub struct NewMediaUpload {
    pub account_id: Id,
    /// Wire `type`: `image`/`video`/`audio`/`gifv`.
    pub r#type: String,
    pub mime_type: String,
    pub description: String,
    pub file_size: u64,
    /// Blob key of the staged original.
    pub file_key: String,
    /// Cover image blob key (video/audio).
    pub thumbnail_key: Option<String>,
    /// Focus point (`focus` param).
    pub focus: Option<crate::domain::FocusPoint>,
    /// Delayed processing (v2 create for larger formats): the attachment stays
    /// `Pending` until the `ProcessMedia` job completes.
    pub delay: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub bitrate: Option<u64>,
    /// Core extension CORE-L: Pixelfed license id (validated before the
    /// store call; `None` = all rights reserved). Already policy-gated by
    /// the caller (`off` stores `None`).
    pub license: Option<u8>,
}

/// Processing result committed by the `ProcessMedia` job (idempotent).
#[derive(Debug, Clone)]
pub struct MediaProcessingResult {
    /// `Ready` (or `Failed` with `error`).
    pub state: crate::domain::MediaState,
    pub r#type: String,
    pub blurhash: Option<String>,
    /// Blob key of the generated `small` preview.
    pub preview_key: Option<String>,
    pub preview_width: Option<u32>,
    pub preview_height: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub bitrate: Option<u64>,
    pub error: Option<String>,
}

/// Owner-scoped media patch (`PUT /api/v1/media/:id`).
#[derive(Debug, Clone, Default)]
pub struct MediaPatch {
    pub description: Option<String>,
    /// `Some(None)` clears the focus point; `None` leaves it unchanged.
    pub focus: Option<Option<crate::domain::FocusPoint>>,
    /// Replace the cover image blob key (`None` leaves it unchanged).
    pub thumbnail_key: Option<Option<String>>,
    /// Core extension CORE-L: replace the license (`None` leaves it
    /// unchanged; `Some(None)` clears to all-rights).
    pub license: Option<Option<u8>>,
}

pub trait MediaStore: Send + Sync {
    fn create_media(&self, input: NewMedia) -> Result<MediaAttachment>;
    /// The subset of `ids` owned by `owner_id` and unattached (or already
    /// attached to `status_id`), returned in the requested order.
    fn attachable_media(
        &self,
        owner_id: Id,
        ids: &[Id],
        status_id: Option<Id>,
    ) -> Result<Vec<MediaAttachment>>;
    fn media_by_id(&self, id: Id) -> Result<Option<MediaAttachment>>;

    /// Attachments by id in ONE transaction (page loader; missing ids omitted,
    /// duplicates read once).
    fn media_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, MediaAttachment>>;

    /// Insert an uploaded attachment's metadata + `ProcessMedia` job in ONE
    /// transaction.
    fn create_media_upload(&self, input: NewMediaUpload, now_ms: u64) -> Result<MediaAttachment>;

    /// The owner's media row (Mastodon `set_media_attachment`: only the
    /// owner's own, not-yet-attached rows are addressable; others are `None`
    /// → 404).
    fn owner_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>>;

    /// Apply an owner-scoped patch (description/focus/thumbnail).
    fn update_media(
        &self,
        account_id: Id,
        id: Id,
        patch: MediaPatch,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>>;

    /// Delete an owner's unattached attachment (bytes are removed by the
    /// caller after commit). `None` when missing/foreign; the use case
    /// rejects attached rows with the Mastodon in-usage error.
    fn delete_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>>;

    /// Commit a processing result (idempotent; `None` when the row is gone).
    fn mark_media_processed(
        &self,
        id: Id,
        result: MediaProcessingResult,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>>;
}

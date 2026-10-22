//! Media upload inputs (S0 decomposition of [`crate::application::media`]).
//!
//! This module owns the caller-supplied data for the upload use cases: the
//! uploaded bytes themselves ([`UploadFile`]) and the surrounding scalar
//! parameters bundled as [`UploadInput`].
//!
//! Both types are borrow-only views over the HTTP layer's buffers: they copy
//! no bytes and own no files. [`UploadFile::Spooled`] borrows the spool the
//! multipart handler already wrote to disk so the use case can publish it
//! onto its blob key with one rename instead of copying.
//!
//! The definitions here are canonical. The identical `UploadFile` in
//! [`crate::application::media`] is kept until S4 removes it; the parent
//! module handles any re-export.

use crate::domain::Id;
use crate::media::transcode::TranscodeConfig;
use crate::media::upload_spool::SNIFF_PREFIX_BYTES;
use crate::media::upload_spool::UploadSpool;

/// The uploaded file: resident bytes, or a capped spool file the HTTP layer
/// already wrote to disk (published onto its blob key with one rename).
pub enum UploadFile<'upload> {
    /// In-memory request bytes (non-streaming callers and tests).
    Bytes(&'upload [u8]),
    /// A spool file the streaming HTTP path wrote under the byte cap.
    Spooled(&'upload mut UploadSpool),
}

impl UploadFile<'_> {
    /// Total upload length in bytes, without materializing spooled bytes.
    pub fn len(&self) -> u64 {
        match self {
            UploadFile::Bytes(bytes) => bytes.len() as u64,
            UploadFile::Spooled(spool) => spool.len(),
        }
    }

    /// Leading bytes used for content sniffing (at most
    /// [`SNIFF_PREFIX_BYTES`]); the declared multipart content type is never
    /// trusted, so sniffing reads only this prefix.
    pub fn prefix(&self) -> &[u8] {
        match self {
            UploadFile::Bytes(bytes) => &bytes[..bytes.len().min(SNIFF_PREFIX_BYTES)],
            UploadFile::Spooled(spool) => spool.prefix(),
        }
    }
}

/// Borrowed inputs for the upload use cases (`upload_media`,
/// `upload_media_with_config`, `upload_media_spooled` in
/// [`crate::application::media`]).
///
/// Every reference-typed field borrows from the caller: the struct performs
/// no allocation and imposes no ownership transfer, so the same buffers can
/// be reused whether the upload finishes inline or defers (`delay`) to the
/// `ProcessMedia` job.
pub struct UploadInput<'a> {
    /// Wall-clock time of the request, in milliseconds since the Unix epoch.
    pub now_ms: u64,
    /// Account performing the upload; becomes the attachment owner.
    pub account_id: Id,
    /// The uploaded bytes: resident or already spooled to disk.
    pub file: UploadFile<'a>,
    /// Optional operator-supplied cover image bytes (validated as an image).
    pub thumbnail_bytes: Option<&'a [u8]>,
    /// Content warning / alt text for the attachment.
    pub description: &'a str,
    /// Optional focal point (`"x,y"`, both floats) for image cropping.
    pub focus: Option<&'a str>,
    /// When true, video/audio on the v2 endpoint stay `Pending` (answer 202)
    /// until the `ProcessMedia` job runs instead of finishing inline.
    pub delay: bool,
    /// Configured upload size cap in bytes (images and video/audio alike).
    pub max_file_size: u64,
    /// Transcode knobs; `None` (or disabled) keeps the legacy lifecycle.
    pub config: Option<&'a TranscodeConfig>,
    /// Operator license-attachment policy (`"allow"`, `"required"`, ...).
    pub license_policy: &'a str,
    /// License id when the caller attaches one.
    pub license: Option<i64>,
    /// Operator alt-text policy enforced against `description`.
    pub alt_text_policy: &'a str,
}

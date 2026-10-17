//! Media use cases (Phase 4): upload, processing lifecycle, updates, deletion
//! and byte serving.

use crate::application::ports::{BlobStore, Store};
use crate::domain::{Error, Id, Result};

/// Execute a `ProcessMedia` job: finish processing staged bytes and commit
/// the result (idempotent). Implemented by the media slice; the durable job
/// envelope is already wired.
pub fn process_media_job(
    _store: &dyn Store,
    _blobs: &dyn BlobStore,
    media_id: Id,
    _now_ms: u64,
) -> Result<()> {
    let _ = media_id;
    Err(Error::Internal("process_media_job arrives with the media slice".into()))
}

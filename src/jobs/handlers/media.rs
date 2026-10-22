//! Media job handlers (S8 slice group B).
//!
//! Sync-drain side of `ProcessMedia` / `TranscodeMedia`. Both arms carry a
//! `{media_id}` payload and funnel through the same media finish. The worker
//! threads its toolchain via [`WorkerCtx`]; the sync drain historically
//! passed `None` and took the legacy pure-Rust finish (see rewire note).

use crate::application::ports::{BlobStore, Store};
use crate::domain::{Error, Id, Job, JobKind, Result};
use crate::media::transcode::TranscodeConfig;

/// Handle a media job with the worker context's store, blobs and toolchain.
///
/// Mirrors `handlers::handle_with_transcode` for [`JobKind::ProcessMedia`] and
/// [`JobKind::TranscodeMedia`]; any other kind is a caller error.
pub fn handle(
    job: &Job,
    store: &dyn Store,
    blobs: &dyn BlobStore,
    transcode: Option<&TranscodeConfig>,
) -> Result<()> {
    match job.kind {
        JobKind::ProcessMedia => {
            let media_id = job
                .payload
                .get("media_id")
                .and_then(|v| v.as_u64())
                .map(Id::new)
                .ok_or_else(|| Error::Internal("process_media: missing media_id".into()))?;
            crate::application::media::process_media_job_with_config(
                &*store,
                &*blobs,
                media_id,
                job.due_at_ms,
                transcode,
            )
        }
        JobKind::TranscodeMedia => {
            let media_id =
                crate::media::transcode::TranscodeJobPayload::media_id_from_json(&job.payload)
                    .map_err(|err| Error::Internal(err.to_string()))?;
            crate::application::media::process_media_job_with_config(
                &*store,
                &*blobs,
                media_id,
                job.due_at_ms,
                transcode,
            )
        }
        _ => Err(Error::Internal("media: unexpected job kind".into())),
    }
}

//! Phase 7.1 transcode job wiring.
//!
//! The live implementation lives in [`crate::media::transcode`]
//! ([`run_transcode_job`](crate::media::transcode::run_transcode_job) and
//! [`TranscodeJobPayload`](crate::media::transcode::TranscodeJobPayload)).
//! That module is declared from `media/mod.rs`, so it compiles without
//! touching the jobs tree. This file re-exports the payload/handler names
//! under `jobs::transcode`; `JobKind::TranscodeMedia` is dispatched by the
//! job worker (`worker.rs`, ffmpeg off the async runtime) and by the sync
//! drain (`handlers.rs`, through the config-aware media finish).

pub use crate::media::transcode::{run_transcode_job, TranscodeJobPayload};

//! Phase 7.1 transcode job wiring shim.
//!
//! The live implementation lives in [`crate::media::transcode`]
//! ([`run_transcode_job`](crate::media::transcode::run_transcode_job) and
//! [`TranscodeJobPayload`](crate::media::transcode::TranscodeJobPayload)).
//! That module is declared from `media/mod.rs`, so it compiles without
//! touching the jobs tree.
//!
//! Once the parent lands the `JobKind::TranscodeMedia` + handler-dispatch
//! patch (see PATCHES FOR PARENT in the Phase 7.1 report), declare
//! `pub mod transcode;` in `jobs/mod.rs`; this file then re-exports the
//! payload/handler names under `jobs::transcode`.

pub use crate::media::transcode::{run_transcode_job, TranscodeJobPayload};

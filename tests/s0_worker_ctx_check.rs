//! S0 check: `jobs::worker_ctx::WorkerCtx` bundles the worker resources.
//!
//! `src/jobs/mod.rs` is parent-owned (shared file), so this test cannot rely
//! on `pub mod worker_ctx;` being wired yet. It compiles the real file
//! in place via `#[path]` and proves, at compile time, that the struct
//! resolves, is `Clone`, and exposes every expected field — without
//! constructing a value (a full `WorkerCtx` needs a `Store`, a
//! `FederationConfig<FedContext>` and a runtime-built `WorkerExtra`, which is
//! S8 integration-test material, not a unit fixture).
//!
//! Once the parent wires the module, this file keeps passing unchanged; S8
//! may then replace the `#[path]` shim with `birchtree::jobs::worker_ctx`.

// Re-export the crate roots the module under test addresses as `crate::…`.
// (`worker_ctx.rs` uses absolute `crate::` paths, so the shim compiles it
// against these names.)
pub use birchtree::activitypub;
pub use birchtree::application;
pub use birchtree::domain;
pub use birchtree::jobs;

/// The real `src/jobs/worker_ctx.rs`, compiled here for type checking.
#[allow(dead_code)]
#[path = "../src/jobs/worker_ctx.rs"]
mod worker_ctx_under_test;

use worker_ctx_under_test::WorkerCtx;

fn assert_clone<T: Clone>() {}

#[test]
fn worker_ctx_is_clone() {
    assert_clone::<WorkerCtx>();
}

#[test]
fn worker_ctx_fields_accessible() {
    fn touch(ctx: &WorkerCtx) {
        let _ = &ctx.store;
        let _ = &ctx.clock;
        let _ = &ctx.mailer;
        let _ = &ctx.blobs;
        let _ = &ctx.streaming;
        let _ = &ctx.federation;
        let _ = &ctx.extra;
    }
    let _ = touch;
}

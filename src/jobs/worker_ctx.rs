//! Shared worker context for the durable-job pipeline (S0).
//!
//! [`WorkerCtx`] bundles the resources every job handler needs — store,
//! clock, mailer, blobs, streaming, federation config and the worker extras
//! — so `process_one` and `run` (see `jobs::worker`) take one context
//! argument instead of eight or more. Pure bundling: no behavior change.
//! S8 adopts this type in the worker and splits the per-kind handlers
//! around it.

use std::sync::Arc;

use activitypub_federation::config::FederationConfig;

use crate::activitypub::FedContext;
use crate::application::ports::BlobStore;
use crate::application::ports::MailSender;
use crate::application::ports::Store;
use crate::application::ports::StreamingSink;
use crate::domain::Clock;
use crate::jobs::worker::WorkerExtra;

/// Shared resources available to every durable-job handler.
///
/// Built once in `main.rs` from `Config`; handlers borrow it per job
/// (`process_one(ctx: &WorkerCtx, job: &Job)`). `Clone` is cheap (one `Arc`
/// bump per handle) so `run` can move a copy per task.
#[derive(Clone)]
pub struct WorkerCtx {
    /// Bundled persistence surface (backed by the `redb` adapter).
    pub store: Arc<dyn Store>,
    /// Wall-clock source for leases, backoff and retention prunes.
    pub clock: Arc<dyn Clock>,
    /// Outbound mail adapter (confirmation / password-reset delivery).
    pub mailer: Arc<dyn MailSender>,
    /// Blob/object storage for media bytes.
    pub blobs: Arc<dyn BlobStore>,
    /// Publishes committed changes to open streams.
    pub streaming: Arc<dyn StreamingSink>,
    /// Federation configuration handed to delivery callbacks.
    pub federation: FederationConfig<FedContext>,
    /// Worker-threaded async resources (push client, transcode, heartbeat).
    pub extra: WorkerExtra,
}

impl WorkerCtx {
    /// Bundle the worker resources into one context.
    pub fn new(
        store: Arc<dyn Store>,
        clock: Arc<dyn Clock>,
        mailer: Arc<dyn MailSender>,
        blobs: Arc<dyn BlobStore>,
        streaming: Arc<dyn StreamingSink>,
        federation: FederationConfig<FedContext>,
        extra: WorkerExtra,
    ) -> Self {
        Self {
            store,
            clock,
            mailer,
            blobs,
            streaming,
            federation,
            extra,
        }
    }
}

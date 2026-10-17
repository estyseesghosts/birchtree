use std::sync::Arc;

use crate::application::ports::{BlobStore, ContextDeps, Ports, Store, StreamingSink};
use crate::domain::{Clock, IdGenerator};

/// Dependency assembly only. No business logic lives here; use cases live in
/// the `accounts/`, `statuses/`, etc. submodules and receive the narrow ports
/// they need from the assembled [`Store`].
#[derive(Clone)]
pub struct Context {
    pub deps: ContextDeps,
    pub store: Arc<dyn Store>,
    /// Blob/object storage for media bytes and emoji images (Phase 4).
    pub blobs: Arc<dyn BlobStore>,
    /// Post-commit publication to open streams (Phase 5).
    pub streaming: Arc<dyn StreamingSink>,
}

impl Context {
    pub fn new(
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        streaming: Arc<dyn StreamingSink>,
    ) -> Self {
        Self {
            deps: ContextDeps { clock, ids },
            store,
            blobs,
            streaming,
        }
    }

    /// Milliseconds since the Unix epoch from the assembled clock.
    pub fn now_ms(&self) -> u64 {
        self.deps.clock.now_ms()
    }

    /// Generate the next identifier from the assembled generator.
    pub fn next_id(&self) -> crate::domain::Id {
        self.deps.ids.generate()
    }
}

impl Ports for Context {
    fn clock(&self) -> Arc<dyn Clock> {
        self.deps.clock.clone()
    }
    fn ids(&self) -> Arc<dyn IdGenerator> {
        self.deps.ids.clone()
    }
}

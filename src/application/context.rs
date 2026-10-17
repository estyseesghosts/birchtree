use std::sync::Arc;

use crate::application::ports::{ContextDeps, Ports, Store};
use crate::domain::{Clock, IdGenerator};

/// Dependency assembly only. No business logic lives here; use cases live in
/// the `accounts/`, `statuses/`, etc. submodules and receive the narrow ports
/// they need from the assembled [`Store`].
#[derive(Clone)]
pub struct Context {
    pub deps: ContextDeps,
    pub store: Arc<dyn Store>,
}

impl Context {
    pub fn new(
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        store: Arc<dyn Store>,
    ) -> Self {
        Self {
            deps: ContextDeps { clock, ids },
            store,
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

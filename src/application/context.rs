use std::sync::Arc;

use crate::application::ports::{ContextDeps, Ports};
use crate::domain::{Clock, IdGenerator};

/// Dependency assembly only. No business logic lives here; use cases live in
/// the `accounts/`, `statuses/`, etc. submodules.
#[derive(Clone)]
pub struct Context {
    pub deps: ContextDeps,
}

impl Context {
    pub fn new(clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self {
            deps: ContextDeps { clock, ids },
        }
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

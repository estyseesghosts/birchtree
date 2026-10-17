use std::sync::Arc;

pub use crate::domain::{Clock, IdGenerator};

/// Dependency-assembly surface for adapters. Each trait is implemented by the
/// `redb` store (or a filesystem/media adapter) and injected into use cases.
pub trait Ports: Send + Sync {
    fn clock(&self) -> Arc<dyn Clock>;
    fn ids(&self) -> Arc<dyn IdGenerator>;
}

/// Snapshot of all dependencies assembled in `application::context`.
#[derive(Clone)]
pub struct ContextDeps {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
}

impl Ports for ContextDeps {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
    fn ids(&self) -> Arc<dyn IdGenerator> {
        self.ids.clone()
    }
}

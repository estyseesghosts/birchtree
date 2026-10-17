//! Job queue surface.
//!
//! The queue itself is the persistence port (`application::ports::JobQueue`)
//! implemented by the `redb` adapter; this module only re-exports the shared
//! scheduling constants so the worker and tests agree on them.

pub use crate::domain::job::{backoff_ms, BACKOFF_BASE_MS, BACKOFF_CAP_MS, LEASE_MS};

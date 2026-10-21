//! Structured logging and secret redaction.
//!
//! Logging is `tracing`-based. The default filter is `info`; set `RUST_LOG` to
//! override. Secret-bearing values must pass through [`redact`] before being
//! logged.

use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber (idempotent).
pub fn init() {
    init_with_default("info");
}

/// Initialize with an explicit fallback filter.
pub fn init_with_default(default: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

/// Redact an `Authorization` header value, preserving the scheme only.
pub fn redact_authorization(value: &str) -> String {
    match value.split_once(' ') {
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("bearer") => "Bearer [redacted]".to_string(),
        _ => "[redacted]".to_string(),
    }
}

/// Redact a secret value entirely.
pub fn redact(_secret: &str) -> &'static str {
    "[redacted]"
}

/// Storage-overhaul counters (Agent C, `docs/database.md`): bytes used,
/// per-class breakdown, hits/misses, bytes fetched, evictions, failed
/// origins. Atomics, no shell, no commands. Interim channel is the log +
/// `/health` snapshot below; admin-visible metrics stay deferred to
/// Phase 7.3 (no admin endpoint exists before then).
pub struct StorageMetrics {
    /// Stored bytes across both pools (durable + cache).
    pub bytes_used: std::sync::atomic::AtomicU64,
    /// Stored bytes per class: [avatar/emoji, preview, full].
    pub bytes_per_class: [std::sync::atomic::AtomicU64; 3],
    pub hits: std::sync::atomic::AtomicU64,
    pub misses: std::sync::atomic::AtomicU64,
    pub bytes_fetched: std::sync::atomic::AtomicU64,
    pub evictions: std::sync::atomic::AtomicU64,
    pub failed_origins: std::sync::atomic::AtomicU64,
}

/// Plain snapshot of [`StorageMetrics`] for `/health` rendering.
#[derive(Debug, Clone, Copy, Default)]
pub struct StorageSnapshot {
    pub bytes_used: u64,
    pub bytes_per_class: [u64; 3],
    pub hits: u64,
    pub misses: u64,
    pub bytes_fetched: u64,
    pub evictions: u64,
    pub failed_origins: u64,
}

impl StorageMetrics {
    pub const fn new() -> Self {
        use std::sync::atomic::AtomicU64;
        Self {
            bytes_used: AtomicU64::new(0),
            bytes_per_class: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            bytes_fetched: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            failed_origins: AtomicU64::new(0),
        }
    }

    /// Relaxed loads into a plain snapshot (interim `/health` channel).
    pub fn snapshot(&self) -> StorageSnapshot {
        use std::sync::atomic::Ordering;
        StorageSnapshot {
            bytes_used: self.bytes_used.load(Ordering::Relaxed),
            bytes_per_class: [
                self.bytes_per_class[0].load(Ordering::Relaxed),
                self.bytes_per_class[1].load(Ordering::Relaxed),
                self.bytes_per_class[2].load(Ordering::Relaxed),
            ],
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            bytes_fetched: self.bytes_fetched.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            failed_origins: self.failed_origins.load(Ordering::Relaxed),
        }
    }
}

pub static STORAGE_METRICS: StorageMetrics = StorageMetrics::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_is_redacted() {
        assert_eq!(redact_authorization("Bearer abc.def"), "Bearer [redacted]");
        assert_eq!(redact_authorization("Basic dXNlcjpwYXNz"), "[redacted]");
        assert_eq!(redact("super-secret"), "[redacted]");
    }
}

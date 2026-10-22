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
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("bearer") => {
            "Bearer [redacted]".to_string()
        }
        _ => "[redacted]".to_string(),
    }
}

/// Redact a secret value entirely.
pub fn redact(_secret: &str) -> &'static str {
    "[redacted]"
}

/// Path of a request URI without the query string.
///
/// Confirmation (`?confirmation_token=…`) and password-reset
/// (`?reset_password_token=…`) bearers travel in the query, so any span,
/// log line or proxy record that captures the full URI leaks a one-time
/// secret. Always record [`redacted_path`] instead of the full URI.
#[must_use]
pub fn redacted_path(uri: &axum::http::Uri) -> &str {
    uri.path()
}

/// `tower-http` span factory that records method + path only, never the
/// query string.
///
/// `TraceLayer::new_for_http()` spans the full URI (including the query) at
/// debug level, which would log confirmation/reset tokens. Wire this via
/// `TraceLayer::new_for_http().make_span_with(RedactedHttpMakeSpan::default())`
/// (see `api/mastodon/router.rs`).
///
/// Proxy note (operator docs): any reverse proxy in front of this server must
/// likewise strip or redact the query string before logging request lines —
/// `%r`/`$request_uri`-style full-URI access logs reintroduce the same token
/// leak this span removes.
#[derive(Debug, Clone, Default)]
pub struct RedactedHttpMakeSpan;

impl<B> tower_http::trace::MakeSpan<B> for RedactedHttpMakeSpan {
    fn make_span(&mut self, request: &axum::http::Request<B>) -> tracing::Span {
        tracing::debug_span!(
            "http_request",
            method = %request.method(),
            path = %redacted_path(request.uri()),
            version = ?request.version(),
        )
    }
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

    #[test]
    fn redacted_path_drops_the_query_string() {
        for (raw, path) in [
            (
                "/auth/confirmation?confirmation_token=secret-token",
                "/auth/confirmation",
            ),
            (
                "/auth/password/edit?reset_password_token=secret-token",
                "/auth/password/edit",
            ),
            ("/api/v1/accounts?limit=20", "/api/v1/accounts"),
            ("/health", "/health"),
        ] {
            let uri: axum::http::Uri = raw.parse().expect("test uri");
            assert_eq!(redacted_path(&uri), path, "{raw}");
            assert!(!redacted_path(&uri).contains("secret-token"), "{raw}");
        }
    }

    #[test]
    fn redacted_span_factory_builds_without_the_query() {
        use std::io::Write;
        use tower_http::trace::MakeSpan as _;

        #[derive(Debug, Clone, Default)]
        struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl Write for Captured {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("capture").extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let request = axum::http::Request::builder()
                .method("GET")
                .uri("/auth/confirmation?confirmation_token=secret-token")
                .body(())
                .expect("test request");
            let span = RedactedHttpMakeSpan.make_span(&request);
            let _entered = span.enter();
            tracing::debug!("probe event inside the span");
        });
        let text = String::from_utf8(captured.0.lock().expect("capture").clone())
            .expect("tracing output is utf-8");
        assert!(text.contains("/auth/confirmation"), "{text}");
        assert!(!text.contains("secret-token"), "token leaked into {text}");
        assert!(
            !text.contains("confirmation_token"),
            "query key leaked into {text}"
        );
    }
}

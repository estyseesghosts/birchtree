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

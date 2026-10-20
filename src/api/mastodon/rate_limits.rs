//! Rate limiting and `X-RateLimit-*` headers.
//!
//! Mastodon (via Rack::Attack) exposes `X-RateLimit-Limit`,
//! `X-RateLimit-Remaining` and `X-RateLimit-Reset` and returns HTTP 429 with
//! `{ "error": "Too many requests" }` when a throttle trips. This module
//! implements a bounded in-memory fixed-window limiter with the same header
//! shape. It is a per-process limiter; multi-node deployment would move it to
//! the storage adapter.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Default API throttle: 1500 requests / 5 minutes (Mastodon's authenticated
/// API throttle).
pub const DEFAULT_LIMIT: u32 = 1_500;
/// Default window in seconds.
pub const DEFAULT_PERIOD_SECS: u64 = 300;

/// The rate-limit state surfaced to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    /// Unix seconds at which the current window resets.
    pub reset_at_s: i64,
}

/// A bounded fixed-window limiter.
#[derive(Debug, Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<String, (i64, u32)>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count a request for `key` and return the remaining budget. The window
    /// resets when its start time plus the period has elapsed.
    pub fn check(&self, key: &str, limit: u32, period_secs: u64, now_s: i64) -> RateLimit {
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        // Clamp the map before taking the entry so a hostile client cannot grow
        // it without bound (and so the borrow checker sees a single borrow).
        if !windows.contains_key(key) && windows.len() > 100_000 {
            windows.clear();
        }
        let entry = windows.entry(key.to_string()).or_insert((now_s, 0));
        let reset_at = entry.0 + period_secs as i64;
        if now_s >= reset_at {
            *entry = (now_s, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        let used = entry.1;
        let window_start = entry.0;
        RateLimit {
            limit,
            remaining: limit.saturating_sub(used),
            reset_at_s: window_start + period_secs as i64,
        }
    }
}

/// The process-wide limiter used by [`middleware`].
fn global() -> &'static RateLimiter {
    static LIMITER: OnceLock<RateLimiter> = OnceLock::new();
    LIMITER.get_or_init(RateLimiter::new)
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn client_key(req: &Request) -> String {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Middleware: count the request, attach `X-RateLimit-*`, and return 429 when
/// the budget is exhausted. `multiplier` scales the limit (1 in production).
pub async fn middleware(State(multiplier): State<u32>, req: Request, next: Next) -> Response {
    let key = client_key(&req);
    let limit = DEFAULT_LIMIT.saturating_mul(multiplier.max(1));
    let rl = global().check(&key, limit, DEFAULT_PERIOD_SECS, now_s());
    if rl.remaining == 0 {
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(json!({ "error": "Too many requests" })),
        )
            .into_response();
        apply_headers(response.headers_mut(), rl);
        return response;
    }
    let mut response = next.run(req).await;
    apply_headers(response.headers_mut(), rl);
    response
}

fn apply_headers(headers: &mut axum::http::HeaderMap, rl: RateLimit) {
    headers.insert(
        HeaderName::from_static("x-ratelimit-limit"),
        HeaderValue::from_str(&rl.limit.to_string()).unwrap(),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderValue::from_str(&rl.remaining.to_string()).unwrap(),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-reset"),
        HeaderValue::from_str(&rl.reset_at_s.to_string()).unwrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_counts_and_resets() {
        let limiter = RateLimiter::new();
        let a = limiter.check("ip", 2, 60, 1000);
        assert_eq!(a.remaining, 1);
        let b = limiter.check("ip", 2, 60, 1001);
        assert_eq!(b.remaining, 0);
        // Same window: still exhausted.
        let c = limiter.check("ip", 2, 60, 1002);
        assert_eq!(c.remaining, 0);
        // Next window resets.
        let d = limiter.check("ip", 2, 60, 1060);
        assert_eq!(d.remaining, 1);
        assert_eq!(d.reset_at_s, 1120);
    }

    #[test]
    fn keys_are_isolated() {
        let limiter = RateLimiter::new();
        limiter.check("a", 1, 60, 0);
        let b = limiter.check("b", 1, 60, 0);
        assert_eq!(b.remaining, 0);
    }
}

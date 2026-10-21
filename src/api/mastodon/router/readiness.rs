//! Readiness checks and bounded operational metrics.
//!
//! `/health` stays pure liveness. `/ready` runs every registered check and
//! answers a bare `ready` / `not ready`; which check failed is logged, never
//! returned. `/metrics` renders a fixed, small set of series (no per-user or
//! per-path labels) and only answers loopback peers.
//!
//! The runtime registers its probes at startup through [`RuntimeProbes`]:
//! storage write capability, worker heartbeat freshness, writability of the
//! durable and cache media pools, plus gauges such as queue backlog and age.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::rate_limits::RateLimitState;
use crate::observability::STORAGE_METRICS;

/// Upper bound for the whole readiness check run.
const CHECK_DEADLINE: Duration = Duration::from_secs(2);

type CheckFn = Arc<dyn Fn() -> bool + Send + Sync>;
type GaugeFn = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Named readiness checks and metric gauges registered by the runtime.
///
/// Names are `'static` so the series set is fixed at compile time.
#[derive(Clone, Default)]
pub struct RuntimeProbes {
    checks: Vec<(&'static str, CheckFn)>,
    gauges: Vec<(&'static str, GaugeFn)>,
}

impl RuntimeProbes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a check that must return true for the node to be ready.
    pub fn with_check(
        mut self,
        name: &'static str,
        check: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.checks.push((name, Arc::new(check)));
        self
    }

    /// Register a gauge rendered by `/metrics`.
    pub fn with_gauge(
        mut self,
        name: &'static str,
        gauge: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        self.gauges.push((name, Arc::new(gauge)));
        self
    }

    /// Run every check; the names of the failing ones.
    pub fn failing_checks(&self) -> Vec<&'static str> {
        let mut failed: Vec<&'static str> = Vec::new();
        for (name, check) in &self.checks {
            if !check() {
                failed.push(name);
            }
        }
        failed
    }
}

/// Liveness stamp of the background worker: it calls [`WorkerHeartbeat::beat`]
/// every loop iteration (idle or busy); readiness requires a recent beat.
#[derive(Debug)]
pub struct WorkerHeartbeat {
    last_beat_ms: AtomicU64,
}

impl Default for WorkerHeartbeat {
    fn default() -> Self {
        Self {
            last_beat_ms: AtomicU64::new(now_ms()),
        }
    }
}

impl WorkerHeartbeat {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record progress now.
    pub fn beat(&self) {
        self.last_beat_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Milliseconds since the last beat.
    pub fn age_ms(&self) -> u64 {
        now_ms().saturating_sub(self.last_beat_ms.load(Ordering::Relaxed))
    }

    /// Whether the worker made progress within `max_age`.
    pub fn is_fresh(&self, max_age: Duration) -> bool {
        u128::from(self.age_ms()) <= max_age.as_millis()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A check that proves `dir` accepts a create + write + delete.
pub fn writable_directory_check(dir: PathBuf) -> impl Fn() -> bool + Send + Sync + 'static {
    move || {
        let probe = dir.join(format!(".ready-probe-{}", std::process::id()));
        let wrote = std::fs::write(&probe, b"ok").is_ok();
        let removed = std::fs::remove_file(&probe).is_ok();
        wrote && removed
    }
}

/// `GET /ready`: storage read, plus every registered probe, within a deadline.
pub async fn ready(
    State(state): State<ApiState>,
    Extension(probes): Extension<RuntimeProbes>,
) -> (StatusCode, &'static str) {
    if !state.context.store.is_ready() {
        return (StatusCode::SERVICE_UNAVAILABLE, "not ready");
    }
    let run = tokio::task::spawn_blocking(move || probes.failing_checks());
    match tokio::time::timeout(CHECK_DEADLINE, run).await {
        Ok(Ok(failed)) if failed.is_empty() => (StatusCode::OK, "ready"),
        Ok(Ok(failed)) => {
            tracing::warn!(failed_checks = ?failed, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready")
        }
        Ok(Err(_)) | Err(_) => {
            tracing::warn!("readiness checks did not complete");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready")
        }
    }
}

/// `GET /metrics`: Prometheus text, loopback peers only (everyone else gets
/// the same 404 as an unknown path).
pub async fn metrics(
    peer: Option<ConnectInfo<SocketAddr>>,
    Extension(probes): Extension<RuntimeProbes>,
    Extension(limits): Extension<RateLimitState>,
) -> Response {
    let is_loopback = peer.is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback());
    if !is_loopback {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body: String = tokio::task::spawn_blocking(move || render_metrics(&probes, &limits))
        .await
        .unwrap_or_default();
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    response
}

fn render_metrics(probes: &RuntimeProbes, limits: &RateLimitState) -> String {
    let mut out = String::new();
    let storage = STORAGE_METRICS.snapshot();
    let limiter = limits.limiter().stats();
    let series: [(&str, u64); 11] = [
        ("birchtree_storage_bytes_used", storage.bytes_used),
        ("birchtree_storage_bytes_avatar_emoji", storage.bytes_per_class[0]),
        ("birchtree_storage_bytes_preview", storage.bytes_per_class[1]),
        ("birchtree_storage_bytes_full", storage.bytes_per_class[2]),
        ("birchtree_cache_hits_total", storage.hits),
        ("birchtree_cache_misses_total", storage.misses),
        ("birchtree_cache_evictions_total", storage.evictions),
        ("birchtree_cache_failed_origins_total", storage.failed_origins),
        ("birchtree_rate_limit_tracked_keys", limiter.tracked_keys as u64),
        ("birchtree_rate_limit_denied_total", limiter.denied_total),
        ("birchtree_rate_limit_overflow_total", limiter.overflow_total),
    ];
    for (name, value) in series {
        let _ = writeln!(out, "{name} {value}");
    }
    for (name, gauge) in &probes.gauges {
        let _ = writeln!(out, "birchtree_{name} {}", gauge());
    }
    for (name, check) in &probes.checks {
        let _ = writeln!(out, "birchtree_check_ok{{check=\"{name}\"}} {}", u8::from(check()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_checks_name_only_the_failures() {
        let probes = RuntimeProbes::new()
            .with_check("good", || true)
            .with_check("bad", || false);
        assert_eq!(probes.failing_checks(), vec!["bad"]);
    }

    #[test]
    fn heartbeat_goes_stale_and_recovers() {
        let heartbeat = WorkerHeartbeat::new();
        assert!(heartbeat.is_fresh(Duration::from_secs(5)));
        heartbeat.last_beat_ms.store(1, Ordering::Relaxed);
        assert!(!heartbeat.is_fresh(Duration::from_secs(5)));
        heartbeat.beat();
        assert!(heartbeat.is_fresh(Duration::from_secs(5)));
    }

    #[test]
    fn writable_directory_check_detects_missing_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = writable_directory_check(dir.path().to_path_buf());
        assert!(present());
        let missing = writable_directory_check(dir.path().join("missing"));
        assert!(!missing());
    }
}

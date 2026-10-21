//! Rate limiting and `X-RateLimit-*` headers.
//!
//! Mastodon (via Rack::Attack) exposes `X-RateLimit-Limit`,
//! `X-RateLimit-Remaining` and `X-RateLimit-Reset` and returns HTTP 429 with
//! `{ "error": "Too many requests" }` when a throttle trips. This module
//! implements a bounded in-memory fixed-window limiter with the same header
//! shape.
//!
//! # Client identity
//!
//! The identity is the TCP peer address. `X-Forwarded-For` is honoured only
//! when the peer is a configured trusted proxy; see [`resolve_client_ip`] for
//! the single parsing rule. IPv6 clients are keyed by their `/64`.
//!
//! # Limits
//!
//! Every request counts against the general budget. Sensitive routes
//! (password login, token issuance, registration, password reset, confirmation
//! resend, app creation) additionally count against a stricter per-route
//! budget with its own window, so one route being throttled never consumes
//! another route's allowance. Password-reset and confirmation-resend also
//! count per normalized address, whether or not the address belongs to an
//! account, so the 429 never reveals account existence.
//!
//! # Scope
//!
//! State is per process. A multi-process deployment would multiply every
//! effective limit by the replica count unless the state moves to shared
//! storage.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Default API throttle: 1500 requests / 5 minutes (Mastodon's authenticated
/// API throttle).
pub const DEFAULT_LIMIT: u32 = 1_500;
/// Default window in seconds.
pub const DEFAULT_PERIOD_SECS: u64 = 300;
/// Maximum number of tracked identities before new ones share an overflow
/// bucket.
pub const DEFAULT_CAPACITY: usize = 100_000;
/// Largest request body the per-address policies will buffer.
const ADDRESS_BODY_LIMIT: usize = 16 * 1024;

/// The rate-limit state surfaced to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    /// Unix seconds at which the current window resets.
    pub reset_at_s: i64,
    /// True when this request went over the limit and must be refused. The
    /// request that uses the last unit of budget (`remaining == 0`) is
    /// still served.
    pub exceeded: bool,
}

/// One fixed window.
#[derive(Debug, Clone, Copy)]
struct Window {
    start_s: i64,
    period_secs: u64,
    count: u32,
}

impl Window {
    fn expired_at(&self, now_s: i64) -> bool {
        now_s >= self.start_s.saturating_add(self.period_secs as i64)
    }

    /// Count one request, restarting the window when it has elapsed.
    fn hit(&mut self, limit: u32, period_secs: u64, now_s: i64) -> RateLimit {
        if self.expired_at(now_s) || self.period_secs != period_secs {
            *self = Window {
                start_s: now_s,
                period_secs,
                count: 0,
            };
        }
        self.count = self.count.saturating_add(1);
        RateLimit {
            limit,
            remaining: limit.saturating_sub(self.count),
            reset_at_s: self.start_s.saturating_add(period_secs as i64),
            exceeded: self.count > limit,
        }
    }
}

#[derive(Debug, Default)]
struct LimiterState {
    windows: HashMap<String, Window>,
    /// Shared bucket for identities that arrive while the table is full.
    overflow: Option<Window>,
    last_sweep_s: i64,
}

/// Counters for the bounded metrics endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LimiterStats {
    pub tracked_keys: usize,
    pub denied_total: u64,
    pub overflow_total: u64,
}

/// A bounded fixed-window limiter.
///
/// The table holds at most `capacity` identities. When it is full, expired
/// windows are swept (at most once per second); if it is still full, further
/// new identities share one overflow window. Existing identities keep their
/// own counters, so a flood of fabricated keys can neither evict nor reset
/// other users' windows; it can only throttle other brand-new identities.
#[derive(Debug)]
pub struct RateLimiter {
    capacity: usize,
    state: Mutex<LimiterState>,
    denied: AtomicU64,
    overflowed: AtomicU64,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// A limiter tracking at most `capacity` identities (minimum 1).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(LimiterState::default()),
            denied: AtomicU64::new(0),
            overflowed: AtomicU64::new(0),
        }
    }

    /// Count a request for `key` and return the remaining budget. The window
    /// resets when its start time plus the period has elapsed.
    pub fn check(&self, key: &str, limit: u32, period_secs: u64, now_s: i64) -> RateLimit {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.windows.contains_key(key) && state.windows.len() >= self.capacity {
            if now_s > state.last_sweep_s {
                state.last_sweep_s = now_s;
                state.windows.retain(|_, window| !window.expired_at(now_s));
            }
            if state.windows.len() >= self.capacity {
                self.overflowed.fetch_add(1, Ordering::Relaxed);
                let overflow = state.overflow.get_or_insert(Window {
                    start_s: now_s,
                    period_secs,
                    count: 0,
                });
                let result = overflow.hit(limit, period_secs, now_s);
                if result.exceeded {
                    self.denied.fetch_add(1, Ordering::Relaxed);
                }
                return result;
            }
        }
        let window = state.windows.entry(key.to_string()).or_insert(Window {
            start_s: now_s,
            period_secs,
            count: 0,
        });
        let result = window.hit(limit, period_secs, now_s);
        if result.exceeded {
            self.denied.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// Bounded counters (no keys) for the metrics endpoint.
    pub fn stats(&self) -> LimiterStats {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        LimiterStats {
            tracked_keys: state.windows.len(),
            denied_total: self.denied.load(Ordering::Relaxed),
            overflow_total: self.overflowed.load(Ordering::Relaxed),
        }
    }
}

/// How a route-specific budget is keyed in addition to the source address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressKey {
    None,
    /// Also count per normalized `email` form/JSON field.
    Email,
}

/// A stricter budget for one sensitive route.
#[derive(Debug, Clone, Copy)]
pub struct RoutePolicy {
    scope: &'static str,
    limit: u32,
    period_secs: u64,
    address: AddressKey,
    /// Budget for the address key when `address` is not `None`.
    address_limit: u32,
}

const THIRTY_MINUTES_SECS: u64 = 1_800;

const LOGIN_POLICY: RoutePolicy = RoutePolicy {
    scope: "login",
    limit: 10,
    period_secs: DEFAULT_PERIOD_SECS,
    address: AddressKey::None,
    address_limit: 0,
};
const TOKEN_POLICY: RoutePolicy = RoutePolicy {
    scope: "token",
    limit: 30,
    period_secs: DEFAULT_PERIOD_SECS,
    address: AddressKey::None,
    address_limit: 0,
};
const REGISTRATION_POLICY: RoutePolicy = RoutePolicy {
    scope: "registration",
    limit: 10,
    period_secs: THIRTY_MINUTES_SECS,
    address: AddressKey::None,
    address_limit: 0,
};
const PASSWORD_RESET_POLICY: RoutePolicy = RoutePolicy {
    scope: "password_reset",
    limit: 5,
    period_secs: THIRTY_MINUTES_SECS,
    address: AddressKey::Email,
    address_limit: 3,
};
const CONFIRMATION_RESEND_POLICY: RoutePolicy = RoutePolicy {
    scope: "confirmation_resend",
    limit: 5,
    period_secs: THIRTY_MINUTES_SECS,
    address: AddressKey::Email,
    address_limit: 3,
};
const PASSWORD_REDEEM_POLICY: RoutePolicy = RoutePolicy {
    scope: "password_redeem",
    limit: 10,
    period_secs: THIRTY_MINUTES_SECS,
    address: AddressKey::None,
    address_limit: 0,
};
const APP_CREATE_POLICY: RoutePolicy = RoutePolicy {
    scope: "app_create",
    limit: 30,
    period_secs: THIRTY_MINUTES_SECS,
    address: AddressKey::None,
    address_limit: 0,
};

/// The route-specific policy for `method` + `path`, if any.
pub fn route_policy(method: &Method, path: &str) -> Option<RoutePolicy> {
    if method != Method::POST {
        return None;
    }
    match path {
        "/oauth/authorize" => Some(LOGIN_POLICY),
        "/oauth/token" => Some(TOKEN_POLICY),
        "/api/v1/accounts" => Some(REGISTRATION_POLICY),
        "/api/v1/emails/password_reset" => Some(PASSWORD_RESET_POLICY),
        "/api/v1/emails/confirmations" => Some(CONFIRMATION_RESEND_POLICY),
        "/auth/password" => Some(PASSWORD_REDEEM_POLICY),
        "/api/v1/apps" => Some(APP_CREATE_POLICY),
        _ => None,
    }
}

/// Resolve the client address from the TCP peer and forwarding headers.
///
/// Rule: the chain is the `X-Forwarded-For` entries (all header lines, in
/// order) followed by the peer. If the peer is not a trusted proxy, headers
/// are ignored and the peer is the client. Otherwise walk the chain from the
/// right, skipping every hop that is a trusted proxy; the first hop that is
/// not trusted is the client. An unparsable entry ends the walk at the last
/// verified hop, so garbage can never select an arbitrary identity. `None`
/// when the peer is unknown.
pub fn resolve_client_ip(
    peer: Option<IpAddr>,
    forwarded_for: &[&str],
    trusted_proxies: &[IpAddr],
) -> Option<IpAddr> {
    let peer = peer?;
    let mut current: IpAddr = peer;
    if !trusted_proxies.contains(&current) {
        return Some(current);
    }
    let mut entries: Vec<&str> = Vec::new();
    for header in forwarded_for {
        entries.extend(header.split(','));
    }
    for entry in entries.iter().rev() {
        if !trusted_proxies.contains(&current) {
            break;
        }
        match parse_forwarded_hop(entry) {
            Some(hop) => current = hop,
            None => break,
        }
    }
    Some(current)
}

/// Parse one `X-Forwarded-For` hop: a bare address, `addr:port` or
/// `[v6]:port`.
fn parse_forwarded_hop(raw: &str) -> Option<IpAddr> {
    let trimmed = raw.trim();
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Some(ip);
    }
    trimmed.parse::<SocketAddr>().ok().map(|addr| addr.ip())
}

/// Rate-limit identity for an address: IPv6 clients share a `/64`.
fn identity_for(ip: Option<IpAddr>) -> String {
    match ip {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => {
            let segments = v6.segments();
            format!(
                "{:x}:{:x}:{:x}:{:x}::/64",
                segments[0], segments[1], segments[2], segments[3]
            )
        }
        None => "unknown".to_string(),
    }
}

/// Shared middleware state: the limiter, the trusted proxies and the
/// lab-only multiplier.
#[derive(Clone)]
pub struct RateLimitState {
    limiter: Arc<RateLimiter>,
    trusted_proxies: Arc<Vec<IpAddr>>,
    multiplier: u32,
}

impl RateLimitState {
    pub fn new(trusted_proxies: Arc<Vec<IpAddr>>, multiplier: u32) -> Self {
        Self::with_limiter(Arc::new(RateLimiter::new()), trusted_proxies, multiplier)
    }

    pub fn with_limiter(
        limiter: Arc<RateLimiter>,
        trusted_proxies: Arc<Vec<IpAddr>>,
        multiplier: u32,
    ) -> Self {
        Self {
            limiter,
            trusted_proxies,
            multiplier: multiplier.max(1),
        }
    }

    /// The limiter, for metrics.
    pub fn limiter(&self) -> &Arc<RateLimiter> {
        &self.limiter
    }

    fn client_identity(&self, req: &Request) -> String {
        let peer: Option<IpAddr> = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip());
        let mut forwarded: Vec<&str> = Vec::new();
        for value in req.headers().get_all("x-forwarded-for") {
            if let Ok(text) = value.to_str() {
                forwarded.push(text);
            }
        }
        identity_for(resolve_client_ip(peer, &forwarded, &self.trusted_proxies))
    }

    fn scaled(&self, limit: u32) -> u32 {
        limit.saturating_mul(self.multiplier)
    }
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn too_many(rl: RateLimit, now: i64) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(json!({ "error": "Too many requests" })),
    )
        .into_response();
    apply_headers(response.headers_mut(), rl);
    let retry_after: i64 = (rl.reset_at_s - now).max(1);
    if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("retry-after"), value);
    }
    response
}

/// Middleware: count the request, attach `X-RateLimit-*`, and return 429 when
/// the general or a route-specific budget is exhausted.
pub async fn middleware(State(state): State<RateLimitState>, req: Request, next: Next) -> Response {
    let identity = state.client_identity(&req);
    let now = now_s();
    let general = state.limiter.check(
        &format!("general:{identity}"),
        state.scaled(DEFAULT_LIMIT),
        DEFAULT_PERIOD_SECS,
        now,
    );
    if general.exceeded {
        return too_many(general, now);
    }

    let mut req = req;
    if let Some(policy) = route_policy(req.method(), req.uri().path()) {
        let route = state.limiter.check(
            &format!("{}:{identity}", policy.scope),
            state.scaled(policy.limit),
            policy.period_secs,
            now,
        );
        if route.exceeded {
            return too_many(route, now);
        }
        if policy.address == AddressKey::Email {
            match buffer_address_key(req).await {
                Ok((rebuilt, address)) => {
                    req = rebuilt;
                    if let Some(address) = address {
                        let per_address = state.limiter.check(
                            &format!("{}:addr:{address}", policy.scope),
                            state.scaled(policy.address_limit),
                            policy.period_secs,
                            now,
                        );
                        if per_address.exceeded {
                            return too_many(per_address, now);
                        }
                    }
                }
                Err(response) => return response,
            }
        }
    }

    let mut response = next.run(req).await;
    apply_headers(response.headers_mut(), general);
    response
}

/// Buffer the (small) body, extract a hashed normalized `email` field, and
/// hand back an equivalent request. Oversized bodies are refused with 413.
async fn buffer_address_key(req: Request) -> Result<(Request, Option<String>), Response> {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, ADDRESS_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                axum::Json(json!({ "error": "Request body too large" })),
            )
                .into_response());
        }
    };
    let content_type: String = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let email: Option<String> = if content_type.contains("json") {
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v.get("email").and_then(|e| e.as_str()).map(str::to_string))
    } else {
        url::form_urlencoded::parse(&bytes)
            .find(|(name, _)| name == "email")
            .map(|(_, value)| value.into_owned())
    };
    let address: Option<String> = email
        .map(|raw| raw.trim().to_lowercase())
        .filter(|normalized| !normalized.is_empty())
        .map(|normalized| hash_address(&normalized));
    Ok((Request::from_parts(parts, Body::from(bytes)), address))
}

/// Hash of a normalized address: bounded key size, no address in memory dumps
/// of the limiter table.
fn hash_address(normalized: &str) -> String {
    let digest = Sha256::digest(normalized.as_bytes());
    hex::encode(&digest[..16])
}

fn apply_headers(headers: &mut axum::http::HeaderMap, rl: RateLimit) {
    headers.insert(
        HeaderName::from_static("x-ratelimit-limit"),
        HeaderValue::from(rl.limit),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderValue::from(rl.remaining),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-reset"),
        HeaderValue::from(rl.reset_at_s),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("test address")
    }

    #[test]
    fn window_counts_and_resets() {
        let limiter = RateLimiter::new();
        let a = limiter.check("ip", 2, 60, 1000);
        assert_eq!(a.remaining, 1);
        let b = limiter.check("ip", 2, 60, 1001);
        assert_eq!(b.remaining, 0);
        assert!(!b.exceeded, "the last unit of budget is still served");
        // Same window: now over the limit.
        let c = limiter.check("ip", 2, 60, 1002);
        assert_eq!(c.remaining, 0);
        assert!(c.exceeded);
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

    #[test]
    fn untrusted_peer_cannot_choose_identity() {
        let trusted = [ip("10.0.0.1")];
        let resolved = resolve_client_ip(Some(ip("203.0.113.9")), &["1.2.3.4"], &trusted);
        assert_eq!(resolved, Some(ip("203.0.113.9")));
    }

    #[test]
    fn trusted_chain_resolves_rightmost_untrusted_hop() {
        let trusted = [ip("10.0.0.1"), ip("10.0.0.2")];
        // client, then our two proxies; the peer is the nearest proxy.
        let resolved = resolve_client_ip(
            Some(ip("10.0.0.2")),
            &["198.51.100.7, 10.0.0.1"],
            &trusted,
        );
        assert_eq!(resolved, Some(ip("198.51.100.7")));
        // A client-supplied leading entry cannot override the appended hop.
        let spoofed = resolve_client_ip(
            Some(ip("10.0.0.1")),
            &["9.9.9.9", "198.51.100.7"],
            &trusted,
        );
        assert_eq!(spoofed, Some(ip("198.51.100.7")));
        // Garbage stops the walk at the last verified hop.
        let garbage = resolve_client_ip(Some(ip("10.0.0.1")), &["not-an-ip"], &trusted);
        assert_eq!(garbage, Some(ip("10.0.0.1")));
        // No header: the proxy itself.
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.1")), &[], &trusted),
            Some(ip("10.0.0.1"))
        );
        assert_eq!(resolve_client_ip(None, &["1.2.3.4"], &trusted), None);
    }

    #[test]
    fn ipv6_clients_share_a_slash_64() {
        let a = identity_for(Some(ip("2001:db8:1:2:aaaa::1")));
        let b = identity_for(Some(ip("2001:db8:1:2:bbbb::9")));
        assert_eq!(a, b);
        assert_ne!(a, identity_for(Some(ip("2001:db8:1:3::1"))));
    }

    #[test]
    fn flooding_new_keys_never_clears_existing_windows() {
        let limiter = RateLimiter::with_capacity(4);
        limiter.check("victim", 2, 300, 100);
        let victim = limiter.check("victim", 2, 300, 101);
        assert_eq!(victim.remaining, 0);
        assert!(limiter.check("victim", 2, 300, 101).exceeded);
        for n in 0..1_000 {
            limiter.check(&format!("attacker-{n}"), 2, 300, 102);
        }
        let stats = limiter.stats();
        assert!(stats.tracked_keys <= 4, "{stats:?}");
        assert!(stats.overflow_total > 0);
        // The victim's exhausted window survived the flood.
        assert_eq!(limiter.check("victim", 2, 300, 103).remaining, 0);
    }

    #[test]
    fn full_table_recovers_after_windows_expire() {
        let limiter = RateLimiter::with_capacity(2);
        limiter.check("a", 5, 10, 0);
        limiter.check("b", 5, 10, 0);
        // Full and unexpired: a third identity overflows.
        limiter.check("c", 5, 10, 1);
        assert_eq!(limiter.stats().overflow_total, 1);
        // After expiry the sweep frees room and "c" gets its own window.
        let fresh = limiter.check("c", 5, 10, 11);
        assert_eq!(fresh.remaining, 4);
        assert!(limiter.stats().tracked_keys <= 2);
    }

    #[test]
    fn route_policies_cover_sensitive_posts_only() {
        assert!(route_policy(&Method::POST, "/oauth/token").is_some());
        assert!(route_policy(&Method::POST, "/oauth/authorize").is_some());
        assert!(route_policy(&Method::POST, "/api/v1/accounts").is_some());
        assert!(route_policy(&Method::POST, "/api/v1/emails/password_reset").is_some());
        assert!(route_policy(&Method::POST, "/api/v1/emails/confirmations").is_some());
        assert!(route_policy(&Method::GET, "/oauth/token").is_none());
        assert!(route_policy(&Method::POST, "/api/v1/statuses").is_none());
    }

    #[test]
    fn address_hash_is_case_and_space_insensitive_via_normalization() {
        let a = hash_address(&"  Alice@Example.COM ".trim().to_lowercase());
        let b = hash_address("alice@example.com");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }
}

//! Safe outbound client for Web Push delivery (SSRF defence).
//!
//! Upstream reference: Mastodon v4.7.3 (`8ff11c2b`) performs no SSRF
//! filtering on push endpoints at all; this module closes that gap for
//! birchtree. An authenticated user can register any endpoint URL, so every
//! URL the server dials on notification delivery is untrusted input.
//!
//! The policy reuses [`crate::federation::policy`] (the single catalogue of
//! prohibited ranges: loopback, RFC 1918, link-local `169.254/16` and
//! `fe80::/10`, CGNAT `100.64/10`, unique-local `fc00::/7`, multicast,
//! unspecified, documentation, benchmarking and protocol-assignment ranges,
//! plus IPv4-embedded IPv6 forms judged by the address they carry). There is
//! deliberately one range catalogue for all outbound traffic; this module
//! only adds the push-specific wiring (HTTPS default-deny, manual redirects,
//! VAPID header handling).
//!
//! Enforcement points, in order:
//!
//! 1. Write time ([`validate_endpoint_for_subscription`], called by the push
//!    subscription endpoints): shape check plus DNS resolution — every
//!    address the name resolves to must be allowed. A hostname that does not
//!    resolve at all is accepted: with no addresses nothing can be dialled,
//!    and delivery revalidates with fresh DNS before every send, so a
//!    later-resolving name is checked at the moment it matters.
//! 2. Delivery time ([`validate_delivery_target`], called by
//!    [`crate::application::push::deliver_push`]): shape check plus DNS
//!    resolution, fail-closed — any refusal or resolution failure settles
//!    the attempt without a connection.
//! 3. Connect time: [`build_push_client`] returns a client whose DNS resolver
//!    refuses any answer containing a blocked address, so a rebinding answer
//!    that changed between validation and connection still cannot be dialled.
//!    Proxies are disabled and reqwest-level redirects are off; redirects are
//!    followed manually by the delivery loop, which revalidates every hop.

use std::net::IpAddr;
use std::time::Duration;

use url::Url;

use crate::federation::net;
use crate::federation::policy;

/// Re-exported so push callers name one module for the whole surface.
pub use crate::federation::policy::Cidr;

/// Limits and policy for the outbound push client. The defaults are the
/// production values: HTTPS only, no allowlisted ranges.
#[derive(Debug, Clone)]
pub struct PushClientConfig {
    /// Allow `http://` targets. Off in production. The lab constructor
    /// enables it together with a loopback allowlist for local tests.
    pub allow_http: bool,
    /// Operator-allowed private ranges (lab networks only; empty by default).
    pub allowlist: Vec<Cidr>,
    /// TCP connect timeout for one delivery attempt.
    pub connect_timeout: Duration,
    /// Total time for one delivery attempt, including the body.
    pub request_timeout: Duration,
    /// `User-Agent` sent on delivery requests.
    pub user_agent: String,
}

impl Default for PushClientConfig {
    fn default() -> Self {
        Self {
            allow_http: false,
            allowlist: Vec::new(),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            user_agent: format!("birchtree/{} (push)", env!("CARGO_PKG_VERSION")),
        }
    }
}

impl PushClientConfig {
    /// Lab/test policy: plain HTTP plus the loopback ranges, so a test push
    /// service on `127.0.0.1` or `[::1]` is reachable. Never use in
    /// production: it re-opens exactly the SSRF channel this module closes.
    pub fn lab_loopback() -> Self {
        Self {
            allow_http: true,
            allowlist: vec![
                Cidr::parse("127.0.0.0/8").expect("loopback v4 allowlist"),
                Cidr::parse("::1/128").expect("loopback v6 allowlist"),
            ],
            ..Self::default()
        }
    }
}

/// Build the `reqwest` client deliveries must use: no proxy, no automatic
/// redirects (the delivery loop follows them manually under policy),
/// bounded timeouts, and a DNS resolver that refuses any answer containing
/// a blocked address.
pub fn build_push_client(config: &PushClientConfig) -> Result<reqwest::Client, String> {
    net::client(&net::SafeHttpConfig {
        allow_http: config.allow_http,
        allowlist: config.allowlist.clone(),
        connect_timeout: config.connect_timeout,
        request_timeout: config.request_timeout,
        user_agent: config.user_agent.clone(),
        extra_roots: Vec::new(),
    })
}

/// Check an endpoint URL's shape without touching the network: it must parse,
/// use HTTPS (or HTTP when the config allows it, lab only), carry no
/// credentials, have a host, and — for IP literals — name an allowed
/// address. Hostnames pass here; their addresses are checked by
/// [`validate_endpoint_for_subscription`] (write time) and
/// [`validate_delivery_target`] (delivery time).
pub fn validate_push_url(raw: &str, config: &PushClientConfig) -> Result<Url, String> {
    if raw.trim().is_empty() {
        return Err("Validation failed: Endpoint can't be blank".to_string());
    }
    let url =
        Url::parse(raw.trim()).map_err(|_| "Validation failed: Endpoint is invalid".to_string())?;
    policy::check_url(&url, config.allow_http, &config.allowlist).map_err(|err| match err {
        policy::PolicyError::Scheme | policy::PolicyError::Credentials => {
            "Validation failed: Endpoint is invalid".to_string()
        }
        policy::PolicyError::NoHost | policy::PolicyError::Address => {
            "Validation failed: Endpoint is not allowed".to_string()
        }
        policy::PolicyError::Allowlist(_) => {
            "Validation failed: Endpoint is not allowed".to_string()
        }
    })?;
    Ok(url)
}

/// Whether `origin` (a `scheme://host[:port]` audience string) equals the
/// origin of `url`, comparing scheme, host and effective port (an explicit
/// default port matches the implicit one). An unparsable origin never matches
/// (fail closed: the credential is stripped).
pub fn same_origin(origin: &str, url: &Url) -> bool {
    let Ok(base) = Url::parse(origin) else {
        return false;
    };
    base.scheme() == url.scheme()
        && base.host() == url.host()
        && base.port_or_known_default() == url.port_or_known_default()
}

/// Check a resolved address list (every address a hostname resolved to, or
/// the single literal): all of them must be diallable. A mixed answer — one
/// public address next to a prohibited one — is refused as a whole, so DNS
/// rebinding cannot smuggle a private target past a public-looking name.
pub fn check_resolved_addrs(addrs: &[IpAddr], allowlist: &[Cidr]) -> Result<(), String> {
    if addrs.is_empty() {
        return Err("push endpoint did not resolve to any address".to_string());
    }
    if addrs
        .iter()
        .any(|addr| !policy::address_allowed(*addr, allowlist))
    {
        return Err("push endpoint address is not allowed".to_string());
    }
    Ok(())
}

/// Resolve `url`'s host and check every returned address. Used before each
/// delivery attempt; the client's resolver enforces the same check again at
/// connect time.
async fn resolve_and_check(url: &Url, config: &PushClientConfig) -> Result<(), String> {
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|err| format!("push endpoint did not resolve: {err}"))?
        .map(|addr| addr.ip())
        .collect();
    check_resolved_addrs(&addrs, &config.allowlist)
}

/// How a refused delivery target settles the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The subscription is invalid or weaponised: delete it, like upstream's
    /// gone-endpoint cleanup.
    Gone,
    /// Possibly transient (DNS or network wobble): retry with backoff.
    Retry,
}

/// A refused delivery target and how it settles.
#[derive(Debug, Clone)]
pub struct TargetRefusal {
    pub settlement: Refusal,
    pub message: String,
}

/// Write-time validation for a subscription endpoint: shape check plus DNS.
/// Every address the host resolves to must be allowed. A hostname that does
/// not resolve is accepted — with no addresses nothing can be dialled — and
/// delivery revalidates with fresh DNS before every send.
pub async fn validate_endpoint_for_subscription(
    raw: &str,
    config: &PushClientConfig,
) -> Result<Url, String> {
    let url = validate_push_url(raw, config)?;
    match resolve_and_check(&url, config).await {
        Ok(()) => Ok(url),
        Err(message) if message.starts_with("push endpoint did not resolve") => Ok(url),
        Err(_) => Err("Validation failed: Endpoint is not allowed".to_string()),
    }
}

/// Delivery-time validation for one request URL: shape check plus DNS,
/// fail-closed. Any refusal settles without a connection being made.
pub async fn validate_delivery_target(
    url: &Url,
    config: &PushClientConfig,
) -> Result<(), TargetRefusal> {
    let refused = |settlement: Refusal, message: String| TargetRefusal {
        settlement,
        message,
    };
    if policy::check_url(url, config.allow_http, &config.allowlist).is_err() {
        return Err(refused(
            Refusal::Gone,
            format!("push endpoint is not allowed: {url}"),
        ));
    }
    match resolve_and_check(url, config).await {
        Ok(()) => Ok(()),
        Err(message) if message.starts_with("push endpoint did not resolve") => {
            Err(refused(Refusal::Retry, message))
        }
        Err(message) => Err(refused(Refusal::Gone, message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PushClientConfig {
        PushClientConfig::default()
    }

    #[test]
    fn shape_requires_https_host_and_no_credentials() {
        assert!(validate_push_url("https://push.example/sub", &config()).is_ok());
        assert!(validate_push_url("", &config()).is_err());
        assert!(validate_push_url("gopher://x/y", &config()).is_err());
        assert!(validate_push_url("http://push.example/sub", &config()).is_err());
        assert!(validate_push_url("https://user:pw@push.example/", &config()).is_err());
        assert!(validate_push_url("https://", &config()).is_err());
    }

    #[test]
    fn lab_config_allows_plain_http_shape() {
        let lab = PushClientConfig::lab_loopback();
        assert!(validate_push_url("http://127.0.0.1:1/sub", &lab).is_ok());
        // HTTP is a shape question only here; the loopback address still
        // needs the allowlist, so a non-loopback literal stays refused.
        assert!(validate_push_url("http://10.0.0.5/sub", &lab).is_err());
    }

    #[test]
    fn mixed_answers_are_refused() {
        let public: IpAddr = "8.8.8.8".parse().expect("public");
        let private: IpAddr = "169.254.169.254".parse().expect("private");
        assert!(check_resolved_addrs(&[public], &[]).is_ok());
        assert!(check_resolved_addrs(&[public, private], &[]).is_err());
        assert!(check_resolved_addrs(&[], &[]).is_err());
    }

    #[test]
    fn origin_comparison_ignores_default_ports() {
        let url = Url::parse("https://push.example:443/sub").expect("url");
        assert!(same_origin("https://push.example", &url));
        assert!(!same_origin("https://other.example", &url));
        assert!(!same_origin("http://push.example", &url));
        let other_port = Url::parse("https://push.example:8443/sub").expect("url");
        assert!(!same_origin("https://push.example", &other_port));
    }

    #[test]
    fn client_builds_with_the_default_policy() {
        assert!(build_push_client(&config()).is_ok());
    }
}

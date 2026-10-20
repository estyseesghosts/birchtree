//! The outbound HTTP client and URL verifier handed to `activitypub_federation`.
//!
//! The crate performs every fetch and send, and calls the injected client for
//! them. This module decides what that client may reach:
//! 1. Proxies are disabled. `HTTP_PROXY`-style variables cannot route a request.
//! 2. Redirects are not followed by reqwest. The crate follows one redirect
//!    itself and validates it with the URL verifier below.
//! 3. The resolver refuses a name when any address it returns is blocked, and
//!    reqwest connects only to the addresses the resolver returned. A DNS
//!    rebinding answer therefore cannot reach a private address, because the
//!    connection is made to an address validated at resolution time.
//! 4. Connect and total timeouts bound the time spent on any request.
//! 5. [`PolicyVerifier`] checks each URL's shape (scheme, credentials,
//!    IP-literal host) before the crate fetches or sends to it.

use std::error::Error as StdError;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use url::Url;

use crate::federation::policy::{self, Cidr, PolicyError};

/// Limits and policy for the outbound client. The defaults are the production
/// values; the operator changes them only through explicit configuration.
#[derive(Debug, Clone)]
pub struct SafeHttpConfig {
    /// Allow `http://` targets. Off in production (lab networks only).
    pub allow_http: bool,
    /// Operator-allowed private ranges (lab networks only; empty by default).
    pub allowlist: Vec<Cidr>,
    pub connect_timeout: Duration,
    /// Total time for one request, including the body.
    pub request_timeout: Duration,
    pub user_agent: String,
    /// Extra trust roots, added to the system roots. Empty in production; the lab
    /// sets them with `--federation-extra-ca` for its private certificate authority.
    pub extra_roots: Vec<reqwest::Certificate>,
}

impl Default for SafeHttpConfig {
    fn default() -> Self {
        Self {
            allow_http: false,
            allowlist: Vec::new(),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            user_agent: format!("birchtree/{} (federation)", env!("CARGO_PKG_VERSION")),
            extra_roots: Vec::new(),
        }
    }
}

/// A DNS resolver that refuses any name whose answer contains a blocked address.
struct PolicyResolver {
    allowlist: Arc<Vec<Cidr>>,
}

impl Resolve for PolicyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allowlist = self.allowlist.clone();
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::other("no addresses")) as Box<dyn StdError + Send + Sync>);
            }
            if addrs.iter().any(|a| !policy::address_allowed(a.ip(), &allowlist)) {
                return Err(Box::new(PolicyError::Address) as Box<dyn StdError + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Build the reqwest client the crate uses for every outbound request.
pub fn client(cfg: &SafeHttpConfig) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(cfg.connect_timeout)
        .timeout(cfg.request_timeout)
        .user_agent(cfg.user_agent.clone())
        .dns_resolver(Arc::new(PolicyResolver {
            allowlist: Arc::new(cfg.allowlist.clone()),
        }));
    for root in &cfg.extra_roots {
        builder = builder.add_root_certificate(root.clone());
    }
    builder.build().map_err(|e| e.to_string())
}

/// Turns every 4xx answer into an error. The crate treats a 4xx as delivered and
/// does not retry it; birchtree wants 4xx retried (owner decision, 2026-10-18, D28).
/// An error here reaches the crate as `ReqwestMiddleware`, and the delivery job
/// settles it as a retry. The job's retry budget bounds the attempts.
///
/// Only deliveries (activity POSTs to an inbox) are converted. Document
/// fetches (GET/HEAD) pass their status through, so the crate maps a missing
/// document to `NotFound`, a deleted one to `ObjectDeleted` (410) and a bad
/// body to a parse error — each terminal for that URI — instead of every 4xx
/// becoming a retried middleware error.
pub struct FailClientErrors;

#[async_trait]
impl reqwest_middleware::Middleware for FailClientErrors {
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        let is_fetch = matches!(*req.method(), reqwest::Method::GET | reqwest::Method::HEAD);
        let response = next.run(req, extensions).await?;
        if is_fetch {
            return Ok(response);
        }
        let status = response.status();
        if status.is_client_error() {
            return Err(reqwest_middleware::Error::Middleware(anyhow::anyhow!(
                "remote answered {status}"
            )));
        }
        Ok(response)
    }
}

/// The client the crate uses: the safe client with the 4xx rule.
pub fn middleware_client(cfg: &SafeHttpConfig) -> Result<reqwest_middleware::ClientWithMiddleware, String> {
    Ok(reqwest_middleware::ClientBuilder::new(client(cfg)?)
        .with(FailClientErrors)
        .build())
}

/// The crate's URL hook: refuses any URL that fails the shape policy. The
/// crate calls it for inbound activity IDs, fetched URLs and inboxes it sends to.
#[derive(Debug, Clone)]
pub struct PolicyVerifier {
    pub allow_http: bool,
    pub allowlist: Arc<Vec<Cidr>>,
}

impl PolicyVerifier {
    pub fn new(cfg: &SafeHttpConfig) -> Self {
        Self {
            allow_http: cfg.allow_http,
            allowlist: Arc::new(cfg.allowlist.clone()),
        }
    }
}

#[async_trait]
impl activitypub_federation::config::UrlVerifier for PolicyVerifier {
    async fn verify(&self, url: &Url) -> Result<(), activitypub_federation::error::Error> {
        policy::check_url(url, self.allow_http, &self.allowlist)
            .map_err(|_| activitypub_federation::error::Error::UrlVerificationError("url refused by federation policy"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_refuses_private_literals_and_plain_http() {
        let cfg = SafeHttpConfig::default();
        let verifier = PolicyVerifier::new(&cfg);
        let check = |s: &str| policy::check_url(&Url::parse(s).unwrap(), verifier.allow_http, &verifier.allowlist);
        assert!(check("https://example.social/users/a").is_ok());
        assert!(check("http://example.social/users/a").is_err());
        assert!(check("https://10.0.0.5/inbox").is_err());
        assert!(check("https://127.0.0.1/inbox").is_err());
    }

    #[test]
    fn client_builds_with_the_default_policy() {
        assert!(client(&SafeHttpConfig::default()).is_ok());
    }
}

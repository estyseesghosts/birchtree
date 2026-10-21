//! ActivityPub adapter (Phase 6).
//!
//! The `activitypub_federation` crate does the protocol: HTTP signatures and
//! digests, inbox verification, WebFinger, dereferencing and delivery. This
//! module supplies birchtree's types for the crate's traits (`actor`,
//! `activities`), the HTTP routes (`http`), the error mapping (`error`), and
//! the outbound commands (`outbound`, `resolve`). Business rules stay in
//! `application::federation`; storage stays in `persistence::redb::federation`.

pub mod actor;
pub mod activities;
pub mod error;
pub mod http;
pub mod instance;
pub mod key_generation;
pub mod objects;
pub mod outbound;
pub mod outgoing;
pub mod resolve;
pub mod wire;

use std::sync::Arc;

use activitypub_federation::config::FederationConfig;

use crate::application::context::Context;
use crate::config::RemoteCacheConfig;
use crate::federation::net::{self, SafeHttpConfig};

/// The app data the crate hands to every federation callback (`Data<FedContext>`).
#[derive(Clone)]
pub struct FedContext {
    /// Store, clock and id generator.
    pub app: Arc<Context>,
    /// The public base URL (no trailing slash), for local actor URLs.
    pub public_url: Arc<str>,
    /// Whether public registration is open (advertised in NodeInfo).
    pub registrations_open: bool,
    /// Instance title/description (advertised in NodeInfo `metadata`;
    /// upstream `NodeInfo::Serializer#metadata`).
    pub site_title: Arc<str>,
    pub site_description: Arc<str>,
    /// The outbound transport policy (remote media fetches apply it too).
    pub http: std::sync::Arc<SafeHttpConfig>,
    /// A plain client for remote media: the crate's client is not reachable from here.
    pub media_client: reqwest::Client,
    /// Authorized fetch: reads of our documents need a valid HTTP signature.
    pub authorized_fetch: bool,
    /// Relay peers public activities forward to (federation tail, G-D).
    /// Empty = no relay traffic.
    pub relays: Arc<Vec<String>>,
    /// Remote-media fetch policy (on demand by default): ingest enqueues
    /// `FetchRemoteMedia` only when
    /// [`crate::media::cache_maintenance::fetch_at_ingest`] says so.
    pub remote_cache: RemoteCacheConfig,
}

/// The crate's configuration for this server.
///
/// `debug` is the crate's own lab switch: its production checks refuse any
/// host that resolves to a private address, and it refuses `http` URLs. The
/// lab sets it only together with the operator allowlist, and nothing else
/// uses it. Production runs with `debug == false`.
pub async fn build_config(
    context: Arc<Context>,
    public_url: &str,
    registrations_open: bool,
    site_title: &str,
    site_description: &str,
    http: &SafeHttpConfig,
    debug: bool,
    instance: Option<&instance::InstanceActor>,
    authorized_fetch: bool,
    relays: &[String],
) -> Result<FederationConfig<FedContext>, String> {
    build_config_with_remote_cache(
        context,
        public_url,
        registrations_open,
        site_title,
        site_description,
        http,
        debug,
        instance,
        authorized_fetch,
        relays,
        &RemoteCacheConfig::default(),
    )
    .await
}

/// As [`build_config`], with the operator's remote-media cache policy: the
/// ingest path enqueues `FetchRemoteMedia` eagerly only when the policy
/// allows it (otherwise fetches happen on demand at the first cache miss).
#[allow(clippy::too_many_arguments)]
pub async fn build_config_with_remote_cache(
    context: Arc<Context>,
    public_url: &str,
    registrations_open: bool,
    site_title: &str,
    site_description: &str,
    http: &SafeHttpConfig,
    debug: bool,
    instance: Option<&instance::InstanceActor>,
    authorized_fetch: bool,
    relays: &[String],
    remote_cache: &RemoteCacheConfig,
) -> Result<FederationConfig<FedContext>, String> {
    let domain = host_of(public_url).ok_or_else(|| format!("public url has no host: {public_url}"))?;
    let client = net::middleware_client(http)?;
    let media_client = net::client(http)?;
    let mut builder = FederationConfig::builder();
    if let Some(actor) = instance {
        // Every outbound fetch is signed with the instance actor's key (contract: signed fetch).
        builder.signed_fetch_actor(actor);
    }
    builder
        .domain(domain)
        .app_data(FedContext {
            app: context,
            public_url: Arc::from(public_url.trim_end_matches('/')),
            registrations_open,
            site_title: Arc::from(site_title),
            site_description: Arc::from(site_description),
            http: Arc::new(http.clone()),
            media_client,
            authorized_fetch,
            relays: Arc::new(relays.to_vec()),
            remote_cache: remote_cache.clone(),
        })
        .client(client)
        .url_verifier(Box::new(net::PolicyVerifier::new(http)))
        .request_timeout(http.request_timeout)
        .debug(debug)
        .allow_http_urls(http.allow_http)
        .build()
        .await
        .map_err(|e| e.to_string())
}

/// The `host[:port]` of a URL, the form the crate and birchtree's accounts use.
pub fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

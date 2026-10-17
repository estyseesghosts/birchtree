pub mod router;
pub mod auth;
pub mod errors;
pub mod request;
pub mod pagination;
pub mod rate_limits;
pub mod dto;
pub mod endpoints;

use std::net::IpAddr;
use std::sync::Arc;

use crate::application::context::Context;

pub use router::build_router;

/// Static, config-derived information the read endpoints expose.
///
/// Assembled once at startup; never mutated at runtime.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// Normalized public base URL (no trailing slash).
    pub public_url: String,
    /// Bare host of the public URL (the local domain).
    pub domain: String,
    pub site_title: String,
    pub site_description: String,
    pub site_contact_email: String,
    pub site_contact_username: Option<String>,
    /// Whether public registration is open (mirrors `Config::registration_open`).
    pub registrations_open: bool,
    /// Streaming API base URL advertised to clients.
    pub streaming_url: String,
}

impl ServerInfo {
    /// Absolute URL for a server-relative asset path.
    pub fn asset_url(&self, path: &str) -> String {
        format!("{}/{}", self.public_url, path.trim_start_matches('/'))
    }

    /// Derive the static server facts from configuration.
    ///
    /// The streaming URL is derived from the public URL (`http` -> `ws`,
    /// `https` -> `wss`) rather than configured separately.
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        let public_url = cfg.normalized_public_url();
        let domain = crate::application::accounts::host_from_url(&public_url);
        let streaming_url = match public_url.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some(("http", rest)) => format!("ws://{rest}"),
            _ => public_url.clone(),
        };
        Self {
            public_url,
            domain,
            site_title: cfg.site_title.clone(),
            site_description: cfg.site_description.clone(),
            site_contact_email: cfg.site_contact_email.clone(),
            site_contact_username: cfg.site_contact_username.clone(),
            registrations_open: cfg.registration_open,
            streaming_url,
        }
    }
}

/// Shared HTTP state.
#[derive(Clone)]
pub struct ApiState {
    pub context: Arc<Context>,
    /// Proxies trusted to set forwarding headers.
    pub trusted_proxies: Arc<Vec<IpAddr>>,
    pub max_body_size: usize,
    pub server: Arc<ServerInfo>,
}

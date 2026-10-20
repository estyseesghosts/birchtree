pub mod router;
pub mod auth;
pub mod errors;
pub mod request;
pub mod pagination;
pub mod rate_limits;
pub mod dto;
pub mod status_view;
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
    /// Whether public registration creates pending accounts (mirrors
    /// `Config::registration_requires_approval`, upstream
    /// `Setting.registrations_mode == "approved"`).
    pub registrations_require_approval: bool,
    /// Maximum status text length in characters (mirrors
    /// `Config::max_status_chars`; advertised by the instance endpoints).
    pub max_status_chars: usize,
    /// Streaming API base URL advertised to clients.
    pub streaming_url: String,
    /// Streaming host configured separately from this server. Streaming paths
    /// on this server redirect there when it differs (upstream's
    /// `StreamingController`). `None` when streams are served here.
    pub streaming_host: Option<StreamingHost>,
    /// Whether the public trends endpoints serve approved items (mirrors
    /// `Config::trends_enabled`, upstream `Setting.trends`).
    // TEMP placement (Subagent A): sibling D declared this on `ApiState` but
    // set it in `ServerInfo::from_config`; every `ApiState` literal on disk
    // predates the field, so it lives here until D wires the routes.
    pub trends_enabled: bool,
    /// Search backend switch (Phase 7.2; mirrors `Config::search_mode`).
    pub search_mode: crate::search::SearchMode,
    /// VAPID public key for push serializers (mirrors
    /// `Config::vapid_public_key`).
    pub vapid_public_key: String,
    /// keystodon bundle directory (mirrors `Config::web_client_dir`).
    pub web_client_dir: Option<std::path::PathBuf>,
    /// Whether the ffmpeg transcode pipeline runs (Phase 7.1).
    pub transcoding_enabled: bool,
    /// ffmpeg binary path (Phase 7.1).
    pub ffmpeg_path: String,
    /// Maximum media upload size in bytes (mirrors `Config::max_file_size`;
    /// the VP9/WebM passthrough bound and the transcode input cap).
    pub max_file_size: u64,
    /// Per-job transcode limits (Phase 7.1).
    pub transcode_wall_time_secs: u64,
    pub transcode_max_output_bytes: u64,
    pub transcode_max_memory_bytes: u64,
}

/// Host and optional explicit port of a separately configured streaming base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingHost {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

impl StreamingHost {
    /// Parse a `ws`, `wss`, `http` or `https` base URL.
    pub fn parse(base: &str) -> Option<Self> {
        let (scheme, rest) = base.split_once("://")?;
        let authority = rest.split('/').next()?;
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') => (host, port.parse::<u16>().ok()),
            _ => (authority, None),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_ascii_lowercase(),
            port,
        })
    }

    /// The effective port: the explicit one, else the scheme default. Upstream
    /// compares against `80` regardless of scheme; the scheme default avoids a
    /// redirect loop for `wss` on the default TLS port.
    fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.scheme.as_str() {
            "https" | "wss" => 443,
            _ => 80,
        })
    }

    /// Whether `other` names the same host and effective port.
    pub fn same_origin(&self, other: &StreamingHost) -> bool {
        self.host == other.host && self.effective_port() == other.effective_port()
    }

    /// Whether a request for `host` (its `Host` header value) on `scheme`
    /// addresses this streaming host.
    pub fn is_same_as(&self, request_host: &str, request_scheme: &str) -> bool {
        let (host, port) = match request_host.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') => (host, port.parse::<u16>().ok()),
            _ => (request_host, None),
        };
        let request_port = port.unwrap_or(match request_scheme {
            "https" => 443,
            _ => 80,
        });
        host.eq_ignore_ascii_case(&self.host) && request_port == self.effective_port()
    }

    /// `Location` for a redirect: the request's scheme and path, this host.
    pub fn location(&self, request_scheme: &str, path_and_query: &str) -> String {
        let authority = match self.port {
            Some(port) => format!("{}:{port}", self.host),
            None => self.host.clone(),
        };
        format!("{request_scheme}://{authority}{path_and_query}")
    }
}

impl ServerInfo {
    /// Absolute URL for a server-relative asset path.
    pub fn asset_url(&self, path: &str) -> String {
        format!("{}/{}", self.public_url, path.trim_start_matches('/'))
    }

    /// Derive the static server facts from configuration.
    ///
    /// The streaming URL is derived from the public URL (`http` -> `ws`,
    /// `https` -> `wss`) unless `streaming_api_base_url` is configured.
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        let public_url = cfg.normalized_public_url();
        let domain = crate::application::accounts::host_from_url(&public_url);
        let derived_streaming_url = match public_url.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some(("http", rest)) => format!("ws://{rest}"),
            _ => public_url.clone(),
        };
        let (streaming_url, streaming_host) = match &cfg.streaming_api_base_url {
            Some(base) => (base.clone(), StreamingHost::parse(base)),
            None => (derived_streaming_url, None),
        };
        // A configured base on this server's own origin needs no redirect.
        let streaming_host = streaming_host.filter(|configured| {
            StreamingHost::parse(&public_url).is_none_or(|public| !configured.same_origin(&public))
        });
        Self {
            public_url,
            domain,
            site_title: cfg.site_title.clone(),
            site_description: cfg.site_description.clone(),
            site_contact_email: cfg.site_contact_email.clone(),
            site_contact_username: cfg.site_contact_username.clone(),
            registrations_open: cfg.registration_open,
            registrations_require_approval: cfg.registration_requires_approval,
            max_status_chars: cfg.max_status_chars,
            streaming_url,
            streaming_host,
            trends_enabled: cfg.trends_enabled,
            search_mode: cfg.search_mode,
            vapid_public_key: cfg.vapid_public_key.clone(),
            web_client_dir: cfg.web_client_dir.clone(),
            transcoding_enabled: cfg.transcoding_enabled,
            ffmpeg_path: cfg.ffmpeg_path.clone(),
            max_file_size: cfg.max_file_size,
            transcode_wall_time_secs: cfg.transcode_wall_time_secs,
            transcode_max_output_bytes: cfg.transcode_max_output_bytes,
            transcode_max_memory_bytes: cfg.transcode_max_memory_bytes,
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
    /// The streaming hub (Phase 5). The same hub is `context.streaming`.
    pub streams: crate::streaming::Hub,
    /// The ActivityPub crate's configuration (Phase 6): the safe HTTP client,
    /// the URL policy and the app data the crate hands to its callbacks.
    pub federation: activitypub_federation::config::FederationConfig<crate::activitypub::FedContext>,
    /// Multiplier on the API rate limit (1 in production). See `Config::rate_limit_multiplier`.
    pub rate_limit_multiplier: u32,
}

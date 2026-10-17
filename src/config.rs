use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

/// Server configuration. Validated before any services are built.
#[derive(Debug, Clone)]
pub struct Config {
    /// Public base URL used for actor/object URIs (must be http/https).
    pub public_url: String,
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    /// Addresses trusted to set forwarding headers (`X-Forwarded-For`).
    pub trusted_proxies: Vec<IpAddr>,
    /// Maximum media upload size in bytes.
    pub max_file_size: u64,
    /// Maximum request body size in bytes for the HTTP layer.
    pub max_body_size: usize,
    /// Worker poll interval when the queue is empty.
    pub worker_poll_interval: Duration,
    /// Whether new account registration is open. Phase 1 keeps this closed;
    /// the CLI bootstrap is always available.
    pub registration_open: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            public_url: "http://localhost:3000".to_string(),
            data_dir: PathBuf::from("./data"),
            listen: "0.0.0.0:3000".parse().unwrap(),
            trusted_proxies: Vec::new(),
            max_file_size: 16 * 1024 * 1024,
            max_body_size: 16 * 1024 * 1024,
            worker_poll_interval: Duration::from_millis(250),
            registration_open: false,
        }
    }
}

impl Config {
    /// Validate configuration. Returns a human-readable error string.
    pub fn validate(&self) -> Result<(), String> {
        if self.public_url.trim().is_empty() {
            return Err("--public-url must not be empty".to_string());
        }
        if !(self.public_url.starts_with("http://") || self.public_url.starts_with("https://")) {
            return Err("--public-url must start with http:// or https://".to_string());
        }
        if self.data_dir.as_os_str().is_empty() {
            return Err("--data-dir must not be empty".to_string());
        }
        if self.max_file_size == 0 {
            return Err("max_file_size must be > 0".to_string());
        }
        if self.max_body_size == 0 {
            return Err("max_body_size must be > 0".to_string());
        }
        if self.max_file_size > self.max_body_size as u64 {
            return Err("max_file_size must not exceed max_body_size".to_string());
        }
        if self.worker_poll_interval.is_zero() {
            return Err("worker_poll_interval must be > 0".to_string());
        }
        Ok(())
    }

    /// Normalize the public URL (strip trailing slash) for actor/object URIs.
    pub fn normalized_public_url(&self) -> String {
        self.public_url.trim_end_matches('/').to_string()
    }

    /// Path of the `redb` database file.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("birchtree.redb")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn rejects_non_http_public_url() {
        let cfg = Config {
            public_url: "ftp://example.social".into(),
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_zero_body_size() {
        let cfg = Config {
            max_body_size: 0,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn normalized_url_strips_trailing_slash() {
        let cfg = Config {
            public_url: "https://example.social/".into(),
            ..Config::default()
        };
        assert_eq!(cfg.normalized_public_url(), "https://example.social");
    }
}

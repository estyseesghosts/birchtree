use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

/// Server configuration. Validated before any services are built.
#[derive(Debug, Clone)]
pub struct Config {
    pub public_url: String,
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub trusted_proxies: Vec<IpAddr>,
    pub max_file_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            public_url: "http://localhost:3000".to_string(),
            data_dir: PathBuf::from("./data"),
            listen: "0.0.0.0:3000".parse().unwrap(),
            trusted_proxies: Vec::new(),
            max_file_size: 16 * 1024 * 1024,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.public_url.is_empty() {
            return Err("--public-url must not be empty".to_string());
        }
        if self.max_file_size == 0 {
            return Err("max_file_size must be > 0".to_string());
        }
        Ok(())
    }

    /// Normalize the public URL (strip trailing slash) for actor/object URIs.
    pub fn normalized_public_url(&self) -> String {
        self.public_url.trim_end_matches('/').to_string()
    }
}

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

/// Shared HTTP state.
#[derive(Clone)]
pub struct ApiState {
    pub context: Arc<Context>,
    /// Proxies trusted to set forwarding headers.
    pub trusted_proxies: Arc<Vec<IpAddr>>,
    pub max_body_size: usize,
}

pub mod router;
pub mod auth;
pub mod errors;
pub mod request;
pub mod pagination;
pub mod rate_limits;
pub mod dto;
pub mod endpoints;

use std::sync::Arc;
use crate::application::context::Context;

pub struct ApiState {
    pub context: Arc<Context>,
    pub trusted_proxies: Arc<Vec<std::net::IpAddr>>,
}

pub fn build_router(_state: ApiState) -> axum::Router {
    axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }))
}

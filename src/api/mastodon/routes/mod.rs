//! Route fragments (S2 decomposition): one file per resource.
//!
//! Each submodule exposes `router() -> Router<ApiState>`; the parent
//! [`super::router::build_router_with_probes`] merges them in registration
//! order (static paths before dynamic ones within each fragment).

pub mod routes_accounts;
pub mod routes_admin;
pub mod routes_collections;
pub mod routes_health;
pub mod routes_instance;
pub mod routes_lists_filters;
pub mod routes_media;
pub mod routes_misc;
pub mod routes_notifications;
pub mod routes_oauth;
pub mod routes_relationships;
pub mod routes_statuses;
pub mod routes_timelines;

//! Streaming API (Phase 5): SSE and WebSocket hosted in-process.
//!
//! Layout:
//! - `channels`: stream names, parameters and upstream channel ids.
//! - `auth`: token extraction, token resolution and scope checks.
//! - `hub`: connection and subscription registry, bounded queues, caps,
//!   heartbeat-independent stall detection and the [`StreamingSink`] adapter.
//! - `fanout`: turns a committed [`StreamEvent`] into frames, using the same
//!   policy the REST timelines use (`application::timelines`).
//! - `render`: payload rendering through the REST DTOs.
//! - `http`, `sse`, `websocket`: the transports.
//!
//! Streams are notifications of committed changes, not an event log. Nothing is
//! persisted for a client that is not connected.
//!
//! [`StreamingSink`]: crate::application::ports::StreamingSink
//! [`StreamEvent`]: crate::application::streaming::StreamEvent

pub mod auth;
pub mod channels;
pub mod fanout;
pub mod hub;
pub mod http;
pub mod render;
pub mod sse;
pub mod websocket;

pub use hub::{Hub, Limits};

use axum::http::StatusCode;

/// A streaming refusal: the HTTP status and Mastodon's error text (upstream
/// `AuthenticationError` and `RequestError`, or a birchtree cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError {
    pub status: StatusCode,
    pub message: &'static str,
}

impl StreamError {
    pub const fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }

    /// `400` (upstream `RequestError`).
    pub const fn bad_request(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// `401` (upstream `AuthenticationError`).
    pub const fn unauthorized(message: &'static str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }
}

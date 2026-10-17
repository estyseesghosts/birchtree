//! Domain-layer errors. These are not HTTP status codes; adapters map them to
//! Mastodon-compatible responses (see `api/mastodon/errors.rs`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("internal: {0}")]
    Internal(String),
}

/// Convenience alias used throughout domain and application code.
pub type Result<T> = std::result::Result<T, Error>;

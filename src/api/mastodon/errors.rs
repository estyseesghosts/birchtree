//! Mastodon-shaped error mapping.
//!
//! Mastodon renders `{ "error": "..." }` (sometimes with `details`) and uses
//! specific status codes. Domain errors are mapped here; handlers never build
//! ad-hoc error responses.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::domain::Error as DomainError;

/// An API error carrying the wire status and `error` message.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Record not found")
    }

    pub fn invalid_token() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "The access token is invalid")
    }

    pub fn insufficient_scope() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "This action is outside the authorized scopes",
        )
    }

    /// A valid token was presented but a user (not an app-only token) is needed.
    pub fn requires_user() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This method requires an authenticated user",
        )
    }

    /// A valid user token was presented but the account lacks the staff role
    /// for an admin route. Upstream's `ErrorResponses#respond_with_error`
    /// renders the Rack status text for the code, so a 403 is `Forbidden`.
    pub fn role_forbidden() -> Self {
        Self::forbidden("Forbidden")
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.message })),
        )
            .into_response()
    }
}

impl From<DomainError> for ApiError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::NotFound(_) => ApiError::not_found(),
            DomainError::InvalidInput(message) => ApiError::unprocessable(message),
            DomainError::Unauthorized(message) => {
                ApiError::new(StatusCode::UNAUTHORIZED, message)
            }
            DomainError::Forbidden(message) => ApiError::new(StatusCode::FORBIDDEN, message),
            DomainError::AlreadyExists(_) | DomainError::Conflict(_) => {
                ApiError::unprocessable("Duplicate record")
            }
            DomainError::Storage(_) | DomainError::Internal(_) => ApiError::internal(),
        }
    }
}

/// Convenience alias used by handlers.
pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_errors_map_to_statuses() {
        assert_eq!(
            ApiError::from(DomainError::NotFound("x".into())).status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::from(DomainError::Forbidden("x".into())).status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            ApiError::from(DomainError::InvalidInput("x".into())).status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            ApiError::from(DomainError::Internal("x".into())).status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn error_body_is_error_key() {
        let response = ApiError::not_found().into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], br#"{"error":"Record not found"}"#);
    }
}

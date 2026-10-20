//! Federation errors and their HTTP status.
//!
//! The crate's `Error` covers the protocol (signatures, digests, parsing,
//! URL policy, fetches). Birchtree's `Error` covers the application. Both map
//! to the status a remote server acts on: 4xx means "do not retry", 5xx means
//! "retry later".

use activitypub_federation::error::Error as ProtocolError;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::domain::Error as DomainError;

#[derive(Debug, thiserror::Error)]
pub enum FedError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("refused: {0}")]
    Refused(String),
    /// A well-formed activity of a kind outside the compatibility profile (a `Question`, for one).
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl FedError {
    /// The status an inbox or document request answers with.
    pub fn status(&self) -> StatusCode {
        match self {
            FedError::Protocol(e) => match e {
                ProtocolError::ActivitySignatureInvalid | ProtocolError::ActivityBodyDigestInvalid => {
                    StatusCode::UNAUTHORIZED
                }
                // Malformed or unsupported activity: the body does not parse as one of the accepted types.
                ProtocolError::ParseReceivedActivity(..) => StatusCode::UNPROCESSABLE_ENTITY,
                ProtocolError::UrlVerificationError(_) => StatusCode::FORBIDDEN,
                ProtocolError::NotFound
                | ProtocolError::ObjectDeleted(_)
                // WebFinger for a name or domain this server does not serve.
                | ProtocolError::WebfingerResolveFailed(_) => StatusCode::NOT_FOUND,
                // A remote document we could not use, or one we could not fetch: the remote side may retry.
                ProtocolError::ParseFetchedObject(..)
                | ProtocolError::FetchWrongId(_)
                | ProtocolError::FetchInvalidContentType(_)
                | ProtocolError::Reqwest(_)
                | ProtocolError::ReqwestMiddleware(_)
                | ProtocolError::ResponseBodyLimit
                | ProtocolError::RequestLimit => StatusCode::BAD_GATEWAY,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            FedError::Domain(e) => match e {
                DomainError::NotFound(_) => StatusCode::NOT_FOUND,
                DomainError::InvalidInput(_) => StatusCode::UNPROCESSABLE_ENTITY,
                DomainError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
                DomainError::Forbidden(_) => StatusCode::FORBIDDEN,
                DomainError::Conflict(_) | DomainError::AlreadyExists(_) => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            FedError::Refused(_) => StatusCode::FORBIDDEN,
            // Same status as an unparseable body: permanent, so the remote does not retry.
            FedError::Unsupported(_) => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

impl IntoResponse for FedError {
    fn into_response(self) -> Response {
        (self.status(), self.to_string()).into_response()
    }
}

/// For the REST layer: a federation failure in a REST request (a remote follow)
/// surfaces as the matching application error.
impl From<FedError> for DomainError {
    fn from(err: FedError) -> Self {
        match err {
            FedError::Domain(e) => e,
            FedError::Refused(msg) => DomainError::Forbidden(msg),
            FedError::Unsupported(msg) => DomainError::InvalidInput(msg),
            FedError::Protocol(ProtocolError::UrlVerificationError(msg)) => {
                DomainError::Forbidden(msg.to_string())
            }
            FedError::Protocol(e) => DomainError::Storage(format!("federation: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_failures_map_to_the_status_the_remote_acts_on() {
        let status = |e: ProtocolError| FedError::Protocol(e).status();
        // Unauthenticated: the remote must not retry with the same bytes.
        assert_eq!(status(ProtocolError::ActivitySignatureInvalid), StatusCode::UNAUTHORIZED);
        assert_eq!(status(ProtocolError::ActivityBodyDigestInvalid), StatusCode::UNAUTHORIZED);
        // Policy refusal and unparseable bodies: permanent (4xx).
        assert_eq!(status(ProtocolError::UrlVerificationError("x")), StatusCode::FORBIDDEN);
        assert_eq!(
            status(ProtocolError::ParseReceivedActivity(serde_json::from_str::<u8>("x").unwrap_err(), None)),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        // A remote document that cannot be fetched or used: 502, which the remote may retry.
        assert_eq!(status(ProtocolError::ResponseBodyLimit), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn application_failures_keep_their_meaning() {
        let status = |e: DomainError| FedError::Domain(e).status();
        assert_eq!(status(DomainError::NotFound("x".into())), StatusCode::NOT_FOUND);
        assert_eq!(status(DomainError::InvalidInput("x".into())), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(status(DomainError::Forbidden("x".into())), StatusCode::FORBIDDEN);
        assert_eq!(status(DomainError::Storage("x".into())), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(FedError::Refused("x".into()).status(), StatusCode::FORBIDDEN);
        assert_eq!(FedError::Unsupported("x".into()).status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}

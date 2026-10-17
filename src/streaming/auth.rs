//! Stream authentication (upstream `accountFromRequest`, `accountFromToken`
//! and `checkScopes`).
//!
//! Every stream needs a user token, public ones included: upstream's
//! authentication middleware runs before the channel is chosen. An app-only
//! (client-credentials) token is refused as an invalid token, as upstream's
//! `users` join refuses it.

use axum::http::StatusCode;

use super::StreamError;
use super::channels::{Channel, StreamKind, not_authorized_for_list};
use crate::application::ports::Store;
use crate::domain::Id;

/// An authenticated stream viewer.
#[derive(Debug, Clone)]
pub struct StreamViewer {
    pub token_id: Id,
    pub viewer: Id,
    pub scopes: Vec<String>,
}

/// The presented token. Upstream precedence: an `Authorization` header (with
/// `Bearer ` stripped), else the `access_token` query parameter, else the
/// `sec-websocket-protocol` header value.
pub fn presented_token(
    authorization: Option<&str>,
    query_token: Option<&str>,
    protocol: Option<&str>,
) -> Option<String> {
    let raw = match authorization.filter(|value| !value.is_empty()) {
        Some(value) => Some(value.strip_prefix("Bearer ").unwrap_or(value)),
        None => query_token
            .filter(|value| !value.is_empty())
            .or(protocol.filter(|value| !value.is_empty())),
    };
    raw.filter(|value| !value.is_empty()).map(str::to_string)
}

/// Resolve the presented token to a user viewer.
pub fn authenticate(
    store: &dyn Store,
    now_ms: u64,
    presented: Option<&str>,
) -> Result<StreamViewer, StreamError> {
    let plaintext = presented.ok_or(StreamError::unauthorized("Missing access token"))?;
    let token = store
        .token_by_plaintext(plaintext)
        .map_err(internal)?
        .ok_or(StreamError::unauthorized("Invalid access token"))?;
    if !token.is_valid_at(now_ms) {
        return Err(StreamError::unauthorized("Invalid access token"));
    }
    let Some(viewer) = token.user_id else {
        return Err(StreamError::unauthorized("Invalid access token"));
    };
    store
        .app_by_id(token.app_id)
        .map_err(internal)?
        .ok_or(StreamError::unauthorized("Invalid access token"))?;
    Ok(StreamViewer {
        token_id: token.id,
        viewer,
        scopes: token.scopes,
    })
}

/// Any one of the stream's scopes (or the broad `read`) admits the viewer.
pub fn require_scopes(viewer: &StreamViewer, channel: Channel) -> Result<(), StreamError> {
    let admitted = channel
        .required_scopes()
        .iter()
        .any(|required| viewer.scopes.iter().any(|scope| scope == required));
    if admitted {
        Ok(())
    } else {
        Err(StreamError::unauthorized(
            "Access token does not have the required scopes",
        ))
    }
}

/// A list stream needs a list the viewer owns (upstream `authorizeListAccess`).
pub fn authorize_list(store: &dyn Store, viewer: Id, kind: &StreamKind) -> Result<(), StreamError> {
    let StreamKind::List { list_id } = kind else {
        return Ok(());
    };
    match store.list_by_id(*list_id).map_err(internal)? {
        Some(list) if list.account_id == viewer => Ok(()),
        _ => Err(not_authorized_for_list()),
    }
}

fn internal(_: crate::domain::Error) -> StreamError {
    StreamError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "An unexpected error occurred",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_wins_then_query_then_protocol() {
        assert_eq!(
            presented_token(Some("Bearer abc"), Some("q"), Some("p")).as_deref(),
            Some("abc")
        );
        assert_eq!(
            presented_token(None, Some("q"), Some("p")).as_deref(),
            Some("q")
        );
        assert_eq!(presented_token(None, None, Some("p")).as_deref(), Some("p"));
        assert_eq!(presented_token(None, Some(""), None), None);
        assert_eq!(presented_token(Some(""), None, None), None);
    }

    #[test]
    fn scopes_need_read_or_the_stream_family() {
        let viewer = StreamViewer {
            token_id: Id::new(1),
            viewer: Id::new(2),
            scopes: vec!["read:notifications".into()],
        };
        assert!(require_scopes(&viewer, Channel::UserNotification).is_ok());
        assert_eq!(
            require_scopes(&viewer, Channel::User).unwrap_err().message,
            "Access token does not have the required scopes"
        );
    }
}

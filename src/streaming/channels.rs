//! Stream names, parameters and channel ids (upstream `streaming/index.js`:
//! `channelNameFromPath`, `channelNameToIds`, `streamNameFromChannelName`,
//! `checkScopes`).
//!
//! A [`Channel`] is a stream name without its parameters. [`resolve`] applies
//! the parameters (`tag`, `list`) and yields a [`StreamKind`]. The
//! [`channel_ids`] strings match upstream's Redis channel names, which upstream
//! uses to de-duplicate a WebSocket connection's subscriptions.

use axum::http::StatusCode;

use super::StreamError;
use crate::application::statuses::normalize_tag;
use crate::domain::Id;

/// Which statuses a public stream carries by origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    Any,
    Local,
    Remote,
}

/// A stream name without its parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    User,
    UserNotification,
    Direct,
    Public { locality: Locality, media: bool },
    Hashtag { local: bool },
    List,
}

/// Client-supplied stream parameters. `tag` and `list` are required by the
/// hashtag and list streams. `only_media` is folded into [`Channel`] when the
/// name is parsed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Params {
    pub tag: Option<String>,
    pub list: Option<String>,
}

/// A resolved subscription target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamKind {
    User,
    UserNotification,
    Direct,
    Public { locality: Locality, media: bool },
    /// `tag` is normalized (`normalize_tag`), as the REST tag timeline does.
    Hashtag { tag: String, local: bool },
    List { list_id: Id },
}

impl Channel {
    /// Parse an SSE path tail (`/api/v1/streaming/{tail}`): `user`,
    /// `user/notification`, `public`, `public/local`, `public/remote`,
    /// `hashtag`, `hashtag/local`, `direct` or `list`. `only_media` selects the
    /// media variant of a public stream, as upstream does.
    pub fn from_sse(tail: &str, only_media: bool) -> Option<Self> {
        Some(match tail {
            "user" => Self::User,
            "user/notification" => Self::UserNotification,
            "direct" => Self::Direct,
            "public" => Self::public(Locality::Any, only_media),
            "public/local" => Self::public(Locality::Local, only_media),
            "public/remote" => Self::public(Locality::Remote, only_media),
            "hashtag" => Self::Hashtag { local: false },
            "hashtag/local" => Self::Hashtag { local: true },
            "list" => Self::List,
            _ => return None,
        })
    }

    /// Parse a WebSocket `stream` name (`user`, `public:local:media`, ...).
    pub fn from_ws(name: &str) -> Option<Self> {
        Some(match name {
            "user" => Self::User,
            "user:notification" => Self::UserNotification,
            "direct" => Self::Direct,
            "public" => Self::public(Locality::Any, false),
            "public:local" => Self::public(Locality::Local, false),
            "public:remote" => Self::public(Locality::Remote, false),
            "public:media" => Self::public(Locality::Any, true),
            "public:local:media" => Self::public(Locality::Local, true),
            "public:remote:media" => Self::public(Locality::Remote, true),
            "hashtag" => Self::Hashtag { local: false },
            "hashtag:local" => Self::Hashtag { local: true },
            "list" => Self::List,
            _ => return None,
        })
    }

    fn public(locality: Locality, media: bool) -> Self {
        Self::Public { locality, media }
    }

    /// Scopes that admit the stream (any one of them): upstream's `checkScopes`.
    /// `user:notification` needs `read:notifications`; every other stream
    /// needs `read:statuses`. Either may be replaced by the broad `read`.
    pub fn required_scopes(self) -> &'static [&'static str] {
        match self {
            Self::UserNotification => &["read", "read:notifications"],
            _ => &["read", "read:statuses"],
        }
    }
}

/// Apply parameters to a channel. A missing `tag` or `list` is `400`; a list id
/// that is not a number is `401` (upstream answers the same for an unknown list,
/// which the caller checks against the store).
pub fn resolve(channel: Channel, params: &Params) -> Result<StreamKind, StreamError> {
    Ok(match channel {
        Channel::User => StreamKind::User,
        Channel::UserNotification => StreamKind::UserNotification,
        Channel::Direct => StreamKind::Direct,
        Channel::Public { locality, media } => StreamKind::Public { locality, media },
        Channel::Hashtag { local } => {
            let raw = params
                .tag
                .as_deref()
                .filter(|tag| !tag.is_empty())
                .ok_or_else(|| StreamError::bad_request("Missing tag name parameter"))?;
            StreamKind::Hashtag {
                tag: normalize_tag(raw),
                local,
            }
        }
        Channel::List => {
            let raw = params
                .list
                .as_deref()
                .filter(|list| !list.is_empty())
                .ok_or_else(|| StreamError::bad_request("Missing list name parameter"))?;
            let list_id = raw.parse::<Id>().map_err(|_| not_authorized_for_list())?;
            StreamKind::List { list_id }
        }
    })
}

/// Upstream's answer for a list the viewer may not stream.
pub fn not_authorized_for_list() -> StreamError {
    StreamError::new(
        StatusCode::UNAUTHORIZED,
        "Not authorized to stream this list",
    )
}

/// The WebSocket `stream` array for a subscription (upstream
/// `streamNameFromChannelName`): the name, then the `tag` or `list` parameter
/// as the client sent it.
pub fn stream_names(channel_name: &str, channel: Channel, params: &Params) -> Vec<String> {
    let mut names = vec![channel_name.to_string()];
    match channel {
        Channel::List => {
            if let Some(list) = &params.list {
                names.push(list.clone());
            }
        }
        Channel::Hashtag { .. } => {
            if let Some(tag) = &params.tag {
                names.push(tag.clone());
            }
        }
        _ => {}
    }
    names
}

/// The upstream channel ids a subscription listens on. `notifications_scope`
/// is true when the token may read notifications, which folds notifications
/// into the `user` stream (upstream `channelsForUserStream`).
pub fn channel_ids(kind: &StreamKind, viewer: Id, notifications_scope: bool) -> Vec<String> {
    match kind {
        StreamKind::User => {
            let mut ids = vec![format!("timeline:{viewer}")];
            if notifications_scope {
                ids.push(format!("timeline:{viewer}:notifications"));
            }
            ids
        }
        StreamKind::UserNotification => vec![format!("timeline:{viewer}:notifications")],
        StreamKind::Direct => vec![format!("timeline:direct:{viewer}")],
        StreamKind::Public { locality, media } => {
            let mut id = "timeline:public".to_string();
            match locality {
                Locality::Any => {}
                Locality::Local => id.push_str(":local"),
                Locality::Remote => id.push_str(":remote"),
            }
            if *media {
                id.push_str(":media");
            }
            vec![id]
        }
        StreamKind::Hashtag { tag, local } => {
            let mut id = format!("timeline:hashtag:{tag}");
            if *local {
                id.push_str(":local");
            }
            vec![id]
        }
        StreamKind::List { list_id } => vec![format!("timeline:list:{list_id}")],
    }
}

/// The de-duplication key of a subscription (upstream joins the channel ids).
pub fn subscription_key(ids: &[String]) -> String {
    ids.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_paths_map_to_upstream_channels() {
        assert_eq!(Channel::from_sse("user", false), Some(Channel::User));
        assert_eq!(
            Channel::from_sse("user/notification", false),
            Some(Channel::UserNotification)
        );
        assert_eq!(
            Channel::from_sse("public", true),
            Some(Channel::Public { locality: Locality::Any, media: true })
        );
        assert_eq!(
            Channel::from_sse("public/local", false),
            Some(Channel::Public { locality: Locality::Local, media: false })
        );
        assert_eq!(Channel::from_sse("hashtag/local", false), Some(Channel::Hashtag { local: true }));
        assert_eq!(Channel::from_sse("nope", false), None);
    }

    #[test]
    fn ws_names_include_the_media_and_locality_variants() {
        assert_eq!(
            Channel::from_ws("public:remote:media"),
            Some(Channel::Public { locality: Locality::Remote, media: true })
        );
        assert_eq!(Channel::from_ws("user:notification"), Some(Channel::UserNotification));
        assert_eq!(Channel::from_ws("bogus"), None);
    }

    #[test]
    fn notification_stream_needs_its_own_scope_family() {
        assert_eq!(Channel::UserNotification.required_scopes(), &["read", "read:notifications"]);
        assert_eq!(Channel::User.required_scopes(), &["read", "read:statuses"]);
    }

    #[test]
    fn hashtag_and_list_require_their_parameter() {
        let none = Params::default();
        assert_eq!(
            resolve(Channel::Hashtag { local: false }, &none).unwrap_err().message,
            "Missing tag name parameter"
        );
        assert_eq!(
            resolve(Channel::List, &none).unwrap_err().message,
            "Missing list name parameter"
        );
        let bad_list = Params { list: Some("abc".into()), ..Params::default() };
        assert_eq!(
            resolve(Channel::List, &bad_list).unwrap_err().message,
            "Not authorized to stream this list"
        );
    }

    #[test]
    fn hashtag_is_normalized_and_keys_match_upstream_channels() {
        let params = Params { tag: Some("#Rust_Lang".into()), ..Params::default() };
        let kind = resolve(Channel::Hashtag { local: true }, &params).unwrap();
        assert_eq!(
            kind,
            StreamKind::Hashtag { tag: "rust_lang".into(), local: true }
        );
        let ids = channel_ids(&kind, Id::new(7), false);
        assert_eq!(ids, vec!["timeline:hashtag:rust_lang:local".to_string()]);
    }

    #[test]
    fn user_stream_folds_notifications_only_with_the_scope() {
        let viewer = Id::new(7);
        assert_eq!(channel_ids(&StreamKind::User, viewer, false).len(), 1);
        assert_eq!(channel_ids(&StreamKind::User, viewer, true).len(), 2);
    }

    #[test]
    fn websocket_frame_stream_names_carry_the_parameter() {
        let params = Params { tag: Some("rust".into()), ..Params::default() };
        assert_eq!(
            stream_names("hashtag", Channel::Hashtag { local: false }, &params),
            vec!["hashtag".to_string(), "rust".to_string()]
        );
        assert_eq!(stream_names("user", Channel::User, &params), vec!["user".to_string()]);
    }
}

//! Conversations (direct-message threads) and Web Push subscriptions.
//!
//! Upstream reference: Mastodon v4.7.3 (`8ff11c2b`)
//! `app/models/conversation.rb`, `app/models/account_conversation.rb`,
//! `app/models/conversation_mute.rb`, `app/models/web/push_subscription.rb`,
//! `app/controllers/api/v1/conversations_controller.rb`,
//! `app/controllers/api/v1/statuses/mutes_controller.rb`,
//! `app/controllers/api/v1/push/subscriptions_controller.rb`.
//!
//! A conversation has no persisted row of its own in this design: birchtree
//! statuses carry no `conversation_id` column, so a conversation is derived
//! per viewer as the reply thread (by `in_reply_to_id`) rooted at a
//! direct-visibility status. The thread root's numeric id is the stable,
//! opaque conversation id on the wire. Only per-viewer *state* (read
//! position, unread override, deletion) and conversation mutes are persisted
//! (see `application::conversations` for the store traits the `redb` adapter
//! implements).
//!
//! This module knows no HTTP DTO shapes and no storage types.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::Id;

/// Per-viewer conversation state, keyed by `(viewer, thread root)`.
///
/// `seen_status_id` is the thread's last status when the viewer last marked
/// it read. `unread_override` records an explicit `POST :id/unread`
/// (`Some(true)`) or is cleared by `POST :id/read` (`None`, so a newer
/// message flips the thread unread again). `deleted` is the viewer's
/// per-viewer `DELETE :id` (it removes participation state, never statuses).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationState {
    pub viewer_id: Id,
    pub root_id: Id,
    pub last_status_id: Id,
    pub seen_status_id: Option<Id>,
    pub unread_override: Option<bool>,
    pub unread: bool,
    pub deleted: bool,
    pub updated_at_ms: u64,
}

impl ConversationState {
    /// Fresh state for a thread the viewer never touched.
    pub fn fresh(viewer_id: Id, root_id: Id, last_status_id: Id, now_ms: u64) -> Self {
        Self {
            viewer_id,
            root_id,
            last_status_id,
            seen_status_id: None,
            unread_override: None,
            unread: false,
            deleted: false,
            updated_at_ms: now_ms,
        }
    }

    /// Whether the thread reads unread given its current last status and
    /// author. An explicit unread mark always wins; a read mark holds until a
    /// newer status (or a status by someone else) arrives.
    pub fn computed_unread(
        &self,
        last_status_id: Id,
        last_author_id: Id,
        viewer_id: Id,
    ) -> bool {
        if let Some(forced) = self.unread_override {
            return forced;
        }
        if self.seen_status_id == Some(last_status_id) {
            return false;
        }
        last_author_id != viewer_id
    }
}

/// A Web Push subscription bound to one access token.
///
/// Mirrors `Web::PushSubscription`: the endpoint and keys, plus the `data`
/// payload (`policy` and per-type `alerts`). Keys are stored as received
/// (base64url); they are decoded and length-checked at write time (see
/// [`validate_push_keys`]) and again before delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebPushSubscription {
    pub id: Id,
    pub token_id: Id,
    pub user_id: Id,
    pub endpoint: String,
    pub key_p256dh: String,
    pub key_auth: String,
    pub standard: bool,
    pub policy: String,
    pub alerts: HashMap<String, bool>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Notification policies a subscription accepts (`data[policy]`).
///
/// Unknown values are stored as given and behave as `none` (upstream's
/// `policy_allows_notification?` falls through to falsy for anything but
/// `all`/`followed`/`follower`/`none`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPolicy {
    All,
    Followed,
    Follower,
    None,
    Unknown,
}

impl PushPolicy {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "all" => PushPolicy::All,
            "followed" => PushPolicy::Followed,
            "follower" => PushPolicy::Follower,
            "none" => PushPolicy::None,
            _ => PushPolicy::Unknown,
        }
    }
}

/// Validate a push subscription endpoint (`Web::PushSubscription`
/// `validates :endpoint, presence: true, url: true`).
pub fn validate_push_endpoint(endpoint: &str) -> Result<(), String> {
    if endpoint.trim().is_empty() {
        return Err("Validation failed: Endpoint can't be blank".to_string());
    }
    let url = url::Url::parse(endpoint)
        .map_err(|_| "Validation failed: Endpoint is invalid".to_string())?;
    match url.scheme() {
        "http" | "https" => {
            if url.host_str().is_none_or(|host| host.is_empty()) {
                return Err("Validation failed: Endpoint is invalid".to_string());
            }
            Ok(())
        }
        _ => Err("Validation failed: Endpoint is invalid".to_string()),
    }
}

/// Validate the subscription keys (`WebPushKeyValidator` encrypts a probe;
/// structurally that is: decodable base64url, a 65-byte uncompressed P-256
/// point for `p256dh`, 16 bytes of auth secret).
pub fn validate_push_keys(p256dh_b64: &str, auth_b64: &str) -> Result<(), String> {
    use base64::Engine as _;
    if p256dh_b64.trim().is_empty() || auth_b64.trim().is_empty() {
        return Err("Validation failed: Invalid key".to_string());
    }
    let p256dh = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(p256dh_b64.trim())
        .map_err(|_| "Validation failed: Invalid key".to_string())?;
    let auth = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(auth_b64.trim())
        .map_err(|_| "Validation failed: Invalid key".to_string())?;
    if p256dh.len() != 65 || p256dh[0] != 0x04 || auth.len() != 16 {
        return Err("Validation failed: Invalid key".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_defaults_to_author_check() {
        let viewer = Id::new(1);
        let other = Id::new(2);
        let state = ConversationState::fresh(viewer, Id::new(9), Id::new(10), 0);
        assert!(state.computed_unread(Id::new(10), other, viewer));
        assert!(!state.computed_unread(Id::new(10), viewer, viewer));
    }

    #[test]
    fn read_mark_holds_until_new_status() {
        let viewer = Id::new(1);
        let other = Id::new(2);
        let mut state = ConversationState::fresh(viewer, Id::new(9), Id::new(10), 0);
        state.seen_status_id = Some(Id::new(10));
        assert!(!state.computed_unread(Id::new(10), other, viewer));
        assert!(state.computed_unread(Id::new(11), other, viewer));
    }

    #[test]
    fn unread_override_wins() {
        let viewer = Id::new(1);
        let mut state = ConversationState::fresh(viewer, Id::new(9), Id::new(10), 0);
        state.seen_status_id = Some(Id::new(10));
        state.unread_override = Some(true);
        assert!(state.computed_unread(Id::new(10), viewer, viewer));
    }

    #[test]
    fn endpoint_validation() {
        assert!(validate_push_endpoint("https://push.example/sub").is_ok());
        assert_eq!(
            validate_push_endpoint("").unwrap_err(),
            "Validation failed: Endpoint can't be blank"
        );
        assert_eq!(
            validate_push_endpoint("gopher://x/y").unwrap_err(),
            "Validation failed: Endpoint is invalid"
        );
    }

    #[test]
    fn key_validation_shapes() {
        use base64::Engine as _;
        let mut p256dh = vec![0x04];
        p256dh.extend([7u8; 64]);
        let auth = [9u8; 16];
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        assert!(validate_push_keys(&enc.encode(&p256dh), &enc.encode(auth)).is_ok());
        assert!(validate_push_keys("", &enc.encode(auth)).is_err());
        assert!(validate_push_keys(&enc.encode(&[1u8; 65]), &enc.encode(auth)).is_err());
        assert!(validate_push_keys(&enc.encode(&p256dh), &enc.encode([1u8; 8])).is_err());
    }
}

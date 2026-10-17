//! Status visibility and interaction policy.
//!
//! One policy module feeds every read surface (REST show/context/lists,
//! later timelines, search, streaming, quote previews) and every interaction
//! gate. `PairState` is the viewer/author relationship input computed by the
//! `RelationshipStore`.

use crate::application::ports::PairState;
use crate::domain::{Id, QuotePolicy, Status, Visibility};

/// Whether the viewer is mentioned by the status (visible or silent mentions).
pub fn is_mentioned(viewer: Option<Id>, status: &Status) -> bool {
    match viewer {
        Some(viewer) => {
            status.mentions.contains(&viewer) || status.silent_mentions.contains(&viewer)
        }
        None => false,
    }
}

/// Predicate 1: can this viewer fetch this status?
///
/// Mirrors Mastodon v4.7.3 `StatusPolicy#show?` for local statuses:
/// deleted -> never; `public`/`unlisted` -> anyone except when the author
/// blocks the viewer; `private` -> author, followers, mentioned; `direct` ->
/// author or mentioned only.
pub fn can_fetch(viewer: Option<Id>, status: &Status, pair: PairState) -> bool {
    if status.is_deleted() {
        return false;
    }
    match status.visibility {
        // Anonymous OK; a blocked viewer is refused (author-side block).
        Visibility::Public | Visibility::Unlisted => !pair.blocking,
        Visibility::Private => {
            viewer == Some(status.account_id) || pair.followed_by || is_mentioned(viewer, status)
        }
        Visibility::Direct => viewer == Some(status.account_id) || is_mentioned(viewer, status),
    }
}

/// `StatusPolicy#reblog?`: not direct, private only by owner, fetchable, and
/// the author does not block the viewer (`can_fetch` covers the block).
pub fn can_reblog(viewer: Id, status: &Status, pair: PairState) -> bool {
    status.visibility.is_rebloggable()
        && (status.visibility != Visibility::Private || viewer == status.account_id)
        && can_fetch(Some(viewer), status, pair)
}

/// `StatusPolicy#favourite?` = `show?` (blocks included via the pair state).
pub fn can_favourite(viewer: Id, status: &Status, pair: PairState) -> bool {
    can_fetch(Some(viewer), status, pair)
}

/// `StatusPolicy#bookmark?` = `show?`.
pub fn can_bookmark(viewer: Id, status: &Status, pair: PairState) -> bool {
    can_fetch(Some(viewer), status, pair)
}

/// How the viewer may quote a status (`quote_policy_for_account`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotePermission {
    /// The viewer is the quoted author (self-quotes always allowed).
    SelfQuote,
    /// Allowed by the status's automatic quote policy.
    Automatic,
    /// Denied (`nobody`, direct, reblog, unfetchable, or blocked).
    Denied,
}

/// Compute the viewer's quote permission (`StatusPolicy#quote?` folded with
/// the status's `quote_approval_policy`).
pub fn quote_permission(viewer: Option<Id>, status: &Status, pair: PairState) -> QuotePermission {
    // Direct messages and reblogs are never quotable.
    if status.visibility == Visibility::Direct || status.is_reblog() {
        return QuotePermission::Denied;
    }
    if !can_fetch(viewer, status, pair) {
        return QuotePermission::Denied;
    }
    if viewer.is_some_and(|v| v == status.account_id) {
        return QuotePermission::SelfQuote;
    }
    match status.quote_policy {
        QuotePolicy::Public => QuotePermission::Automatic,
        // `followers` = the viewer follows the author.
        QuotePolicy::Followers if pair.followed_by => QuotePermission::Automatic,
        QuotePolicy::Followers | QuotePolicy::Nobody => QuotePermission::Denied,
    }
}

/// `StatusPolicy#update?`/`destroy?`: owned.
pub fn is_owner(viewer: Id, status: &Status) -> bool {
    viewer == status.account_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{QuotePolicy, Status, Visibility};

    fn status(visibility: Visibility, author: u64) -> Status {
        Status {
            id: Id::new(100),
            account_id: Id::new(author),
            uri: String::new(),
            url: String::new(),
            source_text: String::new(),
            content_html: String::new(),
            created_at_ms: 0,
            edited_at_ms: None,
            deleted_at_ms: None,
            language: None,
            visibility,
            sensitive: false,
            spoiler_text: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog_of_id: None,
            media_ids: Vec::new(),
            poll_id: None,
            quote_policy: QuotePolicy::default(),
            mentions: Vec::new(),
            silent_mentions: Vec::new(),
            tags: Vec::new(),
            application: None,
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            quotes_count: 0,
        }
    }

    #[test]
    fn public_is_fetchable_by_anyone() {
        let s = status(Visibility::Public, 1);
        assert!(can_fetch(None, &s, PairState::default()));
        assert!(can_fetch(Some(Id::new(2)), &s, PairState::default()));
    }

    #[test]
    fn blocked_viewer_cannot_fetch_public() {
        let s = status(Visibility::Public, 1);
        let pair = PairState {
            blocking: true,
            ..PairState::default()
        };
        assert!(!can_fetch(Some(Id::new(2)), &s, pair));
        assert!(!can_favourite(Id::new(2), &s, pair));
    }

    #[test]
    fn private_requires_follow_or_mention() {
        let s = status(Visibility::Private, 1);
        assert!(!can_fetch(Some(Id::new(2)), &s, PairState::default()));
        let follower = PairState {
            followed_by: true,
            ..PairState::default()
        };
        assert!(can_fetch(Some(Id::new(2)), &s, follower));
        assert!(!can_reblog(Id::new(2), &s, follower));
        assert!(can_reblog(Id::new(1), &s, PairState::default()));
    }

    #[test]
    fn direct_is_author_or_mentioned_only() {
        let mut s = status(Visibility::Direct, 1);
        s.mentions.push(Id::new(3));
        assert!(!can_fetch(Some(Id::new(2)), &s, PairState::default()));
        assert!(can_fetch(Some(Id::new(3)), &s, PairState::default()));
        assert!(!can_reblog(Id::new(3), &s, PairState::default()));
    }

    #[test]
    fn quote_policy_gates_quote_permission() {
        let mut s = status(Visibility::Public, 1);
        s.quote_policy = QuotePolicy::Followers;
        assert_eq!(
            quote_permission(Some(Id::new(1)), &s, PairState::default()),
            QuotePermission::SelfQuote
        );
        assert_eq!(
            quote_permission(Some(Id::new(2)), &s, PairState::default()),
            QuotePermission::Denied
        );
        let follows = PairState {
            followed_by: true,
            ..PairState::default()
        };
        assert_eq!(
            quote_permission(Some(Id::new(2)), &s, follows),
            QuotePermission::Automatic
        );
        s.quote_policy = QuotePolicy::Nobody;
        assert_eq!(
            quote_permission(Some(Id::new(2)), &s, follows),
            QuotePermission::Denied
        );
    }
}

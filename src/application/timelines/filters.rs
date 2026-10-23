//! Viewer-policy filters for the four timeline feeds (S12 bullet 1).
//!
//! Each filter is the `include` half of one feed; the scan half lives in
//! `super` (the orchestrators) and in the `TimelineStore`. No behavior
//! change: predicates moved verbatim out of `timelines.rs`.

use std::collections::HashSet;

use crate::application::policy::{HomeMentionPolicy, is_mentioned};
use crate::application::ports::TimelineFilter;
use crate::domain::{Id, RepliesPolicy, Status, Visibility};

use super::ViewerScope;

/// Home feed policy: own statuses, plus active follows that pass the gates.
pub(super) struct HomeFilter<'a> {
    pub(super) scope: ViewerScope<'a>,
    pub(super) viewer: Id,
}

impl TimelineFilter for HomeFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        let author = status.account_id;
        // The viewer always sees their own statuses in their own home feed.
        if author == self.viewer {
            return true;
        }
        if self.scope.on_exclusive_list(author) {
            return false;
        }
        if !self.scope.follows(author) || !self.scope.author_allowed(author) {
            return false;
        }
        // Private statuses from a followed author are visible; direct ones
        // only to the people they name.
        if status.visibility == Visibility::Direct && !is_mentioned(Some(self.viewer), status) {
            return false;
        }
        if is_orphan_reply(status) {
            return false;
        }
        if author != self.viewer && self.scope.unresolved_reply(status) {
            return false;
        }
        if let Some(parent) = foreign_reply_parent(status) {
            if parent != self.viewer && !self.scope.follows(parent) {
                return false;
            }
        }
        if status.is_reblog()
            && (!self.scope.shows_reblogs_of(author) || !self.scope.reblog_target_allowed(status))
        {
            return false;
        }
        // D23: the viewer's follow of the author may restrict languages.
        if !self
            .scope
            .follow_language_allows(author, status.language.as_deref())
        {
            return false;
        }
        // D22: in `Filter` mode, statuses mentioning a blocked/muted account
        // leave home (upstream behavior, opt-in here). `Show` (default) and
        // `Collapse` (DTO fold) keep them.
        if self.scope.mention_policy() == HomeMentionPolicy::Filter
            && home_mentions_filtered(&self.scope, status)
        {
            return false;
        }
        true
    }
}

/// Public feed policy: public statuses, self-replies only, no reblogs (the
/// store already drops reblogs for this feed).
pub(super) struct PublicFilter<'a> {
    pub(super) scope: ViewerScope<'a>,
}

impl TimelineFilter for PublicFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        let self_reply_or_plain = status.in_reply_to_id.is_none()
            || status.in_reply_to_account_id == Some(status.account_id);
        // D21: upstream `PublicFeed#language_scope` — the viewer's
        // `chosen_languages` restrict even the public timeline (but not the
        // tag timeline: `TagFeed` has no language scope).
        status.visibility == Visibility::Public
            && self_reply_or_plain
            && self.scope.author_allowed(status.account_id)
            && self
                .scope
                .chosen_language_allows(status.language.as_deref())
    }
}

/// Hashtag feed policy: the public feed rules, but replies and reblogs stay.
pub(super) struct TagFilter<'a> {
    pub(super) scope: ViewerScope<'a>,
}

impl TimelineFilter for TagFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        status.visibility == Visibility::Public
            && self.scope.author_allowed(status.account_id)
            && (!status.is_reblog() || self.scope.reblog_target_allowed(status))
    }
}

/// List feed policy: members' statuses under the home gates, with the list's
/// own replies rule instead of the home one.
pub(super) struct ListFilter<'a> {
    pub(super) scope: ViewerScope<'a>,
    pub(super) viewer: Id,
    pub(super) members: HashSet<Id>,
    pub(super) policy: RepliesPolicy,
}

impl TimelineFilter for ListFilter<'_> {
    fn include(&self, status: &Status) -> bool {
        let author = status.account_id;
        if author == self.viewer {
            return true;
        }
        if !self.members.contains(&author) || !self.scope.author_allowed(author) {
            return false;
        }
        if status.visibility == Visibility::Direct && !is_mentioned(Some(self.viewer), status) {
            return false;
        }
        if is_orphan_reply(status) {
            return false;
        }
        if author != self.viewer && self.scope.unresolved_reply(status) {
            return false;
        }
        if let Some(parent) = foreign_reply_parent(status) {
            let kept = parent == self.viewer
                || match self.policy {
                    RepliesPolicy::Followed => self.scope.follows(parent),
                    RepliesPolicy::List => self.members.contains(&parent),
                    RepliesPolicy::None => false,
                };
            if !kept {
                return false;
            }
        }
        if status.is_reblog()
            && (!self.scope.shows_reblogs_of(author) || !self.scope.reblog_target_allowed(status))
        {
            return false;
        }
        true
    }
}

/// Upstream's streaming-side block and mute check on a status's mentions (the
/// author is covered by `author_allowed`): the viewer blocks or mutes one of
/// them. Upstream applies it to public and hashtag streams only.
pub(super) fn mentions_blocked_or_muted(scope: &ViewerScope<'_>, status: &Status) -> bool {
    scope.viewer.is_some()
        && status.mentions.iter().any(|mention| {
            scope
                .facts(*mention)
                .is_some_and(|f| f.pair.blocking || f.pair.muting)
        })
}

/// D22: whether `status` mentions someone the viewer blocks or mutes — or
/// (for reblogs) the reblogged status does. Upstream's `filter_from_home`
/// `check_for_blocks` also covers the author and the reblog author, but those
/// are already gated by `author_allowed` / `reblog_target_allowed` here, so
/// only mentions remain.
pub(super) fn home_mentions_filtered(scope: &ViewerScope<'_>, status: &Status) -> bool {
    if mentions_blocked_or_muted(scope, status) {
        return true;
    }
    let Some(target_id) = status.reblog_of_id else {
        return false;
    };
    match scope.store.status_by_id(target_id) {
        Ok(Some(target)) => mentions_blocked_or_muted(scope, &target),
        // An unloadable reblog target fails closed in
        // `reblog_target_allowed`; it must not filter here.
        Ok(None) | Err(_) => false,
    }
}

/// A reply whose parent status is set but whose parent account is unknown.
/// Such a status can never be rendered, so no feed shows it.
pub(super) fn is_orphan_reply(status: &Status) -> bool {
    status.in_reply_to_id.is_some() && status.in_reply_to_account_id.is_none()
}

/// The account a reply is directed at, when it is not the author's own thread.
pub(super) fn foreign_reply_parent(status: &Status) -> Option<Id> {
    status
        .in_reply_to_account_id
        .filter(|parent| *parent != status.account_id)
}

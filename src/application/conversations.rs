//! Conversations (direct-message threads) and conversation mutes.
//!
//! Upstream reference: Mastodon v4.7.3 (`8ff11c2b`)
//! `app/controllers/api/v1/conversations_controller.rb`,
//! `app/controllers/api/v1/statuses/mutes_controller.rb`,
//! `app/models/account_conversation.rb` (`add_status` / `remove_status` /
//! `participants_from_status`, paginated by `last_status_id`).
//!
//! A conversation is derived per viewer: every non-deleted direct-visibility
//! status the viewer may fetch (authored by the viewer, or mentioning them)
//! belongs to the thread rooted at its oldest ancestor. The thread root's
//! numeric id is the stable conversation id. Only per-viewer state (read
//! position, explicit unread, deletion) and conversation mutes are persisted,
//! through [`ConversationStore`]; the `redb` adapter implements it (schema v9,
//! applied by the parent patch).
//!
//! One transaction per method: state writes commit the state row alone (they
//! touch no counters or jobs). Streaming fan-out happens after commit, in the
//! adapter, via [`crate::application::streaming::StreamEvent::Conversation`].

use std::collections::{HashMap, HashSet};

use crate::application::policy;
use crate::application::ports::Store;
use crate::domain::{
    ConversationState, Error, Id, Result, Status, Visibility, WebPushSubscription,
};

/// Recent candidates read per side when deriving a viewer's threads.
///
/// Threads are discovered from the viewer's own recent direct statuses plus
/// recent direct statuses mentioning them, so derivation is bounded. A quiet
/// thread older than both windows is not listed until it gains a message
/// (documented bound, like the other scan budgets).
pub const CANDIDATE_CAP: usize = 200;

/// Maximum ancestors walked to find a thread root, and maximum statuses read
/// per thread when accumulating participants.
pub const THREAD_WALK_CAP: usize = 100;

/// Conversation list page size (upstream `LIMIT = 20`, max `40`).
pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 40;

/// Store surface for per-viewer conversation state and conversation mutes.
///
/// Implemented once by the `redb` adapter (parent patch). The parent also
/// registers these traits as supertraits of [`Store`], so use cases take
/// `&dyn Store` directly.
pub trait ConversationStore: Send + Sync {
    /// State row for `(viewer, root)`, if the viewer ever touched the thread.
    fn conversation_state(&self, viewer: Id, root: Id) -> Result<Option<ConversationState>>;

    /// State rows for several roots in ONE transaction (missing roots omitted).
    fn conversation_states_for(
        &self,
        viewer: Id,
        roots: &[Id],
    ) -> Result<HashMap<Id, ConversationState>>;

    /// All roots the viewer has state for (for listing deleted markers and
    /// mute-adjacent reads), bounded by `cap`.
    fn viewer_conversation_roots(&self, viewer: Id, cap: usize) -> Result<Vec<Id>>;

    /// Insert or refresh the state row for `(viewer, root)` in ONE transaction.
    fn put_conversation_state(&self, state: ConversationState) -> Result<ConversationState>;

    /// Mute the viewer's conversation rooted at `root`. Returns `true` when
    /// the mute is new (idempotent).
    fn mute_conversation(&self, account: Id, root: Id, now_ms: u64) -> Result<bool>;

    /// Remove the viewer's conversation mute. Returns `true` when one existed.
    fn unmute_conversation(&self, account: Id, root: Id) -> Result<bool>;

    /// Whether the viewer mutes the conversation rooted at `root`.
    fn conversation_muted(&self, account: Id, root: Id) -> Result<bool>;

    /// Mute flags for several roots in ONE transaction (missing = unmuted).
    fn conversation_muted_map(
        &self,
        account: Id,
        roots: &[Id],
    ) -> Result<HashMap<Id, bool>>;

    /// Non-deleted statuses mentioning `account`, newest first, bounded by
    /// `limit` (derived from the mentions index; no unbounded scan).
    fn statuses_mentioning(&self, account: Id, limit: usize) -> Result<Vec<Status>>;
}

/// One derived conversation for a viewer.
#[derive(Debug, Clone)]
pub struct ConversationView {
    /// Thread root id (the stable, opaque conversation id).
    pub root_id: Id,
    /// Newest fetchable thread status (the `last_status`).
    pub last_status: Status,
    /// Union of thread authors and mentioned accounts, minus the viewer
    /// (upstream's `participants_from_status`, accumulated over the thread).
    pub participant_ids: Vec<Id>,
    /// Whether the thread reads unread for the viewer.
    pub unread: bool,
}

/// Query bounds for the conversation index (over `last_status_id`, as
/// upstream paginates by `last_status_id`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ConversationQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
}

/// Walk `in_reply_to_id` to the thread root, batching one level per
/// transaction. A missing or deleted parent ends the walk (the status is its
/// own root from the viewer's perspective).
pub fn thread_root(store: &dyn Store, status: &Status) -> Result<Id> {
    let mut current = status.clone();
    for _ in 0..THREAD_WALK_CAP {
        let Some(parent_id) = current.in_reply_to_id else {
            break;
        };
        match store.status_by_id(parent_id)? {
            Some(parent) if !parent.is_deleted() => current = parent,
            _ => break,
        }
    }
    Ok(current.id)
}

/// Thread roots for a batch of statuses, level by level (one transaction per
/// level, never one per status). A missing or deleted ancestor ends the walk:
/// the status is its own root from the viewer's perspective.
pub fn thread_roots(
    store: &dyn Store,
    statuses: &[Status],
) -> Result<HashMap<Id, Id>> {
    let mut roots: HashMap<Id, Id> = HashMap::new();
    // `cursor` maps each unresolved status to the ancestor under examination.
    let mut cursor: HashMap<Id, Id> = HashMap::new();
    for status in statuses {
        match status.in_reply_to_id {
            None => {
                roots.insert(status.id, status.id);
            }
            Some(parent) => {
                cursor.insert(status.id, parent);
            }
        }
    }
    for _ in 0..THREAD_WALK_CAP {
        if cursor.is_empty() {
            break;
        }
        let need: Vec<Id> = cursor.values().copied().collect::<HashSet<_>>().into_iter().collect();
        let found = store.statuses_by_ids(&need)?;
        let by_id: HashMap<Id, Status> =
            found.into_iter().map(|status| (status.id, status)).collect();
        let mut next: HashMap<Id, Id> = HashMap::new();
        for (status_id, ancestor_id) in cursor {
            match by_id.get(&ancestor_id) {
                Some(parent) if !parent.is_deleted() => match parent.in_reply_to_id {
                    None => {
                        roots.insert(status_id, parent.id);
                        roots.insert(parent.id, parent.id);
                    }
                    Some(grandparent) => {
                        next.insert(status_id, grandparent);
                    }
                },
                // Missing or deleted ancestors break the thread: the status
                // is its own root (never silently promoted elsewhere).
                _ => {
                    roots.insert(status_id, status_id);
                }
            }
        }
        cursor = next;
    }
    // A walk that hit the cap leaves stragglers; they are their own roots
    // rather than dropped.
    for (status_id, _) in cursor {
        roots.entry(status_id).or_insert(status_id);
    }
    for status in statuses {
        roots.entry(status.id).or_insert(status.id);
    }
    Ok(roots)
}

/// Whether the viewer may see `status` on the conversation surface (the
/// shared show gate: missing, deleted and otherwise invisible are
/// indistinguishable). Shared with the endpoint's streaming render.
pub fn fetchable(store: &dyn Store, viewer: Id, status: &Status, now_ms: u64) -> Result<bool> {
    if status.is_deleted() {
        return Ok(false);
    }
    let pair = store.pair_state(status.account_id, viewer, now_ms)?;
    Ok(policy::can_fetch(Some(viewer), status, pair))
}

/// Candidate direct statuses for `viewer`: their own recent direct statuses
/// plus recent direct statuses mentioning them.
fn candidates(store: &dyn Store, viewer: Id) -> Result<Vec<Status>> {
    use crate::application::ports::AccountStatusesQuery;
    let own = store.statuses_by_account(
        viewer,
        AccountStatusesQuery {
            max_id: None,
            since_id: None,
            min_id: None,
            limit: CANDIDATE_CAP,
            pinned_only: false,
            exclude_replies: false,
            exclude_reblogs: false,
        },
    )?;
    let mentioned = store.statuses_mentioning(viewer, CANDIDATE_CAP)?;
    let mut by_id: HashMap<Id, Status> = HashMap::new();
    for status in own.into_iter().chain(mentioned) {
        if status.visibility == Visibility::Direct {
            by_id.insert(status.id, status);
        }
    }
    Ok(by_id.into_values().collect())
}

/// Group candidates into threads and assemble one [`ConversationView`] per
/// thread (newest thread first by `last_status_id`).
fn assemble(
    store: &dyn Store,
    viewer: Id,
    candidates: Vec<Status>,
    now_ms: u64,
) -> Result<Vec<ConversationView>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let roots = thread_roots(store, &candidates)?;
    let mut threads: HashMap<Id, Vec<Status>> = HashMap::new();
    for status in candidates {
        let root = roots.get(&status.id).copied().unwrap_or(status.id);
        threads.entry(root).or_default().push(status);
    }
    // Fetchability needs pair states per author; batch them once.
    let authors: Vec<Id> = threads
        .values()
        .flat_map(|members| members.iter().map(|status| status.account_id))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let pairs = store.batch_pair_states(viewer, &authors, now_ms)?;
    let states = store.conversation_states_for(
        viewer,
        &threads.keys().copied().collect::<Vec<_>>(),
    )?;

    let mut out = Vec::new();
    for (root, mut members) in threads {
        // Deleted or invisible members never surface; a thread left with no
        // fetchable status is skipped (upstream's `remove_status` destroys or
        // shrinks the row on delete).
        members.retain(|status| {
            if status.is_deleted() {
                return false;
            }
            match pairs.get(&status.account_id) {
                // `incoming` is `pair_state(author, viewer)`, the same pair
                // the shared show gate reads.
                Some((incoming, _)) => {
                    policy::can_fetch(Some(viewer), status, *incoming)
                        && (status.account_id == viewer
                            || policy::is_mentioned(Some(viewer), status))
                }
                None => false,
            }
        });
        if members.is_empty() {
            continue;
        }
        members.sort_by_key(|status| status.id);
        let last = members.last().expect("non-empty").clone();
        if let Some(state) = states.get(&root) {
            if state.deleted {
                continue;
            }
        }
        let mut participants: HashSet<Id> = HashSet::new();
        for status in members.iter().take(THREAD_WALK_CAP) {
            participants.insert(status.account_id);
            participants.extend(status.mentions.iter().copied());
        }
        participants.remove(&viewer);
        let mut participant_ids: Vec<Id> = participants.into_iter().collect();
        participant_ids.sort();
        // A direct thread always names someone besides the viewer; a
        // self-addressed edge case keeps the author so `accounts` is never
        // empty (upstream falls back to `[account]`).
        if participant_ids.is_empty() {
            participant_ids.push(last.account_id);
        }
        let unread = match states.get(&root) {
            Some(state) => state.computed_unread(last.id, last.account_id, viewer),
            // Never touched: unread when any message is foreign (upstream
            // keeps `unread` until the viewer opens the thread; the
            // viewer's own replies do not clear it).
            None => members.iter().any(|member| member.account_id != viewer),
        };
        out.push(ConversationView {
            root_id: root,
            last_status: last,
            participant_ids,
            unread,
        });
    }
    out.sort_by_key(|view| view.last_status.id);
    out.reverse();
    Ok(out)
}

/// `GET /api/v1/conversations`: the viewer's threads, newest first,
/// paginated over `last_status_id` (upstream `paginate_by_max_id` /
/// `paginate_by_min_id`).
pub fn list_conversations(
    store: &dyn Store,
    viewer: Id,
    query: ConversationQuery,
    now_ms: u64,
) -> Result<Vec<ConversationView>> {
    let mut views = assemble(store, viewer, candidates(store, viewer)?, now_ms)?;
    if let Some(max_id) = query.max_id {
        views.retain(|view| view.last_status.id < max_id);
    }
    if let Some(since_id) = query.since_id {
        views.retain(|view| view.last_status.id > since_id);
    }
    if let Some(min_id) = query.min_id {
        views.retain(|view| view.last_status.id > min_id);
        views.sort_by_key(|view| view.last_status.id);
    }
    views.truncate(query.limit);
    Ok(views)
}

/// Load one thread for the viewer (`:id` is the thread root).
/// `NotFound("conversation")` when the thread has no fetchable status for the
/// viewer or the viewer deleted it.
pub fn get_conversation(
    store: &dyn Store,
    viewer: Id,
    root: Id,
    now_ms: u64,
) -> Result<ConversationView> {
    // The root may be an older status outside the candidate windows, so load
    // the thread around it: the root plus its reply subtree (bounded).
    let mut members = Vec::new();
    if let Some(status) = store.status_by_id(root)? {
        if status.visibility == Visibility::Direct {
            members.push(status);
        }
    }
    // Replies to the root (one level, cursor-bounded) plus any candidate
    // that resolves to this root.
    let mut queue = vec![root];
    let mut seen: HashSet<Id> = HashSet::from([root]);
    while let Some(parent) = queue.pop() {
        let children = store.status_children(parent, None, THREAD_WALK_CAP)?;
        for child in children {
            if child.visibility == Visibility::Direct && seen.insert(child.id) {
                if child.in_reply_to_id == Some(parent) {
                    queue.push(child.id);
                }
                members.push(child);
            }
            if members.len() >= THREAD_WALK_CAP {
                break;
            }
        }
        if members.len() >= THREAD_WALK_CAP {
            break;
        }
    }
    for status in candidates(store, viewer)? {
        if !seen.contains(&status.id) {
            members.push(status);
        }
    }
    let roots = thread_roots(store, &members)?;
    let members: Vec<Status> = members
        .into_iter()
        .filter(|status| roots.get(&status.id).copied().unwrap_or(status.id) == root)
        .collect();
    let views = assemble(store, viewer, members, now_ms)?;
    views
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotFound("conversation".into()))
}

/// `POST /api/v1/conversations/:id/read`: clear the unread flag; idempotent.
pub fn mark_read(
    store: &dyn Store,
    viewer: Id,
    root: Id,
    now_ms: u64,
) -> Result<ConversationView> {
    let view = get_conversation(store, viewer, root, now_ms)?;
    let mut state = store
        .conversation_state(viewer, root)?
        .unwrap_or_else(|| ConversationState::fresh(viewer, root, view.last_status.id, now_ms));
    state.last_status_id = view.last_status.id;
    state.seen_status_id = Some(view.last_status.id);
    state.unread_override = None;
    state.unread = false;
    state.deleted = false;
    state.updated_at_ms = now_ms;
    store.put_conversation_state(state)?;
    let mut view = view;
    view.unread = false;
    Ok(view)
}

/// `POST /api/v1/conversations/:id/unread`: force the unread flag; idempotent.
pub fn mark_unread(
    store: &dyn Store,
    viewer: Id,
    root: Id,
    now_ms: u64,
) -> Result<ConversationView> {
    let view = get_conversation(store, viewer, root, now_ms)?;
    let mut state = store
        .conversation_state(viewer, root)?
        .unwrap_or_else(|| ConversationState::fresh(viewer, root, view.last_status.id, now_ms));
    state.last_status_id = view.last_status.id;
    state.unread_override = Some(true);
    state.unread = true;
    state.deleted = false;
    state.updated_at_ms = now_ms;
    store.put_conversation_state(state)?;
    let mut view = view;
    view.unread = true;
    Ok(view)
}

/// `DELETE /api/v1/conversations/:id`: remove the viewer's participation
/// state (read position and all); the statuses are untouched. A second delete
/// 404s (the thread no longer exists for the viewer; upstream `destroy!` on a
/// missing row raises `RecordNotFound`).
pub fn delete_conversation(store: &dyn Store, viewer: Id, root: Id, now_ms: u64) -> Result<()> {
    // The thread must exist for the viewer (or the delete is a 404).
    let _ = get_conversation(store, viewer, root, now_ms)?;
    let mut state = store
        .conversation_state(viewer, root)?
        .unwrap_or_else(|| ConversationState::fresh(viewer, root, root, now_ms));
    state.deleted = true;
    state.updated_at_ms = now_ms;
    store.put_conversation_state(state)?;
    Ok(())
}

/// Resolve the status behind a status-mute request under the shared show
/// gate: missing, deleted or otherwise invisible is `NotFound("status")`.
fn fetch_visible(store: &dyn Store, viewer: Id, status_id: Id, now_ms: u64) -> Result<Status> {
    let status = store
        .status_by_id(status_id)?
        .filter(|status| !status.is_deleted())
        .ok_or_else(|| Error::NotFound("status".into()))?;
    let pair = store.pair_state(status.account_id, viewer, now_ms)?;
    if !policy::can_fetch(Some(viewer), &status, pair) {
        return Err(Error::NotFound("status".into()));
    }
    Ok(status)
}

/// `POST /api/v1/statuses/:id/mute`: mute the status's conversation
/// (idempotent). Returns the status for the DTO.
///
/// Upstream (`statuses/mutes_controller.rb#set_conversation`) raises
/// `Mastodon::ValidationError` (422) when the status has no conversation:
/// only direct-visibility statuses belong to one. Missing, deleted or
/// otherwise invisible statuses stay 404 via the show gate above.
pub fn mute_status_conversation(
    store: &dyn Store,
    viewer: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let status = fetch_visible(store, viewer, status_id, now_ms)?;
    require_conversation(&status)?;
    let root = thread_root(store, &status)?;
    store.mute_conversation(viewer, root, now_ms)?;
    Ok(status)
}

/// `POST /api/v1/statuses/:id/unmute`: unmute (no-op when not muted).
pub fn unmute_status_conversation(
    store: &dyn Store,
    viewer: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let status = fetch_visible(store, viewer, status_id, now_ms)?;
    require_conversation(&status)?;
    let root = thread_root(store, &status)?;
    store.unmute_conversation(viewer, root)?;
    Ok(status)
}

/// Only direct-visibility statuses have a conversation; anything else is
/// upstream's 422 `ValidationError` (this server's `InvalidInput` maps to
/// 422 the same way).
fn require_conversation(status: &Status) -> Result<()> {
    if status.visibility != Visibility::Direct {
        return Err(Error::InvalidInput(
            "Validation failed: Status does not have an associated conversation".into(),
        ));
    }
    Ok(())
}

/// Whether the viewer mutes the conversation a status belongs to (feeds the
/// status DTO `muted` flag).
pub fn status_muted(store: &dyn Store, viewer: Id, status: &Status) -> Result<bool> {
    let root = thread_root(store, status)?;
    store.conversation_muted(viewer, root)
}

/// Mute flags for a batch of statuses, keyed by status id (one transaction
/// per level of the root walk plus one for the flags).
pub fn muted_map_for_statuses(
    store: &dyn Store,
    viewer: Id,
    statuses: &[Status],
) -> Result<HashMap<Id, bool>> {
    let roots = thread_roots(store, statuses)?;
    let unique: Vec<Id> = roots.values().copied().collect::<HashSet<_>>().into_iter().collect();
    let muted = store.conversation_muted_map(viewer, &unique)?;
    let mut out = HashMap::new();
    for status in statuses {
        let root = roots.get(&status.id).copied().unwrap_or(status.id);
        out.insert(status.id, muted.get(&root).copied().unwrap_or(false));
    }
    Ok(out)
}

/// Recipients of a `conversation` streaming event for a new direct status:
/// the author plus every mentioned account (upstream fans out per
/// participant through `AccountConversation.add_status`).
pub fn conversation_event_recipients(status: &Status) -> Vec<Id> {
    if status.visibility != Visibility::Direct || status.is_deleted() {
        return Vec::new();
    }
    let mut recipients: HashSet<Id> =
        status.mentions.iter().copied().collect();
    recipients.insert(status.account_id);
    let mut out: Vec<Id> = recipients.into_iter().collect();
    out.sort();
    out
}

/// Wire rendering lives in the adapter
/// (`endpoints::conversations::conversation_json`, shared by the REST index
/// and the streaming fan-out).

/// Accounts referenced by a push `data[alerts]` map that this server can
/// produce (unknown alert keys are dropped, as upstream's `expect` permits
/// only `Notification::TYPES`).
pub fn known_alert_types() -> &'static [&'static str] {
    &[
        "mention",
        "follow",
        "reblog",
        "favourite",
        "poll",
        "follow_request",
        "quote",
    ]
}

/// Whether a notification may push to `subscription` (upstream
/// `Web::PushSubscription#pushable?`: policy gate plus the per-type alert
/// switch; an unset alert defaults to off).
pub fn subscription_pushable(
    subscription: &WebPushSubscription,
    notification_type: &str,
    follower: bool,
    followed_by: bool,
) -> bool {
    let policy_ok = match crate::domain::PushPolicy::parse(&subscription.policy) {
        crate::domain::PushPolicy::All => true,
        crate::domain::PushPolicy::None => false,
        crate::domain::PushPolicy::Followed => followed_by,
        crate::domain::PushPolicy::Follower => follower,
        crate::domain::PushPolicy::Unknown => false,
    };
    policy_ok && subscription.alerts.get(notification_type).copied().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipients_cover_author_and_mentions() {
        let status = Status {
            id: Id::new(10),
            account_id: Id::new(1),
            uri: "https://example.social/users/a/statuses/10".into(),
            url: "https://example.social/@a/10".into(),
            source_text: "hi".into(),
            content_html: "<p>hi</p>".into(),
            created_at_ms: 0,
            edited_at_ms: None,
            deleted_at_ms: None,
            language: None,
            visibility: Visibility::Direct,
            sensitive: false,
            spoiler_text: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog_of_id: None,
            media_ids: Vec::new(),
            place_id: None,
            poll_id: None,
            quote_policy: crate::domain::QuotePolicy::Nobody,
            mentions: vec![Id::new(2), Id::new(3)],
            silent_mentions: Vec::new(),
            tags: Vec::new(),
            application: None,
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            quotes_count: 0,
        };
        assert_eq!(
            conversation_event_recipients(&status),
            vec![Id::new(1), Id::new(2), Id::new(3)]
        );
    }

    #[test]
    fn non_direct_status_has_no_recipients() {
        let mut status = Status {
            id: Id::new(10),
            account_id: Id::new(1),
            uri: String::new(),
            url: String::new(),
            source_text: String::new(),
            content_html: String::new(),
            created_at_ms: 0,
            edited_at_ms: None,
            deleted_at_ms: None,
            language: None,
            visibility: Visibility::Public,
            sensitive: false,
            spoiler_text: None,
            in_reply_to_id: None,
            in_reply_to_account_id: None,
            reblog_of_id: None,
            media_ids: Vec::new(),
            place_id: None,
            poll_id: None,
            quote_policy: crate::domain::QuotePolicy::Public,
            mentions: vec![Id::new(2)],
            silent_mentions: Vec::new(),
            tags: Vec::new(),
            application: None,
            replies_count: 0,
            reblogs_count: 0,
            favourites_count: 0,
            quotes_count: 0,
        };
        assert!(conversation_event_recipients(&status).is_empty());
        status.visibility = Visibility::Direct;
        status.deleted_at_ms = Some(1);
        assert!(conversation_event_recipients(&status).is_empty());
    }
}

//! Thread context and edit-history reads.
//!
//! Authenticated callers get the near-unlimited Mastodon context (4096
//! ancestors, 4096 descendants, unbounded depth); anonymous callers get 40
//! ancestors and 60 descendants within depth 20. The caps bound the ids
//! *loaded* (Mastodon applies its SQL `LIMIT` before permission filtering),
//! so an invisible ancestor/descendant is skipped while the walk continues.
//! Descendants are collected breadth-first; each parent's children are read
//! newest-first, at most 200 per level.

use std::collections::HashSet;
use std::collections::VecDeque;

use super::update::fetchable;
use super::update::show_status;
use crate::application::ports::Store;
use crate::domain::Id;
use crate::domain::Result;
use crate::domain::Status;
use crate::domain::StatusVersion;

/// Per-parent children fetch while walking a thread's descendants.
const CHILDREN_PER_LEVEL: usize = 200;

/// `GET /api/v1/statuses/:id/context`: the status's thread around the root.
#[derive(Debug, Clone)]
pub struct ContextTree {
    /// Reply chain up to the root, oldest first.
    pub ancestors: Vec<Status>,
    /// Replies below the root, breadth-first (parent's children ahead of
    /// deeper levels, mirroring Mastodon's thread order).
    pub descendants: Vec<Status>,
}

/// `GET /api/v1/statuses/:id/context` — ancestors and descendants of a
/// status under the show gate (`NotFound("status")` when the root is
/// invisible).
///
/// Authenticated callers get the near-unlimited Mastodon context (4096
/// ancestors, 4096 descendants, unbounded depth); anonymous callers get 40
/// ancestors and 60 descendants within depth 20. The caps bound the ids
/// *loaded* (Mastodon applies its SQL `LIMIT` before permission filtering),
/// so an invisible ancestor/descendant is skipped while the walk continues.
/// Descendants are collected breadth-first; each parent's children are read
/// newest-first, at most 200 per level.
pub fn status_context(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
    authenticated: bool,
) -> Result<ContextTree> {
    let root = show_status(store, viewer, status_id, now_ms)?;
    let (ancestor_cap, descendant_cap, depth_limit) = if authenticated {
        (4_096usize, 4_096usize, None)
    } else {
        (40usize, 60usize, Some(20usize))
    };

    let ancestors = collect_ancestors(store, viewer, &root, now_ms, ancestor_cap)?;
    let descendants =
        collect_descendants(store, viewer, &root, now_ms, descendant_cap, depth_limit)?;
    Ok(ContextTree {
        ancestors,
        descendants,
    })
}

/// Walk `in_reply_to_id` upward, oldest-last, then reverse to oldest-first.
fn collect_ancestors(
    store: &dyn Store,
    viewer: Option<Id>,
    root: &Status,
    now_ms: u64,
    cap: usize,
) -> Result<Vec<Status>> {
    let mut visited: HashSet<Id> = HashSet::new();
    visited.insert(root.id);
    let mut chain: Vec<Status> = Vec::new();
    let mut cursor = root.in_reply_to_id;
    let mut walked = 0usize;

    while let Some(parent_id) = cursor {
        if walked >= cap {
            break;
        }
        walked += 1;
        let Some(parent) = store.status_by_id(parent_id)? else {
            break;
        };
        // Cycle guard on corrupt reply chains.
        if !visited.insert(parent.id) {
            break;
        }
        cursor = parent.in_reply_to_id;
        // Invisible ancestors are excluded, but the chain keeps walking.
        if fetchable(store, viewer, &parent, now_ms)? {
            chain.push(parent);
        }
    }
    chain.reverse();
    Ok(chain)
}

/// Breadth-first descent of the reply tree; stops at the total cap or the
/// depth limit (unlimited for authenticated callers).
fn collect_descendants(
    store: &dyn Store,
    viewer: Option<Id>,
    root: &Status,
    now_ms: u64,
    cap: usize,
    depth_limit: Option<usize>,
) -> Result<Vec<Status>> {
    let mut visited: HashSet<Id> = HashSet::new();
    visited.insert(root.id);
    let mut queue: VecDeque<(Id, usize)> = VecDeque::new();
    queue.push_back((root.id, 0));
    let mut descendants: Vec<Status> = Vec::new();
    let mut loaded = 0usize;

    while let Some((parent_id, depth)) = queue.pop_front() {
        if descendants.len() >= cap || loaded >= cap {
            break;
        }
        if depth_limit.is_some_and(|max| depth >= max) {
            continue;
        }
        for child in store.status_children(parent_id, None, CHILDREN_PER_LEVEL)? {
            if !visited.insert(child.id) {
                continue;
            }
            loaded += 1;
            queue.push_back((child.id, depth + 1));
            // Invisible/deleted children are skipped; their replies below
            // them are still walked.
            if fetchable(store, viewer, &child, now_ms)? {
                descendants.push(child);
                if descendants.len() >= cap {
                    break;
                }
            }
            if loaded >= cap {
                break;
            }
        }
    }
    descendants.sort_by_key(|status| {
        // Upstream `Status::ThreadingConcern#find_statuses_from_tree_path`
        // (`app/models/concerns/status/threading_concern.rb`, `promote: true`
        // at pin `8ff11c2b`): self-replies (the author answering themselves)
        // sort ahead of other replies. `sort_by_key` is stable, like
        // upstream's `promote_by!`, so relative order within each group is
        // preserved. `false < true`, so self-replies (key `false`) come first.
        status.in_reply_to_account_id != Some(status.account_id)
    });
    Ok(descendants)
}

/// `GET /api/v1/statuses/:id/history` — the status's edit snapshots, oldest
/// first, under the show gate.
///
/// A never-edited status has no stored snapshots, so a version-0 snapshot is
/// synthesized from the current record (timestamped by `edited_at_ms` when
/// present, otherwise `created_at_ms`).
pub fn status_history(
    store: &dyn Store,
    viewer: Option<Id>,
    status_id: Id,
    now_ms: u64,
) -> Result<Vec<StatusVersion>> {
    let status = show_status(store, viewer, status_id, now_ms)?;
    let mut versions = store.status_versions(status_id)?;
    if versions.is_empty() {
        let poll_options = store
            .poll_by_status(status_id)?
            .map(|poll| poll.options.into_iter().map(|o| o.title).collect());
        versions.push(StatusVersion {
            status_id,
            version: 0,
            account_id: status.account_id,
            content_html: status.content_html.clone(),
            source_text: status.source_text.clone(),
            spoiler_text: status.spoiler_text.clone(),
            sensitive: status.sensitive,
            media_ids: status.media_ids.clone(),
            poll_options,
            created_at_ms: status.edited_at_ms.unwrap_or(status.created_at_ms),
        });
    }
    Ok(versions)
}

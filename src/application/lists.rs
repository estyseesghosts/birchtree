//! List use cases.
//!
//! Mirrors Mastodon's `Api::V1::ListsController` + `Lists::AccountsController`
//! and `AddAccountsToListService`: title/limit validation with the exact
//! 422 messages, ownership 404s, and membership that requires an active
//! follow (or pending request) from the list owner.

use crate::application::ports::Store;
use crate::domain::{Error, Id, List, RepliesPolicy, Result};

/// Mastodon en.yml `activerecord.errors.models.list` messages.
const ERR_LIMIT: &str = "Validation failed: You have reached the maximum number of lists";
const ERR_TITLE_BLANK: &str = "Validation failed: Title can't be blank";
const ERR_TITLE_TOO_LONG: &str =
    "Validation failed: Title is too long (maximum is 256 characters)";
const ERR_POLICY: &str =
    "Validation failed: Replies policy is not included in the list";
const ERR_MEMBER_NOT_FOUND: &str = "Record not found";
const ERR_MEMBER_NOT_FOLLOWING: &str = "Validation failed: Account must be following";

/// `GET /api/v1/lists` — the viewer's lists (all of them; v4.7.3 does not
/// paginate this endpoint).
pub fn index(store: &dyn Store, viewer: Id) -> Result<Vec<List>> {
    store.lists_by_account(viewer)
}

/// `POST /api/v1/lists` — create a list. `replies_policy` and `exclusive` are
/// accepted at creation as upstream's `list_params` does (default `list` and
/// `false`).
pub fn create(
    store: &dyn Store,
    viewer: Id,
    title: &str,
    replies_policy: Option<String>,
    exclusive: Option<bool>,
    now_ms: u64,
) -> Result<List> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::InvalidInput(ERR_TITLE_BLANK.into()));
    }
    if title.chars().count() > List::TITLE_LENGTH_LIMIT {
        return Err(Error::InvalidInput(ERR_TITLE_TOO_LONG.into()));
    }
    let policy = parse_policy(replies_policy)?.unwrap_or_default();
    let existing = store.lists_by_account(viewer)?;
    if existing.len() >= List::PER_ACCOUNT_LIMIT {
        return Err(Error::InvalidInput(ERR_LIMIT.into()));
    }
    store.create_list(viewer, title, policy, exclusive.unwrap_or(false), now_ms)
}

/// The wire `replies_policy`, validated; `None` when absent.
fn parse_policy(raw: Option<String>) -> Result<Option<RepliesPolicy>> {
    match raw {
        None => Ok(None),
        Some(raw) => RepliesPolicy::from_wire(&raw)
            .map(Some)
            .ok_or_else(|| Error::InvalidInput(ERR_POLICY.into())),
    }
}

/// `GET /api/v1/lists/:id` — the viewer's list, or a Mastodon 404.
pub fn show(store: &dyn Store, viewer: Id, id: Id) -> Result<List> {
    owned_list(store, viewer, id)
}

/// `PUT/PATCH /api/v1/lists/:id` — update title/replies_policy/exclusive.
pub fn update(
    store: &dyn Store,
    viewer: Id,
    id: Id,
    title: Option<String>,
    replies_policy: Option<String>,
    exclusive: Option<bool>,
    now_ms: u64,
) -> Result<List> {
    // Ownership is checked first (Mastodon's `set_list` 404s before
    // validation).
    owned_list(store, viewer, id)?;
    let policy = parse_policy(replies_policy)?;
    let title = title.map(|title| {
        let trimmed = title.trim().to_string();
        if trimmed.is_empty() {
            return Err(Error::InvalidInput(ERR_TITLE_BLANK.into()));
        }
        if trimmed.chars().count() > List::TITLE_LENGTH_LIMIT {
            return Err(Error::InvalidInput(ERR_TITLE_TOO_LONG.into()));
        }
        Ok(trimmed)
    })
    .transpose()?;
    store
        .update_list(viewer, id, title, policy, exclusive, now_ms)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))
}

/// `DELETE /api/v1/lists/:id` — delete the viewer's list.
pub fn destroy(store: &dyn Store, viewer: Id, id: Id) -> Result<()> {
    owned_list(store, viewer, id)?;
    store.delete_list(viewer, id)?;
    Ok(())
}

/// `GET /api/v1/lists/:id/accounts` — list members, newest first.
pub fn members(
    store: &dyn Store,
    viewer: Id,
    id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Id>> {
    owned_list(store, viewer, id)?;
    store.list_members(id, max_id, limit)
}

/// `POST /api/v1/lists/:id/accounts` — add members (`account_ids[]`).
///
/// Every id must exist (404) and be followed (or pending) by the viewer
/// (422); adding an existing member is a no-op.
pub fn add_members(store: &dyn Store, viewer: Id, id: Id, account_ids: &[Id]) -> Result<()> {
    owned_list(store, viewer, id)?;
    for account_id in account_ids {
        if store.account_by_id(*account_id)?.is_none() {
            return Err(Error::NotFound(ERR_MEMBER_NOT_FOUND.into()));
        }
        let follow = store.follow_of(viewer, *account_id)?;
        let permitted = follow.as_ref().is_some_and(|f| {
            matches!(
                f.state,
                crate::domain::FollowState::Active | crate::domain::FollowState::Pending
            )
        });
        if !permitted {
            return Err(Error::InvalidInput(ERR_MEMBER_NOT_FOLLOWING.into()));
        }
    }
    store.add_list_members(id, account_ids)?;
    Ok(())
}

/// `DELETE /api/v1/lists/:id/accounts` — remove members (idempotent).
pub fn remove_members(store: &dyn Store, viewer: Id, id: Id, account_ids: &[Id]) -> Result<()> {
    owned_list(store, viewer, id)?;
    store.remove_list_members(id, account_ids)?;
    Ok(())
}

/// Resolve a list the viewer owns (Mastodon 404 body for anything else).
fn owned_list(store: &dyn Store, viewer: Id, id: Id) -> Result<List> {
    let list = store
        .list_by_id(id)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))?;
    if list.account_id != viewer {
        return Err(Error::NotFound("Record not found".into()));
    }
    Ok(list)
}

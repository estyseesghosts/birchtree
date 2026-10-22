use std::collections::HashSet;

use crate::domain::{Id, List, RepliesPolicy, Result};

pub trait ListStore: Send + Sync {
    /// Create a list for `account_id` (the use case enforces the per-account
    /// limit and title rules).
    fn create_list(
        &self,
        account_id: Id,
        title: &str,
        replies_policy: RepliesPolicy,
        exclusive: bool,
        now_ms: u64,
    ) -> Result<List>;
    fn list_by_id(&self, id: Id) -> Result<Option<List>>;
    /// The owner's lists (index returns all; no pagination in v4.7.3).
    fn lists_by_account(&self, account_id: Id) -> Result<Vec<List>>;
    /// Patch a list the viewer owns; `None` when missing/foreign (404).
    fn update_list(
        &self,
        account_id: Id,
        id: Id,
        title: Option<String>,
        replies_policy: Option<RepliesPolicy>,
        exclusive: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<List>>;
    /// Delete a list the viewer owns (and its memberships); `None` when
    /// missing/foreign.
    fn delete_list(&self, account_id: Id, id: Id) -> Result<bool>;

    /// Add members (must follow, or have a pending request — enforced by the
    /// use case). Idempotent: existing members are skipped; the returned ids
    /// are the newly added ones.
    fn add_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>>;
    /// Remove members (idempotent); returns the removed ids.
    fn remove_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>>;
    /// Members of a list, newest first (cursor = member account id).
    fn list_members(&self, list_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;
    /// All members of a list (bounded), for the list timeline merge.
    fn all_list_members(&self, list_id: Id, cap: usize) -> Result<Vec<Id>>;
    /// The `candidates` that belong to one of `owner`'s lists with
    /// `exclusive = true` (upstream's `exclusive_list_users`, which hides them
    /// from home). One read transaction; no scan cap, because the cost is the
    /// owner's exclusive lists times the candidates.
    fn exclusive_list_members(&self, owner: Id, candidates: &[Id]) -> Result<HashSet<Id>>;
}

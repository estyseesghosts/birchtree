use crate::domain::{Id, Result};

// ---------------------------------------------------------------------------
// Collections (gaps batch G-C; port moved here at the parent merge so the
// REST adapters can join it to `Store`).
// ---------------------------------------------------------------------------

/// A curated collection of statuses owned by one account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CollectionRecord {
    pub id: Id,
    pub account_id: Id,
    pub title: String,
    pub description: String,
    pub public: bool,
    #[serde(default)]
    pub revoked: bool,
    /// Denormalized membership count. Stays rendered even when the owner's
    /// `hide_collections` hides the members (D30/D52: members hidden, counts
    /// stay).
    pub items_count: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// What changed in a collection (fan-out job payload + handler dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionChange {
    Add,
    Remove,
    Revoke,
    Delete,
}

impl CollectionChange {
    pub const fn as_str(self) -> &'static str {
        match self {
            CollectionChange::Add => "add",
            CollectionChange::Remove => "remove",
            CollectionChange::Revoke => "revoke",
            CollectionChange::Delete => "delete",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "add" => CollectionChange::Add,
            "remove" => CollectionChange::Remove,
            "revoke" => CollectionChange::Revoke,
            "delete" => CollectionChange::Delete,
            _ => return None,
        })
    }
}

/// Narrow port over the `COLLECTIONS` / `COLLECTION_ITEMS` tables.
///
/// `account_id` arguments are the claimed owner: trait methods return
/// `NotFound("Record not found")` for missing *or* foreign collections (never
/// an ownership oracle). Use-case wrappers add validation and policy.
pub trait CollectionStore: Send + Sync {
    fn create_collection(
        &self,
        account_id: Id,
        title: &str,
        description: &str,
        public: bool,
        now_ms: u64,
    ) -> Result<CollectionRecord>;

    fn collection_by_id(&self, id: Id) -> Result<Option<CollectionRecord>>;

    /// The owner's collections, newest first.
    fn collections_by_account(&self, account_id: Id) -> Result<Vec<CollectionRecord>>;

    /// Patch an owned collection; `None` when missing/foreign.
    fn update_collection(
        &self,
        account_id: Id,
        id: Id,
        title: Option<String>,
        description: Option<String>,
        public: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<CollectionRecord>>;

    /// Delete an owned collection and all its memberships; `false` when
    /// missing/foreign. Enqueues one `CollectionFanout` (`delete`) in the
    /// same transaction.
    fn delete_collection(&self, account_id: Id, id: Id, now_ms: u64) -> Result<bool>;

    /// Member status ids, newest first; cursors exclusive (`id < max_id`),
    /// bounded scan.
    fn collection_items(
        &self,
        collection_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Id>>;

    /// The owner's collections containing `status_id`, newest first.
    fn collections_containing_status(
        &self,
        account_id: Id,
        status_id: Id,
    ) -> Result<Vec<CollectionRecord>>;

    /// Add members (idempotent; existing members skipped). Returns the newly
    /// added ids. Enqueues one `CollectionFanout` (`add`, batch ≤ 500) in
    /// the same transaction. `NotFound` when missing/foreign; `InvalidInput`
    /// when revoked, over the batch bound, or over the item cap.
    fn add_collection_items(
        &self,
        account_id: Id,
        collection_id: Id,
        status_ids: &[Id],
        now_ms: u64,
    ) -> Result<Vec<Id>>;

    /// Remove members (idempotent). Returns the removed ids. Enqueues one
    /// `CollectionFanout` (`remove`) in the same transaction when anything
    /// was removed.
    fn remove_collection_items(
        &self,
        account_id: Id,
        collection_id: Id,
        status_ids: &[Id],
        now_ms: u64,
    ) -> Result<Vec<Id>>;

    /// Revoke an owned collection: every membership is dropped,
    /// `revoked = true`, and one `CollectionFanout` (`revoke`) is enqueued
    /// in the same transaction. `None` when missing/foreign.
    fn revoke_collection(
        &self,
        account_id: Id,
        id: Id,
        now_ms: u64,
    ) -> Result<Option<CollectionRecord>>;

    /// `(account_id, reblog_id)` pairs for `status_id`, newest first, scoped
    /// to distributable reblogs: the reblog wrapper must exist, must not be
    /// a tombstone, and its visibility must be `distributable` (public or
    /// unlisted — `Visibility::is_distributable`). This is the D52 scope the
    /// plain `reblogged_by` scan lacks.
    fn reblogged_by_distributable(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>>;
}

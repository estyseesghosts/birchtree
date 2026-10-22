use std::collections::HashMap;

use crate::domain::bookmark::Bookmark;
use crate::domain::favourite::Favourite;
use crate::domain::{
    Id, QuotePolicy, Result, Status, StatusApplication, StatusVersion, Visibility,
};

/// A poll to embed in a created or edited status.
#[derive(Debug, Clone)]
pub struct NewPollSpec {
    pub multiple: bool,
    pub hide_totals: bool,
    /// Absolute expiry; local polls always have one.
    pub expires_at_ms: u64,
    /// 2–4 option titles, validated by the use case.
    pub options: Vec<String>,
}

/// A quote to attach at status creation (local quoted statuses in Phase 3).
#[derive(Debug, Clone)]
pub struct NewQuoteSpec {
    pub quoted_status_id: Id,
    /// Author of the quoted status (for the silent mention + counter update).
    pub quoted_account_id: Id,
    /// The quoting status's `quote_approval_policy`.
    pub approval_policy: QuotePolicy,
}

/// Input for creating a status in one atomic transaction.
/// The URI and URL of a remote status, as its server published them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatusIdentity {
    pub uri: String,
    pub url: String,
}

#[derive(Debug, Clone, Default)]
pub struct NewStatus {
    /// Set for a status received from a remote server: its own URI and URL are
    /// stored as given, and no local URI is minted. `None` for a local status.
    pub remote: Option<RemoteStatusIdentity>,
    pub account_id: Id,
    /// Canonical ActivityPub object URI base; the store appends `/{id}`
    /// (computed by the use case from the public URL + username).
    pub uri_base: String,
    /// Local web URL base; the store appends `/{id}`.
    pub url_base: String,
    pub source_text: String,
    pub content_html: String,
    pub spoiler_text: Option<String>,
    pub sensitive: bool,
    pub visibility: Visibility,
    pub language: Option<String>,
    pub in_reply_to_id: Option<Id>,
    pub in_reply_to_account_id: Option<Id>,
    /// Attached media ids, in the author's order.
    pub media_ids: Vec<Id>,
    /// Core extension CORE-P: attached place id (`None` = no place).
    pub place_id: Option<Id>,
    /// Resolved mention account ids, in mention order (serialized).
    pub mentions: Vec<Id>,
    /// Resolved silent mentions (quote access grants): visibility only.
    pub silent_mentions: Vec<Id>,
    /// Normalized hashtags without the `#` prefix (D25).
    pub tags: Vec<String>,
    pub poll: Option<NewPollSpec>,
    pub quote: Option<NewQuoteSpec>,
    pub application: Option<StatusApplication>,
    /// Mastodon `Idempotency-Key` header value, when present.
    pub idempotency_key: Option<String>,
}

/// Result of a status creation; a replayed idempotent retry returns the
/// originally created status with `replayed = true`.
#[derive(Debug, Clone)]
pub struct CreatedStatus {
    pub status: Status,
    pub replayed: bool,
}

/// A deletion's pre-deletion snapshots (see [`StatusStore::delete_status`]).
#[derive(Debug, Clone)]
pub struct DeletedStatus {
    pub status: Status,
    /// Reblog wrappers tombstoned by the same transaction.
    pub reblogs: Vec<Status>,
}

/// Field-level edit patch (`PUT /api/v1/statuses/:id`). `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct StatusUpdate {
    pub source_text: Option<String>,
    pub content_html: Option<String>,
    /// `Some(None)` clears a spoiler; `None` leaves it unchanged.
    pub spoiler_text: Option<Option<String>>,
    pub sensitive: Option<bool>,
    pub language: Option<String>,
    pub media_ids: Option<Vec<Id>>,
    /// Core extension CORE-P: replace the attached place (`None` leaves it
    /// unchanged; `Some(None)` clears; `Some(Some(id))` sets).
    pub place_id: Option<Option<Id>>,
    /// Replace poll options/multiple (resets all votes, per Mastodon).
    pub poll: Option<NewPollSpec>,
    /// Extend poll expiry without resetting votes.
    pub poll_expires_in_ms: Option<u64>,
    pub quote_approval_policy: Option<QuotePolicy>,
    /// Replace the stored hashtags (already normalized by the caller) and
    /// move the hashtag-index keys in the same transaction. `None` leaves
    /// tags and index untouched.
    pub tags: Option<Vec<String>>,
}

/// Per-viewer status flags for DTO packing.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusViewFlags {
    pub favourited: bool,
    pub bookmarked: bool,
    /// The viewer has reblogged the (proper) status.
    pub reblogged: bool,
    /// The viewer has pinned the status.
    pub pinned: bool,
    /// The viewer mutes the status's conversation (thread root).
    pub muted: bool,
}

/// Cursor-bounded query for an account's statuses.
#[derive(Debug, Clone, Copy)]
pub struct AccountStatusesQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
    /// Return the account's pinned statuses in pin order instead.
    pub pinned_only: bool,
    pub exclude_replies: bool,
    pub exclude_reblogs: bool,
}

/// Statuses, replies, reblogs, edits, pins and status interactions.
pub trait StatusStore: Send + Sync {
    /// Create a status with its poll/quote, indexes, counters, idempotency
    /// record and follow-up jobs in ONE transaction.
    fn create_status(&self, input: NewStatus, now_ms: u64) -> Result<CreatedStatus>;

    fn status_by_id(&self, id: Id) -> Result<Option<Status>>;

    /// Non-deleted statuses among `ids`, ordered by id descending.
    fn statuses_by_ids(&self, ids: &[Id]) -> Result<Vec<Status>>;

    /// Apply an edit: snapshot the pre-edit version (once), patch fields, bump
    /// `edited_at_ms`, keep counters — ONE transaction. Returns `Ok(None)`
    /// when the patch changed nothing (no snapshot, no `edited_at` bump).
    fn update_status(
        &self,
        id: Id,
        account_id: Id,
        update: StatusUpdate,
        now_ms: u64,
    ) -> Result<Option<Status>>;

    /// Tombstone a status in ONE transaction (tombstone + dependent reblogs +
    /// pin removal + parent/author counter fixes + follow-up job). Returns the
    /// pre-deletion snapshots: the status and every reblog wrapper the cascade
    /// tombstoned (streaming needs them to send `delete` events). `NotFound`
    /// when missing, deleted, or not owned.
    fn delete_status(&self, id: Id, account_id: Id, now_ms: u64) -> Result<DeletedStatus>;

    /// The live reblog wrapper `account_id` created of `status_id`, if any.
    fn own_reblog(&self, account_id: Id, status_id: Id) -> Result<Option<Status>>;

    /// Direct reply children of `parent_id`, newest first, cursor-bounded.
    fn status_children(
        &self,
        parent_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Status>>;

    /// Edit-history snapshots, oldest first (empty when never edited).
    fn status_versions(&self, status_id: Id) -> Result<Vec<StatusVersion>>;

    /// Statuses authored by `account_id` (non-deleted), id-descending with
    /// cursors; `pinned_only` returns the account's pins in pin order.
    fn statuses_by_account(
        &self,
        account_id: Id,
        query: AccountStatusesQuery,
    ) -> Result<Vec<Status>>;

    /// Viewer flags for a batch of statuses (no N+1).
    fn viewer_status_flags(
        &self,
        viewer_id: Id,
        status_ids: &[Id],
    ) -> Result<HashMap<Id, StatusViewFlags>>;

    // -- Interactions (each ONE transaction: row + counters + follow-up job) --

    /// Add a favourite (idempotent: already-favourited returns the status
    /// unchanged). NotFound when the status is missing or not visible.
    fn favourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    /// Remove a favourite (no-op when absent), returning the status.
    fn unfavourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    fn bookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    fn unbookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Create (or return the existing) reblog by `account_id` of `status_id`.
    fn reblog_status(
        &self,
        account_id: Id,
        status_id: Id,
        visibility: Visibility,
        now_ms: u64,
    ) -> Result<Status>;
    /// Delete the viewer's reblog of `status_id`, returning the original
    /// (counters decremented; no-op when the viewer never reblogged).
    fn unreblog_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Pin own status (limit enforced by the use case), returning the status.
    fn pin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    /// Unpin own status (no-op when not pinned), returning the status.
    fn unpin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Update only the quote approval policy — no edit snapshot and no
    /// `edited_at` bump (mirrors Mastodon's interaction-policy endpoint).
    /// NotFound when missing/deleted; the caller enforces ownership.
    fn set_quote_policy(&self, status_id: Id, policy: QuotePolicy) -> Result<Status>;

    /// Accounts that favourited `status_id`, newest first, as
    /// `(account_id, favourite_id)` pairs; the interaction id is the cursor.
    fn favourited_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>>;
    /// Accounts that reblogged `status_id`, newest first, as
    /// `(account_id, reblog_status_id)` pairs; the reblog id is the cursor.
    fn reblogged_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>>;

    /// The viewer's favourites, newest first (cursor = favourite row id).
    fn favourited_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Favourite>>;
    /// The viewer's bookmarks, newest first (cursor = bookmark row id).
    fn bookmarked_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Bookmark>>;

    /// One bounded page of the tag-normalization backfill (D25): newest
    /// first, `(status id, stored tags, source text)`. `after` is the
    /// exclusive resume cursor (`None` starts at the newest row).
    fn backfill_tag_batch(
        &self,
        after: Option<Id>,
        cap: usize,
    ) -> Result<Vec<(Id, Vec<String>, String)>>;
    /// Rewrite a status's stored tag list and move its hashtag-index keys in
    /// ONE transaction (D25 backfill writer).
    fn rewrite_status_tags(&self, status_id: Id, new_tags: &[String]) -> Result<()>;
}

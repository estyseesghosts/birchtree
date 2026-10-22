//! `StatusStore` for `RedbStore`: one-line delegations into `statuses_*`.
//!
//! Rust forbids splitting one trait impl across files (E0119), so the
//! per-area bodies live as `pub(super)` free fns in `statuses_{create,
//! update, delete, interactions, read}` and this module owns the SINGLE
//! `impl StatusStore for RedbStore`.

use std::collections::HashMap;

use crate::application::ports::AccountStatusesQuery;
use crate::application::ports::CreatedStatus;
use crate::application::ports::DeletedStatus;
use crate::application::ports::NewStatus;
use crate::application::ports::StatusStore;
use crate::application::ports::StatusUpdate;
use crate::application::ports::StatusViewFlags;
use crate::domain::Id;
use crate::domain::QuotePolicy;
use crate::domain::Result;
use crate::domain::Status;
use crate::domain::StatusVersion;
use crate::domain::Visibility;
use crate::domain::bookmark::Bookmark;
use crate::domain::favourite::Favourite;

use super::RedbStore;
use super::statuses_create;
use super::statuses_delete;
use super::statuses_interactions;
use super::statuses_read;
use super::statuses_update;

impl StatusStore for RedbStore {
    fn create_status(&self, input: NewStatus, now_ms: u64) -> Result<CreatedStatus> {
        statuses_create::create_status_impl(self, input, now_ms)
    }

    fn status_by_id(&self, id: Id) -> Result<Option<Status>> {
        statuses_read::status_by_id_impl(self, id)
    }

    fn statuses_by_ids(&self, ids: &[Id]) -> Result<Vec<Status>> {
        statuses_read::statuses_by_ids_impl(self, ids)
    }

    fn update_status(
        &self,
        id: Id,
        account_id: Id,
        update: StatusUpdate,
        now_ms: u64,
    ) -> Result<Option<Status>> {
        statuses_update::update_status_impl(self, id, account_id, update, now_ms)
    }

    fn delete_status(&self, id: Id, account_id: Id, now_ms: u64) -> Result<DeletedStatus> {
        statuses_delete::delete_status_impl(self, id, account_id, now_ms)
    }

    fn own_reblog(&self, account_id: Id, status_id: Id) -> Result<Option<Status>> {
        statuses_read::own_reblog_impl(self, account_id, status_id)
    }

    fn status_children(
        &self,
        parent_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Status>> {
        statuses_read::status_children_impl(self, parent_id, max_id, limit)
    }

    fn status_versions(&self, status_id: Id) -> Result<Vec<StatusVersion>> {
        statuses_read::status_versions_impl(self, status_id)
    }

    fn statuses_by_account(
        &self,
        account_id: Id,
        query: AccountStatusesQuery,
    ) -> Result<Vec<Status>> {
        statuses_read::statuses_by_account_impl(self, account_id, query)
    }

    fn viewer_status_flags(
        &self,
        viewer_id: Id,
        status_ids: &[Id],
    ) -> Result<HashMap<Id, StatusViewFlags>> {
        statuses_read::viewer_status_flags_impl(self, viewer_id, status_ids)
    }

    fn favourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        statuses_interactions::favourite_status_impl(self, account_id, status_id, now_ms)
    }

    fn unfavourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        statuses_interactions::unfavourite_status_impl(self, account_id, status_id, now_ms)
    }

    fn bookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        statuses_interactions::bookmark_status_impl(self, account_id, status_id, now_ms)
    }

    fn unbookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        statuses_interactions::unbookmark_status_impl(self, account_id, status_id, now_ms)
    }

    fn reblog_status(
        &self,
        account_id: Id,
        status_id: Id,
        visibility: Visibility,
        now_ms: u64,
    ) -> Result<Status> {
        statuses_interactions::reblog_status_impl(self, account_id, status_id, visibility, now_ms)
    }

    fn unreblog_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        statuses_interactions::unreblog_status_impl(self, account_id, status_id, now_ms)
    }

    fn pin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        statuses_interactions::pin_status_impl(self, account_id, status_id, now_ms)
    }

    fn unpin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status> {
        statuses_interactions::unpin_status_impl(self, account_id, status_id, now_ms)
    }

    fn set_quote_policy(&self, status_id: Id, policy: QuotePolicy) -> Result<Status> {
        statuses_interactions::set_quote_policy_impl(self, status_id, policy)
    }

    fn favourited_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>> {
        statuses_interactions::favourited_by_impl(self, status_id, max_id, limit)
    }

    fn reblogged_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>> {
        statuses_interactions::reblogged_by_impl(self, status_id, max_id, limit)
    }

    fn favourited_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Favourite>> {
        statuses_interactions::favourited_statuses_impl(
            self, account_id, max_id, since_id, min_id, limit,
        )
    }

    fn bookmarked_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Bookmark>> {
        statuses_interactions::bookmarked_statuses_impl(
            self, account_id, max_id, since_id, min_id, limit,
        )
    }

    fn backfill_tag_batch(
        &self,
        after: Option<Id>,
        cap: usize,
    ) -> Result<Vec<(Id, Vec<String>, String)>> {
        statuses_read::backfill_tag_batch_impl(self, after, cap)
    }

    fn rewrite_status_tags(&self, status_id: Id, new_tags: &[String]) -> Result<()> {
        statuses_update::rewrite_status_tags_impl(self, status_id, new_tags)
    }
}

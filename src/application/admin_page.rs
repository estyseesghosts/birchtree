//! Cursor pages for the admin account/report indexes (S0 shared params).
//!
//! `list_accounts`/`list_reports` (`application/admin.rs`) and the
//! `AdminStore::admin_*_page` methods (`application/ports.rs`) repeat the
//! `(filter, max_id, since_id, min_id, ascending, limit)` cluster.
//! [`AdminPage`] bundles the cursor half; [`AdminFilterPage`] pairs it with a
//! filter such as `AdminAccountFilter` or `AdminReportFilter`. The filter
//! type stays generic (no `ports` import) so this module keeps the
//! `domain <- application` direction with no import cycle.

use crate::domain::Id;

/// Cursor page over an admin index (id-descending unless `ascending`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminPage {
    /// Exclusive upper cursor (rows below this id).
    pub max_id: Option<Id>,
    /// Exclusive lower cursor for the refresh direction.
    pub since_id: Option<Id>,
    /// Exclusive lower cursor for the history direction.
    pub min_id: Option<Id>,
    /// Oldest-first order (admin v2 ascending pages).
    pub ascending: bool,
    /// Maximum rows to return (bounded by [`AdminPage::clamp`]).
    pub limit: usize,
}

impl AdminPage {
    /// Bundle the page arguments in field order.
    pub fn new(
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        ascending: bool,
        limit: usize,
    ) -> Self {
        Self {
            max_id,
            since_id,
            min_id,
            ascending,
            limit,
        }
    }

    /// Clamp `limit` into `(0, max_limit]`, falling back to `default_limit`
    /// for zero (same convention as `PageCursor::clamp`).
    pub fn clamp(&self, default_limit: usize, max_limit: usize) -> Self {
        let limit = if self.limit == 0 {
            default_limit
        } else {
            self.limit.min(max_limit)
        };
        Self { limit, ..*self }
    }
}

/// An admin index query: a row filter plus its cursor page.
///
/// `F` is the store filter type (e.g. `AdminAccountFilter` for
/// `admin_accounts_page`, `AdminReportFilter` for `admin_reports_page`).
#[derive(Debug, Clone, Copy)]
pub struct AdminFilterPage<F> {
    /// Row filter (filters are small plain structs, stored by value).
    pub filter: F,
    /// Cursor page.
    pub page: AdminPage,
}

impl<F> AdminFilterPage<F> {
    /// Pair a filter with its cursor page.
    pub fn new(filter: F, page: AdminPage) -> Self {
        Self { filter, page }
    }
}

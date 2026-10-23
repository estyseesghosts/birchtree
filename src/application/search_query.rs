//! Borrowed parameter object for `GET /api/v2/search` (S0 shared params).
//!
//! [`search`](crate::application::search::search) and `search_with_mode`
//! (`application/search.rs`) take the same ten-to-twelve scalar arguments
//! (`viewer`, `query`, `kind`, `offset`, `limit`, `following`, `account_id`,
//! `max_id`, `min_id`). This struct bundles them so later slices can shrink
//! those signatures without touching validation or ordering. Borrowed, not
//! owned: no allocation change at call sites.

use crate::domain::Id;

/// Parameters for `GET /api/v2/search` over local data.
///
/// Field order mirrors the argument list of
/// [`search`](crate::application::search::search): `kind` narrows to
/// `accounts`/`hashtags`/`statuses` (`None` = everything except statuses);
/// `offset` pages accounts/hashtags; `following` restricts accounts to
/// followed ones; `account_id`/`max_id`/`min_id` scope status search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchQuery<'a> {
    /// Authenticated viewer (`None` = anonymous; status search is empty).
    pub viewer: Option<Id>,
    /// Raw `q` value (trimmed by the use case).
    pub query: &'a str,
    /// Result type filter (`accounts`, `hashtags`, or `statuses`).
    pub kind: Option<&'a str>,
    /// Accounts/hashtags offset (statuses use id cursors instead).
    pub offset: usize,
    /// Maximum rows per section (bounded by `DEFAULT_LIMIT`/`MAX_LIMIT`).
    pub limit: usize,
    /// Only accounts the viewer follows.
    pub following: bool,
    /// Only statuses authored by this account.
    pub account_id: Option<Id>,
    /// Only statuses with ids below this (newest-first page cursor).
    pub max_id: Option<Id>,
    /// Only statuses with ids above this (refresh cursor).
    pub min_id: Option<Id>,
}

impl<'a> SearchQuery<'a> {
    /// Bundle the `search` arguments in field order.
    // WHY: constructor mirrors the search cluster field-for-field so call
    // sites convert mechanically; prefer the struct literal instead.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        viewer: Option<Id>,
        query: &'a str,
        kind: Option<&'a str>,
        offset: usize,
        limit: usize,
        following: bool,
        account_id: Option<Id>,
        max_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Self {
        Self {
            viewer,
            query,
            kind,
            offset,
            limit,
            following,
            account_id,
            max_id,
            min_id,
        }
    }
}

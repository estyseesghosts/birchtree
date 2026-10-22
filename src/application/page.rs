//! Shared cursor pagination for application use cases.
//!
//! Mastodon-style cursor pages walk an id-ordered feed with three optional
//! bounds: [`PageCursor::max_id`] pages backwards (older items),
//! [`PageCursor::since_id`] picks up items newer than a known id, and
//! [`PageCursor::min_id`] bounds a poll for new items. All comparisons on
//! [`Id`] are numeric; decimal ids must never be ordered lexicographically.
//!
//! Every bound is optional so a bare request (no query parameters) reads as
//! "the newest page". The requested page size travels in
//! [`PageCursor::limit`] and is bounded by [`PageCursor::clamp`] before the
//! store is touched, keeping scans bounded no matter what the client sends.

use crate::domain::Id;

/// Cursor selecting one page of an id-ordered feed.
///
/// Bounds are exclusive: `max_id` returns items strictly below it,
/// `since_id` and `min_id` return items strictly above them. A `None` bound
/// leaves that end of the range open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCursor {
    /// Return items with an id below this bound (the older page).
    pub max_id: Option<Id>,
    /// Return items with an id above this bound (newer items).
    pub since_id: Option<Id>,
    /// Return items with an id above this bound (polling for new items).
    pub min_id: Option<Id>,
    /// Maximum items to return. Zero means "no limit given".
    pub limit: usize,
}

impl PageCursor {
    /// Build a cursor from raw query parameters.
    ///
    /// Bounds pass through unchanged; `limit` is stored as given and bounded
    /// later with [`PageCursor::clamp`].
    pub fn new(max_id: Option<Id>, since_id: Option<Id>, min_id: Option<Id>, limit: usize) -> Self {
        Self {
            max_id,
            since_id,
            min_id,
            limit,
        }
    }

    /// Bound the page size to `[1, max_limit]`.
    ///
    /// A zero `limit` (client sent no `limit` parameter) falls back to
    /// `default_limit`; any other value is capped at `max_limit`. Bounds are
    /// copied through unchanged. `Self` is `Copy`, so this returns a new
    /// cursor and leaves the original untouched.
    pub fn clamp(&self, default_limit: usize, max_limit: usize) -> Self {
        let limit = if self.limit == 0 {
            default_limit
        } else {
            self.limit.min(max_limit)
        };
        Self { limit, ..*self }
    }
}

/// A [`PageCursor`] paired with its scan direction.
///
/// Most feeds scan newest-first (`ascending` is false). Timelines that
/// render oldest-first (for example a full conversation thread) set
/// `ascending` to true while keeping the same cursor bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AscendingPage {
    /// The cursor bounds and page size.
    pub cursor: PageCursor,
    /// Scan oldest-first when true, newest-first when false.
    pub ascending: bool,
}

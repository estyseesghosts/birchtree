//! S0 checks for `application::page` cursor construction and limit clamping.
//!
//! Scoped to [`PageCursor::new`], [`PageCursor::clamp`], and the
//! [`AscendingPage`] wrapper. Requires the parent to wire `pub mod page;`
//! into `src/application/mod.rs`.

use birchtree::application::page::{AscendingPage, PageCursor};
use birchtree::domain::Id;

#[test]
fn new_stores_bounds_and_limit() {
    let max = Id::new(300);
    let since = Id::new(100);
    let cursor = PageCursor::new(Some(max), Some(since), None, 20);
    assert_eq!(cursor.max_id, Some(max));
    assert_eq!(cursor.since_id, Some(since));
    assert_eq!(cursor.min_id, None);
    assert_eq!(cursor.limit, 20);
}

#[test]
fn clamp_zero_limit_falls_back_to_default() {
    let cursor = PageCursor::new(None, None, None, 0);
    let clamped = cursor.clamp(20, 40);
    assert_eq!(clamped.limit, 20);
    assert_eq!(clamped.max_id, None);
    // Copy semantics: the original cursor is unchanged.
    assert_eq!(cursor.limit, 0);
}

#[test]
fn clamp_caps_explicit_limit_at_maximum() {
    let cursor = PageCursor::new(None, None, Some(Id::new(7)), 500);
    let clamped = cursor.clamp(20, 40);
    assert_eq!(clamped.limit, 40);
    assert_eq!(clamped.min_id, Some(Id::new(7)));

    let within = PageCursor::new(None, None, None, 10).clamp(20, 40);
    assert_eq!(within.limit, 10);

    let page = AscendingPage {
        cursor: clamped,
        ascending: true,
    };
    assert!(page.ascending);
    assert_eq!(page.cursor.limit, 40);
}

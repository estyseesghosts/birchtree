//! S0 checks: `SearchQuery` and `AdminPage`/`AdminFilterPage` constructors.
//!
//! NOTE: these paths resolve once the parent wires the parent-owned
//! `src/application/mod.rs` (`pub mod search_query;` + `pub mod admin_page;`);
//! this file does not edit that shared file itself.

use birchtree::application::admin_page::{AdminFilterPage, AdminPage};
use birchtree::application::search_query::SearchQuery;
use birchtree::domain::Id;

#[test]
fn search_query_new_bundles_fields_in_order() {
    let viewer = Id::new(7);
    let query = SearchQuery::new(
        Some(viewer),
        "birch",
        Some("accounts"),
        5,
        20,
        true,
        Some(Id::new(9)),
        Some(Id::new(100)),
        Some(Id::new(50)),
    );
    assert_eq!(query.viewer, Some(viewer));
    assert_eq!(query.query, "birch");
    assert_eq!(query.kind, Some("accounts"));
    assert_eq!(query.offset, 5);
    assert_eq!(query.limit, 20);
    assert!(query.following);
    assert_eq!(query.account_id, Some(Id::new(9)));
    assert_eq!(query.max_id, Some(Id::new(100)));
    assert_eq!(query.min_id, Some(Id::new(50)));

    // `Copy`: field reads do not move the query.
    let copied = query;
    assert_eq!(copied, query);
}

#[test]
fn search_query_all_none_fields() {
    let query = SearchQuery::new(None, "x", None, 0, 20, false, None, None, None);
    assert_eq!(query.viewer, None);
    assert_eq!(query.kind, None);
    assert_eq!(query.account_id, None);
    assert_eq!(query.max_id, None);
    assert_eq!(query.min_id, None);
}

#[test]
fn admin_page_new_and_clamp() {
    let page = AdminPage::new(
        Some(Id::new(100)),
        Some(Id::new(10)),
        Some(Id::new(20)),
        true,
        200,
    );
    assert_eq!(page.max_id, Some(Id::new(100)));
    assert_eq!(page.since_id, Some(Id::new(10)));
    assert_eq!(page.min_id, Some(Id::new(20)));
    assert!(page.ascending);
    assert_eq!(page.limit, 200);

    // Over the cap clamps down; cursors and order are preserved.
    let clamped = page.clamp(20, 40);
    assert_eq!(clamped.limit, 40);
    assert_eq!(clamped.max_id, page.max_id);
    assert_eq!(clamped.since_id, page.since_id);
    assert_eq!(clamped.min_id, page.min_id);
    assert_eq!(clamped.ascending, page.ascending);

    // Zero limit falls back to the default.
    let zeroed = AdminPage::new(None, None, None, false, 0);
    assert_eq!(zeroed.clamp(20, 40).limit, 20);

    // In-range limit passes through untouched.
    let ok = AdminPage::new(None, None, None, false, 15);
    assert_eq!(ok.clamp(20, 40), ok);
}

#[test]
fn admin_filter_page_pairs_filter_and_page() {
    let page = AdminPage::new(None, None, None, false, 20);
    let paired = AdminFilterPage::new("filter-stub", page);
    assert_eq!(paired.filter, "filter-stub");
    assert_eq!(paired.page, page);
}

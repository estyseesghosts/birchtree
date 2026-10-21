//! Phase 4.1 (D10a): the home feed reads the 1000 most recent follows, and the
//! per-request account cache is sized so those follows all resolve.
//!
//! These tests drive the application use case and the store directly. Going
//! through `POST /api/v1/accounts` for a thousand accounts would hash a
//! thousand passwords, so the accounts are created with a placeholder hash.

mod common;

use birchtree::application::interactions;
use birchtree::application::ports::{AccountStore, NewLocalAccount, TimelinePage, TimelineQuery};
use birchtree::application::relationships::{self, FollowCommand};
use birchtree::application::statuses::{self, CreateStatusCommand};
use birchtree::application::timelines;
use birchtree::domain::{Id, Status};
use birchtree::persistence::redb::RedbStore;

use common::{T0, temp_store};

const BASE: &str = "https://example.social";
/// The home cap (`HOME_AUTHOR_CAP`).
const CAP: usize = 1000;

fn quick_account(store: &RedbStore, name: &str) -> Id {
    store
        .create_local_account(NewLocalAccount {
            username: name.to_string(),
            normalized_username: name.to_string(),
            domain: "example.social".to_string(),
            display_name: name.to_string(),
            password_hash: "placeholder-not-verified".to_string(),
            email: None,
            actor_uri: format!("{BASE}/users/{name}"),
            now_ms: T0,
            send_confirmation_email: false,
            approved: true,
            created_by_application: None,
            locale: None,
            invite_request: None,
            invite_code: None,
            time_zone: None,
            date_of_birth: None,
        })
        .expect("create account")
        .id
}

fn post(store: &RedbStore, account: Id, text: &str, now_ms: u64) -> Id {
    statuses::create_status(
        store,
        BASE,
        now_ms,
        CreateStatusCommand {
            account_id: account,
            text: Some(text.to_string()),
            in_reply_to_id: None,
            quoted_status_id: None,
            quote_approval_policy: None,
            sensitive: None,
            spoiler_text: None,
            visibility: Some("public".to_string()),
            language: None,
            media_ids: Vec::new(),
            poll: None,
            application_name: None,
            application_website: None,
            idempotency_key: None,
        },
        birchtree::application::statuses::DEFAULT_MAX_STATUS_LENGTH,
    )
    .expect("create status")
    .status
    .id
}

fn follow(store: &RedbStore, follower: Id, target: Id, now_ms: u64) {
    relationships::follow(
        store,
        now_ms,
        FollowCommand {
            follower_id: follower,
            target_id: target,
            show_reblogs: None,
            notify: None,
            languages: None,
        },
    )
    .expect("follow");
}

fn query(max_id: Option<Id>) -> TimelineQuery {
    TimelineQuery {
        max_id,
        since_id: None,
        min_id: None,
        limit: 40,
        only_media: false,
        local_only: false,
        remote_only: false,
    }
}

fn home_page(store: &RedbStore, viewer: Id, max_id: Option<Id>) -> TimelinePage {
    timelines::home(store, viewer, query(max_id), T0 + 1_000_000).expect("home")
}

/// Every home status, read page by page with `max_id` cursors.
fn all_home(store: &RedbStore, viewer: Id) -> Vec<Status> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let page = home_page(store, viewer, cursor);
        let Some(last) = page.statuses.last() else {
            break;
        };
        cursor = Some(last.id);
        out.extend(page.statuses);
    }
    out
}

/// A viewer who follows `count` accounts, oldest follow first. Account `i`
/// posts once, at `T0 + i`, so the first account is the oldest follow.
struct Crowd {
    viewer: Id,
    authors: Vec<Id>,
    posts: Vec<Id>,
}

fn crowd(store: &RedbStore, count: usize) -> Crowd {
    let viewer = quick_account(store, "viewer");
    let mut authors = Vec::with_capacity(count);
    let mut posts = Vec::with_capacity(count);
    for i in 0..count {
        let author = quick_account(store, &format!("u{i}"));
        posts.push(post(store, author, &format!("post {i}"), T0 + i as u64));
        authors.push(author);
    }
    for (i, author) in authors.iter().enumerate() {
        follow(store, viewer, *author, T0 + i as u64);
    }
    Crowd { viewer, authors, posts }
}

/// The boundary: a viewer following exactly the cap sees a post from the
/// 1000th follow (the oldest one). Before this change the cap was 500 and the
/// account cache was 500, so that post was dropped.
#[test]
fn a_viewer_following_the_cap_sees_a_post_from_the_thousandth_follow() {
    let (_dir, store) = temp_store();
    let c = crowd(&store, CAP);
    let seen = all_home(&store, c.viewer);
    let oldest_follow_post = c.posts[0];
    assert!(
        seen.iter().any(|s| s.id == oldest_follow_post),
        "the 1000th follow's post must be read: saw {} statuses",
        seen.len()
    );
    assert_eq!(seen.len(), CAP, "every follow's post is shown once");
}

/// One follow past the cap: the oldest follow is no longer among the 1000 most
/// recent, so its post is not read (D10a). The newest follow is still read.
#[test]
fn a_follow_beyond_the_cap_is_not_read() {
    let (_dir, store) = temp_store();
    let c = crowd(&store, CAP + 1);
    let seen: Vec<Id> = all_home(&store, c.viewer).iter().map(|s| s.id).collect();
    assert!(!seen.contains(&c.posts[0]), "the 1001st most recent follow is not read");
    assert!(seen.contains(&c.posts[CAP]), "the newest follow is read");
    assert_eq!(seen.len(), CAP);
}

/// The cap does not lose reblogs of accounts the viewer does not follow. The
/// account cache fills with the 1000 follows first, so this only holds when the
/// cache is larger than the follow cap.
#[test]
fn a_reblog_of_a_stranger_still_shows_with_a_full_follow_list() {
    let (_dir, store) = temp_store();
    let c = crowd(&store, CAP);
    let stranger = quick_account(&store, "stranger");
    let stranger_post = post(&store, stranger, "from a stranger", T0 + 5_000_000);
    let reblogger = c.authors[7];
    let reblog = interactions::reblog(&*store, reblogger, stranger_post, None, T0 + 5_000_001)
        .expect("reblog");

    let first_page = home_page(&store, c.viewer, None);
    assert!(
        first_page.statuses.iter().any(|s| s.id == reblog.id),
        "a followed account's reblog of a stranger is shown on the first page"
    );
}

/// Measurement, not a gate (run with `--ignored --nocapture`). A viewer follows
/// 1000 accounts that each post `per_author` times. Reports whether the oldest
/// follow's post is still read, and the scan counters for the first page. The
/// store's per-request merge budget (`AUTHORS_MERGE_CAP`, 5000 index entries)
/// is shared across authors in follow order, so dense follow sets can exhaust
/// it before the oldest follows are read, even below the cap.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn measure_dense_follow_sets_at_the_cap() {
    let per_author: usize = std::env::var("PER_AUTHOR").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let (_dir, store) = temp_store();
    let viewer = quick_account(&store, "viewer");
    let mut authors = Vec::new();
    for i in 0..CAP {
        authors.push(quick_account(&store, &format!("d{i}")));
    }
    for (i, author) in authors.iter().enumerate() {
        for n in 0..per_author {
            post(&store, *author, &format!("d{i} {n}"), T0 + (i * per_author + n) as u64);
        }
    }
    // The oldest follow posts last, so its post is the newest on the account.
    let fresh = post(&store, authors[0], "fresh from the oldest follow", T0 + 10_000_000);
    for (i, author) in authors.iter().enumerate() {
        follow(&store, viewer, *author, T0 + i as u64);
    }

    let before = store.metrics();
    let first = home_page(&store, viewer, None);
    let after = store.metrics();
    let fresh_on_first_page = first.statuses.iter().any(|s| s.id == fresh);
    let seen = all_home(&store, viewer);
    let fresh_read = seen.iter().any(|s| s.id == fresh);
    eprintln!(
        "per_author={per_author} follows={CAP} first_page={} fresh_on_first_page={fresh_on_first_page} \
         fresh_read_by_paging={fresh_read} items_scanned={} read_txns={} statuses_read={}",
        first.statuses.len(),
        after.items_scanned - before.items_scanned,
        after.read_txns - before.read_txns,
        seen.len(),
    );
}

/// The Phase 4 bounded-scan limits hold at the cap: one page at 1000 follows
/// walks at most 5000 index entries in at most 600 read transactions.
#[test]
fn home_scan_at_the_cap_stays_within_the_bounded_scan_limits() {
    let (_dir, store) = temp_store();
    let c = crowd(&store, CAP);

    let before = store.metrics();
    let page = home_page(&store, c.viewer, None);
    let after = store.metrics();
    assert_eq!(page.statuses.len(), 40);

    let items_scanned = after.items_scanned - before.items_scanned;
    let read_txns = after.read_txns - before.read_txns;
    eprintln!("home at {CAP} follows: items_scanned={items_scanned} read_txns={read_txns}");
    assert!(items_scanned <= 5_000, "items_scanned={items_scanned}");
    assert!(read_txns <= 600, "read_txns={read_txns}");
}

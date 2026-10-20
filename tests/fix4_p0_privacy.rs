//! Fix-round-4 P0 privacy contract tests (F1–F3).
//!
//! Upstream shapes verified against the pin (`8ff11c2b`, Mastodon v4.7.3):
//! - F1: `app/services/reblog_service.rb:23-26`
//!   (`options[:visibility] || account.user&.setting_default_privacy`,
//!   hidden target keeps target visibility).
//! - F2: `app/policies/status_policy.rb:1-30`
//!   (`show?` denies author-blocks-viewer and author-domain-blocks-viewer;
//!   `reblog?`/`favourite?`/`quote?` additionally deny viewer-blocks-author).
//!   The mute and viewer-side fetch denies, and the bookmark viewer-side
//!   deny, are F2 hardening stricter than upstream `show?` (see report).
//! - F3: `app/controllers/api/v1/statuses/reblogs_controller.rb:14-33`
//!   (destroy has no reblog gate; no-op returns the original).

mod common;

use activitypub_federation::http_signatures::generate_actor_keypair;
use birchtree::application::federation_ports::{FederationStore, RemoteAccountUpsert};
use birchtree::application::interactions;
use birchtree::application::policy;
use birchtree::application::ports::{
    AccountStore, NewStatus, RelationshipStore, StatusStore, Store, UserSettingsUpdate,
};
use birchtree::domain::{Error, Id, Visibility};

use common::{mk_account, temp_store, T0};

fn post(store: &dyn Store, author: Id, text: &str, visibility: Visibility) -> Id {
    store
        .create_status(
            NewStatus {
                account_id: author,
                uri_base: "https://example.social/users/test/statuses".to_string(),
                url_base: "https://example.social/@test".to_string(),
                source_text: text.to_string(),
                content_html: format!("<p>{text}</p>"),
                visibility,
                ..NewStatus::default()
            },
            T0,
        )
        .expect("create status")
        .status
        .id
}

fn set_default_privacy(store: &dyn Store, account: Id, privacy: &str) {
    store
        .update_user_settings(
            account,
            UserSettingsUpdate {
                default_privacy: Some(privacy.to_string()),
                ..UserSettingsUpdate::default()
            },
        )
        .expect("update settings");
}

fn is_status_not_found(err: Error) -> bool {
    matches!(err, Error::NotFound(ref msg) if msg == "status")
}

// ---------------------------------------------------------------------------
// F1: reblog visibility defaults.
// ---------------------------------------------------------------------------

#[test]
fn f1_reblog_defaults_to_viewer_default_privacy() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    set_default_privacy(&*store, viewer, "private");

    let target = post(&*store, author, "boost me", Visibility::Public);
    let reblog = interactions::reblog(&*store, viewer, target, None, T0 + 1)
        .expect("reblog succeeds");
    assert_eq!(
        reblog.visibility,
        Visibility::Private,
        "no explicit visibility must use the viewer's default_privacy, not Public"
    );
}

#[test]
fn f1_explicit_visibility_still_honored() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    set_default_privacy(&*store, viewer, "private");

    let target = post(&*store, author, "boost me", Visibility::Public);
    let reblog = interactions::reblog(&*store, viewer, target, Some("unlisted"), T0 + 1)
        .expect("reblog succeeds");
    assert_eq!(reblog.visibility, Visibility::Unlisted);
}

#[test]
fn f1_reblog_default_public_for_unlocked_without_pref() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;

    let target = post(&*store, author, "boost me", Visibility::Public);
    let reblog = interactions::reblog(&*store, viewer, target, None, T0 + 1)
        .expect("reblog succeeds");
    assert_eq!(reblog.visibility, Visibility::Public);
}

#[test]
fn f1_hidden_target_keeps_target_visibility() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    set_default_privacy(&*store, author, "public");

    // Private target reblogged by its owner: upstream keeps the target
    // visibility even over an explicit param (hidden-target rule).
    let target = post(&*store, author, "followers only", Visibility::Private);
    let reblog = interactions::reblog(&*store, author, target, None, T0 + 1)
        .expect("owner reblog succeeds");
    assert_eq!(reblog.visibility, Visibility::Private);
    let reblog = interactions::reblog(&*store, author, target, Some("public"), T0 + 2)
        .expect("owner reblog with explicit visibility succeeds");
    assert_eq!(
        reblog.visibility,
        Visibility::Private,
        "hidden target keeps target visibility over the explicit param"
    );
}

// ---------------------------------------------------------------------------
// F2: viewer-side block/mute/domain gates.
// ---------------------------------------------------------------------------

#[test]
fn f2_viewer_block_denies_fetch_and_actions() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "hello", Visibility::Public);

    // Direction check: pair_state(author, viewer) reports the viewer's block
    // of the author as `blocked_by` (persistence impl reads pair(b, a)).
    store.block(viewer, author, T0 + 1).expect("block");
    let pair = store
        .pair_state(author, viewer, T0 + 2)
        .expect("pair state");
    assert!(pair.blocked_by, "viewer-blocks-author reads as blocked_by");
    assert!(!pair.blocking, "author does not block the viewer");

    let status = store.status_by_id(target).expect("load").expect("exists");
    assert!(!policy::can_fetch(Some(viewer), &status, pair));
    assert!(!policy::can_reblog(viewer, &status, pair));
    assert!(!policy::can_favourite(viewer, &status, pair));
    assert!(!policy::can_bookmark(viewer, &status, pair));
    assert_eq!(
        policy::quote_permission(Some(viewer), &status, pair),
        policy::QuotePermission::Denied
    );

    // Through the use cases the fetch gate fires first (404-equivalent).
    let err = interactions::favourite(&*store, viewer, target, T0 + 3).unwrap_err();
    assert!(is_status_not_found(err), "favourite must 404, got ok");
    let err = interactions::reblog(&*store, viewer, target, None, T0 + 3).unwrap_err();
    assert!(is_status_not_found(err), "reblog must 404, got ok");
    let err = interactions::bookmark(&*store, viewer, target, T0 + 3).unwrap_err();
    assert!(is_status_not_found(err), "bookmark must 404, got ok");
}

#[test]
fn f2_viewer_mute_denies_fetch() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "hello", Visibility::Public);

    store
        .mute(viewer, author, 0, false, T0 + 1)
        .expect("mute");
    let status = store.status_by_id(target).expect("load").expect("exists");
    let pair = store
        .pair_state(author, viewer, T0 + 2)
        .expect("pair state");
    let facts = policy::viewer_side_facts(&*store, viewer, author, T0 + 2).expect("facts");
    assert!(facts.viewer_mutes_author);
    assert!(!policy::can_fetch_full(Some(viewer), &status, pair, facts));

    let err = interactions::favourite(&*store, viewer, target, T0 + 3).unwrap_err();
    assert!(is_status_not_found(err), "favourite of muted author must 404");
}

#[test]
fn f2_viewer_domain_block_denies_fetch() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "hello", Visibility::Public);

    let domain = store
        .account_by_id(author)
        .expect("load author")
        .expect("author")
        .domain;
    store
        .domain_block(viewer, &domain, T0 + 1)
        .expect("domain block");
    let facts = policy::viewer_side_facts(&*store, viewer, author, T0 + 2).expect("facts");
    assert!(facts.viewer_domain_blocks_author);
    let status = store.status_by_id(target).expect("load").expect("exists");
    let pair = store
        .pair_state(author, viewer, T0 + 2)
        .expect("pair state");
    assert!(!policy::can_fetch_full(Some(viewer), &status, pair, facts));
    let err = interactions::favourite(&*store, viewer, target, T0 + 3).unwrap_err();
    assert!(is_status_not_found(err), "favourite must 404");
}

#[test]
fn f2_author_domain_block_denies_remote_viewer_fetch() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let target = post(&*store, author, "hello", Visibility::Public);

    let keys = generate_actor_keypair().expect("keypair");
    let upsert = RemoteAccountUpsert {
        actor_uri: "https://remote.example/users/ron".to_string(),
        username: "ron".to_string(),
        domain: "remote.example".to_string(),
        display_name: String::new(),
        note: String::new(),
        locked: false,
        inbox_uri: "https://remote.example/users/ron/inbox".to_string(),
        shared_inbox_uri: Some("https://remote.example/inbox".to_string()),
        public_key_pem: keys.public_key,
        key_id: "https://remote.example/users/ron#main-key".to_string(),
    };
    let remote = store.upsert_remote(upsert, T0 + 1).expect("remote");

    store
        .domain_block(author, "remote.example", T0 + 2)
        .expect("domain block");
    let facts =
        policy::viewer_side_facts(&*store, remote.id, author, T0 + 3).expect("facts");
    assert!(facts.author_domain_blocks_viewer);
    let status = store.status_by_id(target).expect("load").expect("exists");
    let pair = store
        .pair_state(author, remote.id, T0 + 3)
        .expect("pair state");
    assert!(!policy::can_fetch_full(
        Some(remote.id),
        &status,
        pair,
        facts
    ));

    // A local viewer with no relations is unaffected (allow direction).
    let local = mk_account(&store, "local").id;
    let ok = interactions::favourite(&*store, local, target, T0 + 4).expect("favourite");
    assert_eq!(ok.id, target);
}

#[test]
fn f2_allow_direction_without_relations() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "hello", Visibility::Public);

    let status = store.status_by_id(target).expect("load").expect("exists");
    let pair = store
        .pair_state(author, viewer, T0 + 1)
        .expect("pair state");
    assert!(policy::can_fetch(Some(viewer), &status, pair));
    assert!(policy::can_reblog(viewer, &status, pair));
    assert!(policy::can_favourite(viewer, &status, pair));
    assert!(policy::can_bookmark(viewer, &status, pair));
    assert_eq!(
        policy::quote_permission(Some(viewer), &status, pair),
        policy::QuotePermission::Automatic
    );
    interactions::favourite(&*store, viewer, target, T0 + 2).expect("favourite");
    interactions::bookmark(&*store, viewer, target, T0 + 3).expect("bookmark");
    interactions::reblog(&*store, viewer, target, None, T0 + 4).expect("reblog");
}

#[test]
fn f2_quote_listing_root_gate_denies_blocking_viewer() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "quotable", Visibility::Public);

    store.block(viewer, author, T0 + 1).expect("block");
    let err =
        birchtree::application::quotes::quotes_of(&*store, Some(viewer), target, None, 10, T0 + 2)
            .unwrap_err();
    assert!(is_status_not_found(err), "quotes root must 404 for blocking viewer");
}

// ---------------------------------------------------------------------------
// F3: unreblog carries no reblog gate.
// ---------------------------------------------------------------------------

#[test]
fn f3_unreblog_succeeds_when_target_no_longer_rebloggable() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "boost me", Visibility::Public);

    interactions::reblog(&*store, viewer, target, None, T0 + 1).expect("reblog");
    // The viewer blocks the author afterwards: the target is no longer
    // fetchable/rebloggable, but the destroy path must still succeed.
    store.block(viewer, author, T0 + 2).expect("block");
    let status = store.status_by_id(target).expect("load").expect("exists");
    let pair = store
        .pair_state(author, viewer, T0 + 3)
        .expect("pair state");
    assert!(
        !policy::can_reblog(viewer, &status, pair),
        "precondition: reblog gate now refuses"
    );

    let back = interactions::unreblog(&*store, viewer, target, T0 + 4).expect("unreblog");
    assert_eq!(back.id, target, "destroy returns the ORIGINAL target");
    assert_eq!(back.reblogs_count, 0, "destroy decrements the count");
    let flags = store
        .viewer_status_flags(viewer, &[target])
        .expect("flags");
    assert!(
        !flags.get(&target).is_some_and(|f| f.reblogged),
        "the reblog row is gone"
    );
}

#[test]
fn f3_unreblog_noop_returns_original_when_fetchable() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "boost me", Visibility::Public);

    let back = interactions::unreblog(&*store, viewer, target, T0 + 1).expect("noop unreblog");
    assert_eq!(back.id, target, "no-op returns the ORIGINAL with no overlay error");
}

#[test]
fn f3_unreblog_noop_404s_when_target_invisible() {
    let (_dir, store) = temp_store();
    let author = mk_account(&store, "author").id;
    let viewer = mk_account(&store, "viewer").id;
    let target = post(&*store, author, "boost me", Visibility::Public);

    // No reblog exists and the viewer blocked the author: the no-op path
    // show-gates (upstream authorizes `:show?` on the no-op branch).
    store.block(viewer, author, T0 + 1).expect("block");
    let err = interactions::unreblog(&*store, viewer, target, T0 + 2).unwrap_err();
    assert!(is_status_not_found(err), "no-op with invisible target must 404");
}

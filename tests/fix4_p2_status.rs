//! Fix-round-4 P2a contract tests (statuses paging/overlays/relationships).
//!
//! Upstream shapes verified against the pin (`8ff11c2b`, Mastodon v4.7.3).
//! Each test cites the upstream file and the corresponding ours cite.
//!
//! - #4 `app/models/concerns/status/threading_concern.rb`
//!   (`descendants(..., promote: true)`): self-replies sort ahead of other
//!   replies, relative order preserved.
//! - #3b `app/controllers/api/v1/statuses/quotes_controller.rb:22-26`
//!   (revoke renders `@quote.status`, the quoting status).
//! - F3 `app/controllers/api/v1/statuses/reblogs_controller.rb:30-31`
//!   (destroy no-op returns the original; overlay wiring
//!   `reblogs_map:false` + decremented count lives in the DTO layer —
//!   parent/P2b-owned — so this asserts the use-case half: 200-shape,
//!   original status, true count, viewer flag false).
//! - #7 `app/lib/status_filter.rb` (`silenced_account?`): a silenced author's
//!   quotes stay hidden unless the viewer follows them (or is the owner).
//!   (Blocks/mutes/domains are already refused by `can_fetch_full`.)
//! - #12e `app/controllers/api/v1/accounts/follower_accounts_controller.rb`
//!   / `following_accounts_controller.rb` (`hide_results?`): suspended target
//!   yields `[]`; a target that blocks the viewer hides from that viewer;
//!   self-view is never hidden.
//! - #6 `favourited_by_accounts_controller.rb` /
//!   `reblogged_by_accounts_controller.rb` (`not_excluded_by_account` +
//!   `without_suspended`): blocked/muted/blocked-by accounts are excluded for
//!   authenticated viewers; suspended accounts never appear.
//! - #14c `app/controllers/api/v1/endorsements_controller.rb`
//!   (`unlimited?` = `limit=0` returns everything with no `Link` cursors;
//!   `endorsed_accounts...without_suspended`).
//! - #14e `app/controllers/concerns/deprecation_concern.rb`
//!   (`deprecate_api '2022-11-14'` on the v1 filters controller):
//!   `Deprecation: @1668384000`.
//! - #14a/#14b lists: `limit=0`/unknown-id parity is verified at the use-case
//!   seam (`application::lists`); the `limit=0`-means-all translation lives
//!   in `endpoints/lists.rs` (parent-owned) — the live-endpoint half is a
//!   proposed snippet in the work report.

mod common;

use axum::http::StatusCode;

use birchtree::application::lists;
use birchtree::application::ports::{
    AdminAccountPatch, NewQuoteSpec, NewStatus, QuoteStore, StatusStore, Store,
};
use birchtree::application::{interactions, quotes, relationships, statuses};
use birchtree::domain::{Id, QuotePolicy, QuoteState, Visibility};

use common::{
    TestServer, T0, form_request, get_request, json, mk_account, send, temp_store,
};

fn new_status(account_id: Id, text: &str) -> NewStatus {
    NewStatus {
        remote: None,
        account_id,
        uri_base: "https://example.social/users/test/statuses".to_string(),
        url_base: "https://example.social/@test".to_string(),
        source_text: text.to_string(),
        content_html: format!("<p>{text}</p>"),
        visibility: Visibility::Public,
        ..Default::default()
    }
}

fn post(store: &dyn Store, author: Id, text: &str, now_ms: u64) -> Id {
    store
        .create_status(new_status(author, text), now_ms)
        .expect("create status")
        .status
        .id
}

fn reply_to(
    store: &dyn Store,
    author: Id,
    parent: Id,
    parent_author: Id,
    text: &str,
    now_ms: u64,
) -> Id {
    let mut cmd = new_status(author, text);
    cmd.in_reply_to_id = Some(parent);
    cmd.in_reply_to_account_id = Some(parent_author);
    store
        .create_status(cmd, now_ms)
        .expect("create reply")
        .status
        .id
}

fn quote_of(
    store: &dyn Store,
    author: Id,
    quoted: Id,
    quoted_author: Id,
    text: &str,
    now_ms: u64,
) -> Id {
    let mut cmd = new_status(author, text);
    cmd.quote = Some(NewQuoteSpec {
        quoted_status_id: quoted,
        quoted_account_id: quoted_author,
        approval_policy: QuotePolicy::Public,
    });
    store
        .create_status(cmd, now_ms)
        .expect("create quoting status")
        .status
        .id
}

fn silence(store: &dyn Store, actor: Id, target: Id, now_ms: u64) {
    store
        .admin_update_account(
            actor,
            target,
            AdminAccountPatch {
                silenced: Some(true),
                audit_action: "silence".to_string(),
                ..Default::default()
            },
            now_ms,
        )
        .expect("silence account");
}

fn suspend(store: &dyn Store, actor: Id, target: Id, now_ms: u64) {
    store
        .admin_update_account(
            actor,
            target,
            AdminAccountPatch {
                suspended: Some(true),
                audit_action: "suspend".to_string(),
                ..Default::default()
            },
            now_ms,
        )
        .expect("suspend account");
}

fn follow(store: &dyn Store, follower: Id, target: Id, now_ms: u64) {
    relationships::follow(
        store,
        now_ms,
        relationships::FollowCommand {
            follower_id: follower,
            target_id: target,
            show_reblogs: None,
            notify: None,
            languages: None,
        },
    )
    .expect("follow");
}

// ---------------------------------------------------------------------------
// #4: self-reply promotion in context descendants.
// ---------------------------------------------------------------------------

#[test]
fn context_promotes_self_replies_ahead_of_other_replies() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let bob = mk_account(&store, "bob").id;
    let carol = mk_account(&store, "carol").id;

    let root = post(&*store, alice, "root", T0);
    // Deliberately create the self-reply FIRST (older): a naive
    // newest-first walk would list bob's reply ahead of it.
    let self_reply = reply_to(&*store, alice, root, alice, "self reply", T0 + 1);
    let other_reply = reply_to(&*store, bob, root, alice, "other reply", T0 + 2);

    let tree = statuses::status_context(&*store, Some(carol), root, T0 + 3, true)
        .expect("context loads");
    let ids: Vec<Id> = tree.descendants.iter().map(|s| s.id).collect();
    assert_eq!(
        ids,
        vec![self_reply, other_reply],
        "self-reply must sort ahead of the other reply (upstream promote: true)"
    );
}

// ---------------------------------------------------------------------------
// #3b: revoke renders the quoting status.
// ---------------------------------------------------------------------------

#[test]
fn quote_revoke_keeps_quoting_status_and_marks_quote_revoked() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let bob = mk_account(&store, "bob").id;

    let root = post(&*store, alice, "quotable", T0);
    let quoting = quote_of(&*store, bob, root, alice, "quoting", T0 + 1);
    quotes::revoke_quote(&*store, alice, root, quoting, T0 + 2).expect("revoke succeeds");

    // The quoting status still exists and renders (the endpoint answers 200
    // with it); only the quote edge flips to `revoked`.
    let rendered = store
        .status_by_id(quoting)
        .expect("read quoting status")
        .expect("quoting status survives revoke");
    assert_eq!(rendered.id, quoting);
    assert!(!rendered.is_deleted());
    let record = store
        .quote_by_quoting_status(quoting)
        .expect("read quote")
        .expect("quote row survives revoke");
    assert_eq!(record.state, QuoteState::Revoked);

    // And it drops out of the accepted-quotes listing.
    let entries = quotes::quotes_of(&*store, Some(alice), root, None, 20, T0 + 3)
        .expect("quotes index loads");
    assert!(entries.is_empty());
}

// ---------------------------------------------------------------------------
// F3: unreblog no-op returns the original (use-case half; the
// `reblogs_map:false` overlay itself is DTO-layer, parent/P2b-owned).
// ---------------------------------------------------------------------------

#[test]
fn unreblog_noop_returns_original_with_true_count() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let bob = mk_account(&store, "bob").id;

    let target = post(&*store, alice, "boost me", T0);
    // Bob never reblogged: 200-shape no-op returning the ORIGINAL.
    let original = interactions::unreblog(&*store, bob, target, T0 + 1).expect("no-op unreblog");
    assert_eq!(original.id, target);
    let stored = store
        .status_by_id(target)
        .expect("read")
        .expect("target exists");
    assert_eq!(stored.reblogs_count, 0);
    let flags = store
        .viewer_status_flags(bob, &[target])
        .expect("flags load");
    assert!(!flags.get(&target).is_some_and(|f| f.reblogged));

    // A real unreblog also returns the original with a decremented count.
    let wrapper = interactions::reblog(&*store, bob, target, None, T0 + 2).expect("reblog");
    assert_ne!(wrapper.id, target);
    let original = interactions::unreblog(&*store, bob, target, T0 + 3).expect("unreblog");
    assert_eq!(original.id, target);
    let stored = store
        .status_by_id(target)
        .expect("read")
        .expect("target exists");
    assert_eq!(stored.reblogs_count, 0);
}

// ---------------------------------------------------------------------------
// #7: silenced quoting authors are filtered unless followed (or owner).
// ---------------------------------------------------------------------------

#[test]
fn quotes_index_hides_silenced_author_unless_followed() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let bob = mk_account(&store, "bob").id;
    let carol = mk_account(&store, "carol").id;

    let root = post(&*store, alice, "quotable", T0);
    let quoting = quote_of(&*store, bob, root, alice, "quoting", T0 + 1);
    silence(&*store, alice, bob, T0 + 2);

    // Stranger: hidden.
    let entries = quotes::quotes_of(&*store, Some(carol), root, None, 20, T0 + 3)
        .expect("quotes index loads");
    assert!(entries.is_empty(), "silenced author's quote hidden from strangers");

    // Follower: visible (upstream `account_following_status_account?`).
    follow(&*store, carol, bob, T0 + 4);
    let entries = quotes::quotes_of(&*store, Some(carol), root, None, 20, T0 + 5)
        .expect("quotes index loads");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1.id, quoting);

    // Owner of the quoted status: always visible.
    let entries = quotes::quotes_of(&*store, Some(alice), root, None, 20, T0 + 6)
        .expect("quotes index loads");
    assert_eq!(entries.len(), 1);
}

// ---------------------------------------------------------------------------
// #12e: followers/following hide gate.
// ---------------------------------------------------------------------------

#[test]
fn follower_lists_hide_from_blocked_viewers_and_suspended_targets() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let bob = mk_account(&store, "bob").id;
    let carol = mk_account(&store, "carol").id;

    follow(&*store, bob, alice, T0);
    follow(&*store, carol, alice, T0 + 1);

    // Baseline: a stranger sees both followers.
    let followers =
        relationships::followers_of(&*store, Some(carol), alice, None, 40, T0 + 2)
            .expect("followers load");
    assert!(followers.contains(&bob));

    // The target blocks the viewer -> `[]` (upstream `hide_results?`).
    relationships::block(&*store, alice, carol, T0 + 3).expect("block");
    let followers =
        relationships::followers_of(&*store, Some(carol), alice, None, 40, T0 + 4)
            .expect("followers load");
    assert!(followers.is_empty(), "blocked viewer must see []");
    let following =
        relationships::following_of(&*store, Some(carol), alice, None, 40, T0 + 4)
            .expect("following loads");
    assert!(following.is_empty(), "blocked viewer must see []");

    // Self-view is never hidden.
    let followers = relationships::followers_of(&*store, Some(alice), alice, None, 40, T0 + 5)
        .expect("self followers load");
    assert!(followers.contains(&bob));

    // Suspended target hides from everyone (upstream `unavailable?`).
    suspend(&*store, alice, alice, T0 + 6);
    let followers = relationships::followers_of(&*store, Some(alice), alice, None, 40, T0 + 7)
        .expect("followers load");
    assert!(followers.is_empty(), "suspended target must yield []");
}

// ---------------------------------------------------------------------------
// #14a/#14b: list member paging seams (use-case level; `limit=0`-means-all
// translation lives in endpoints/lists.rs, parent-owned).
// ---------------------------------------------------------------------------

#[test]
fn list_members_unknown_id_is_404() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let bob = mk_account(&store, "bob").id;

    let list = lists::create(&*store, alice, "pals", None, None, T0).expect("create list");
    // Unknown member id -> 404 (upstream `Account.find` raises RecordNotFound).
    let err = lists::add_members(&*store, alice, list.id, &[Id::new(999_999_999)])
        .expect_err("unknown member must 404");
    assert!(
        matches!(err, birchtree::domain::Error::NotFound(_)),
        "unexpected error: {err:?}"
    );
    // A followed account adds cleanly (control).
    follow(&*store, alice, bob, T0 + 1);
    lists::add_members(&*store, alice, list.id, &[bob]).expect("add member");
    let members = lists::members(&*store, alice, list.id, None, 40).expect("members load");
    assert_eq!(members, vec![bob]);
}

// ---------------------------------------------------------------------------
// HTTP-level tests (#6 exclusion, #14c unlimited/without_suspended, #14e
// deprecation header). Shared app/account/token setup mirrors
// tests/phase4_lists.rs.
// ---------------------------------------------------------------------------

fn enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn register_app(srv: &TestServer, scopes: &str) -> serde_json::Value {
    let body = format!(
        "client_name=P2aStatus&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(srv: &TestServer, app: &serde_json::Value, scopes: &str) -> String {
    let body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn register_account(srv: &TestServer, app: &serde_json::Value, username: &str) -> String {
    let app_tok = app_token(srv, app, "write:accounts").await;
    let body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["access_token"].as_str().unwrap().to_string()
}

async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(
        srv,
        "read write follow profile read:accounts write:accounts read:lists write:lists read:statuses write:statuses write:favourites write:blocks read:filters write:filters",
    )
    .await;
    register_account(srv, &app, username).await
}

async fn json_post(srv: &TestServer, path: &str, body: &str, token: &str) -> serde_json::Value {
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", path, body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}: {path}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn verify_id(srv: &TestServer, token: &str) -> Id {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let id: u64 = json(&bytes)["id"]
        .as_str()
        .expect("account id")
        .parse()
        .expect("numeric id");
    Id::new(id)
}

#[tokio::test]
async fn favourited_by_excludes_blockers_and_suspended() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let bob = user_token(&srv, "bob").await;
    let carol = user_token(&srv, "carol").await;
    let bob_id = verify_id(&srv, &bob).await;
    let carol_id = verify_id(&srv, &carol).await;

    let created = json_post(&srv, "/api/v1/statuses", "status=likeable&visibility=public", &alice).await;
    let status_id = created["id"].as_str().unwrap().to_string();

    // Two favouriters.
    json_post(&srv, &format!("/api/v1/statuses/{status_id}/favourite"), "", &bob).await;
    json_post(&srv, &format!("/api/v1/statuses/{status_id}/favourite"), "", &carol).await;

    // Baseline: both visible to the author.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/statuses/{status_id}/favourited_by"),
            Some(&alice),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let ids: Vec<String> = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&bob_id.to_string()));
    assert!(ids.contains(&carol_id.to_string()));

    // The author blocks bob -> bob drops out for authenticated viewers.
    json_post(&srv, &format!("/api/v1/accounts/{bob_id}/block"), "", &alice).await;
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/statuses/{status_id}/favourited_by"),
            Some(&alice),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let ids: Vec<String> = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!ids.contains(&bob_id.to_string()), "blocked account must be excluded");
    assert!(ids.contains(&carol_id.to_string()));

    // Suspended accounts never appear, even anonymously.
    suspend(&*srv.store, verify_id(&srv, &alice).await, carol_id, T0 + 100);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/statuses/{status_id}/favourited_by"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let ids: Vec<String> = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!ids.contains(&carol_id.to_string()), "suspended account must be excluded");
}

#[tokio::test]
async fn endorsements_unlimited_returns_all_without_link() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let bob = user_token(&srv, "bob").await;
    let carol = user_token(&srv, "carol").await;
    let alice_id = verify_id(&srv, &alice).await;
    let bob_id = verify_id(&srv, &bob).await;
    let carol_id = verify_id(&srv, &carol).await;

    // Endorsing requires an active follow.
    json_post(&srv, &format!("/api/v1/accounts/{bob_id}/follow"), "", &alice).await;
    json_post(&srv, &format!("/api/v1/accounts/{carol_id}/follow"), "", &alice).await;
    json_post(&srv, &format!("/api/v1/accounts/{bob_id}/pin"), "", &alice).await;
    json_post(&srv, &format!("/api/v1/accounts/{carol_id}/pin"), "", &alice).await;

    // `limit=0` = unlimited: everything, no `Link` header.
    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/endorsements?limit=0", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes).as_array().unwrap().len(), 2);
    assert!(
        headers.get("link").is_none(),
        "unlimited endorsements must not paginate"
    );

    // Suspended endorsements drop out (`without_suspended`).
    suspend(&*srv.store, alice_id, carol_id, T0 + 100);
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/endorsements?limit=0", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let ids: Vec<String> = json(&bytes)
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![bob_id.to_string()]);
}

#[tokio::test]
async fn v1_filters_carry_deprecation_header() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;

    let (status, headers, bytes) = send(
        &srv.router,
        get_request("/api/v1/filters", Some(&alice), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        headers.get("deprecation").map(|v| v.to_str().unwrap().to_string()),
        Some("@1668384000".to_string()),
        "v1 filters must carry upstream's deprecation timestamp"
    );

    // And on a write response too.
    let (status, headers, _bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/filters",
            "phrase=spoiler&context[]=home",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("deprecation").map(|v| v.to_str().unwrap().to_string()),
        Some("@1668384000".to_string())
    );
}

//! G-C (`docs/gaps.md`): collections CRUD + items + revoke, `in_collections`,
//! account `collections` / `lists` sub-resources, `domain_blocks/preview`,
//! `peers/search`, the minimal `v1_alpha` namespace, the v2 notifications
//! family, `hide_collections` enforcement, the `reblogged_by`
//! distributable-visibility scope, and the `CollectionFanout` job body.
//!
//! The G-C handlers are plain fns (see `endpoints::collections` header for
//! why), so this suite calls them directly with forged `Auth` values; only
//! status/follow/reblog/list setup goes through HTTP (real tokens via the
//! OAuth dance, as in `phase4_lists.rs`). The router-snippet paths are
//! asserted via the `ROUTES` table until the parent mounts them.

mod common;

use activitypub_federation::http_signatures::generate_actor_keypair;
use axum::http::StatusCode;
use axum::response::Response;

use birchtree::api::mastodon::auth::{Auth, Authenticated, OptionalAuth};
use birchtree::api::mastodon::endpoints::collections as gc;
use birchtree::application::collections::{self};
use birchtree::application::federation_ports::{FederationStore, RemoteAccountUpsert};
use birchtree::application::ports::{
    AccountStore, AccountProfileUpdate, CollectionStore, JobQueue, NewNotification,
    NotificationStore, StatusStore,
};
use birchtree::domain::{
    AccessToken, Clock, Id, Job, JobKind, NotificationType, OAuthApplication, SystemClock,
};

use common::{TestServer, form_request, get_request, json, send};

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn now_ms() -> u64 {
    SystemClock.now_ms()
}

fn user_auth(user: Id, scopes: &[&str]) -> Auth {
    Auth(Authenticated {
        token: AccessToken {
            id: Id::new(1),
            app_id: Id::new(2),
            user_id: Some(user),
            token_hash: "forged".to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            created_at_ms: 0,
            expires_at_ms: None,
            revoked: false,
        },
        app: OAuthApplication {
            id: Id::new(2),
            name: "gaps-collections".to_string(),
            client_id: "client".to_string(),
            client_secret_hash: None,
            redirect_uris: Vec::new(),
            scopes: Vec::new(),
            website: None,
            created_at_ms: 0,
        },
        approved: true,
    })
}

fn app_auth() -> Auth {
    let mut auth = user_auth(Id::new(99), &["read", "write"]);
    auth.0.token.user_id = None;
    auth
}

fn opt(auth: &Auth) -> OptionalAuth {
    OptionalAuth(Some(auth.0.clone()))
}

fn anon() -> OptionalAuth {
    OptionalAuth(None)
}

const READ_WRITE: &[&str] = &["read", "write"];

async fn body(response: Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json"))
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn ictx<'a>(
    store: &'a birchtree::persistence::redb::RedbStore,
    server: &'a birchtree::api::mastodon::ServerInfo,
) -> gc::CollectionCtx<'a> {
    gc::CollectionCtx {
        store,
        collections: store,
        server,
        now_ms: now_ms(),
    }
}

// -- OAuth dance (copied shape from `phase4_lists.rs`) for real tokens. -----

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

async fn user_token_and_id(srv: &TestServer, username: &str) -> (String, Id) {
    let app_body = format!(
        "client_name=GapsC&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc("read write follow profile read:accounts write:accounts read:lists write:lists"),
    );
    let (status, _h, bytes) = send(&srv.router, form_request("POST", "/api/v1/apps", &app_body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app = json(&bytes);
    let tok_body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
        enc(app["client_id"].as_str().unwrap()),
        enc(app["client_secret"].as_str().unwrap()),
        enc("write:accounts"),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/oauth/token", &tok_body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let app_tok = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let reg_body = format!(
        "username={username}&email={username}%40example.com&password=password123&agreement=true&locale=en"
    );
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/accounts", &reg_body, Some(&app_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let token = json(&bytes)["access_token"].as_str().unwrap().to_string();
    let (status, _h, bytes) =
        send(&srv.router, get_request("/api/v1/accounts/verify_credentials", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let id: Id = json(&bytes)["id"].as_str().unwrap().parse().unwrap();
    (token, id)
}

async fn post_status(srv: &TestServer, token: &str, text: &str, visibility: &str) -> Id {
    let body = format!("status={}&visibility={}", enc(text), enc(visibility));
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/statuses", &body, Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().parse().unwrap()
}

/// A remote actor on `domain` (no network fetch is made for it).
fn remote_account(
    store: &birchtree::persistence::redb::RedbStore,
    name: &str,
    domain: &str,
) {
    let keys = generate_actor_keypair().expect("keypair");
    let actor_uri = format!("https://{domain}/users/{name}");
    store
        .upsert_remote(
            RemoteAccountUpsert {
                actor_uri: actor_uri.clone(),
                username: name.to_string(),
                domain: domain.to_string(),
                display_name: String::new(),
                note: String::new(),
                locked: false,
                inbox_uri: format!("{actor_uri}/inbox"),
                shared_inbox_uri: None,
                public_key_pem: keys.public_key,
                key_id: format!("{actor_uri}#main-key"),
            },
            now_ms(),
        )
        .expect("upsert remote");
}

// ---------------------------------------------------------------------------
// Collections CRUD.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn collection_crud_round_trip_with_exact_keys() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&*store, "alice");
    let bob = common::mk_account(&*store, "bob");
    let alice_auth = user_auth(alice.id, READ_WRITE);
    let no_scope = user_auth(alice.id, &[]);

    // Create.
    let (status, created) = body(gc::collections_create(
        &*store,
        &alice_auth,
        &pairs(&[("title", "Reads"), ("description", "long reads"), ("public", "true")]),
        now_ms(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(
        created.as_object().unwrap().keys().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
        vec!["account_id", "created_at", "description", "id", "items_count", "public", "revoked", "title", "updated_at"]
    );
    assert_eq!(created["title"], "Reads");
    assert_eq!(created["public"], true);
    assert_eq!(created["revoked"], false);
    assert_eq!(created["items_count"], 0);
    let id = created["id"].as_str().unwrap().to_string();

    // Index shows it; v1_alpha mirrors the index.
    let (status, index) = body(gc::collections_index(&*store, &alice_auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(index.as_array().unwrap().len(), 1);
    let (status, alpha) = body(gc::alpha_collections_index(&*store, &alice_auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(alpha, index);

    // Show (owner + anonymous public read).
    let (status, shown) = body(gc::collections_show(&*store, &opt(&alice_auth), &id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown["title"], "Reads");
    let (status, _) = body(gc::collections_show(&*store, &anon(), &id)).await;
    assert_eq!(status, StatusCode::OK);

    // Update.
    let (status, updated) = body(gc::collections_update(
        &*store,
        &alice_auth,
        &id,
        &pairs(&[("title", "Reads 2"), ("public", "false")]),
        now_ms(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["title"], "Reads 2");
    assert_eq!(updated["public"], false);

    // Private now: bob and anonymous 404.
    let bob_auth = user_auth(bob.id, READ_WRITE);
    let (status, _) = body(gc::collections_show(&*store, &opt(&bob_auth), &id)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = body(gc::collections_show(&*store, &anon(), &id)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Validation + auth edges.
    let (status, err) = body(gc::collections_create(&*store, &alice_auth, &pairs(&[("title", "  ")]), now_ms())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err["error"], "Validation failed: Title can't be blank");
    let (status, _) = body(gc::collections_create(&*store, &no_scope, &pairs(&[("title", "x")]), now_ms())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = body(gc::collections_index(&*store, &app_auth())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = body(gc::collections_show(&*store, &opt(&alice_auth), "bogus")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Bob cannot mutate alice's collection (no oracle: 404, not 403).
    let (status, _) = body(gc::collections_update(&*store, &bob_auth, &id, &pairs(&[("title", "hi")]), now_ms())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = body(gc::collections_destroy(&*store, &bob_auth, &id, now_ms())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Destroy.
    let (status, _) = body(gc::collections_destroy(&*store, &alice_auth, &id, now_ms())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = body(gc::collections_show(&*store, &opt(&alice_auth), &id)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Items add/remove/list + revoke + fan-out enqueue.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn items_add_remove_round_trip() {
    let srv = TestServer::new(true);
    let (alice_tok, alice_id) = user_token_and_id(&srv, "alice").await;
    let (_bob_tok, bob_id) = user_token_and_id(&srv, "bob").await;
    let s1 = post_status(&srv, &alice_tok, "first", "public").await;
    let s2 = post_status(&srv, &alice_tok, "second", "public").await;

    let store = &*srv.store;
    let alice_auth = user_auth(alice_id, READ_WRITE);
    let (_, created) = body(gc::collections_create(store, &alice_auth, &pairs(&[("title", "c")]), now_ms())).await;
    let id = created["id"].as_str().unwrap().to_string();

    // Add both (private collection default).
    let s1s = s1.to_string();
    let s2s = s2.to_string();
    let (status, added) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, &id, &pairs(&[("status_ids[]", s1s.as_str()), ("status_ids[]", s2s.as_str())]))).await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(added["added"].as_array().unwrap().len(), 2);

    // Duplicate add is idempotent.
    let (status, added) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, &id, &pairs(&[("status_ids[]", s1s.as_str())]))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(added["added"].as_array().unwrap().is_empty());

    // Owner reads both members, newest first.
    let server = common::server_info();
    let (status, items) = body(gc::items_index(&ictx(store, &server), &opt(&alice_auth), &id, None)).await;
    assert_eq!(status, StatusCode::OK, "{items}");
    assert_eq!(items.as_array().unwrap().len(), 2);
    assert_eq!(items[0]["id"], s2.to_string());
    assert_eq!(items[1]["id"], s1.to_string());

    // Cursor paging.
    let (status, page) = body(gc::items_index(&ictx(store, &server), &opt(&alice_auth), &id, Some(&format!("max_id={s2}")))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["id"], s1.to_string());

    // Remove one; repeat is a no-op.
    let (status, removed) = body(gc::items_remove(store, &alice_auth, &id, &pairs(&[("status_ids[]", s1s.as_str())]), now_ms())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed["removed"][0], s1.to_string());
    let (status, removed) = body(gc::items_remove(store, &alice_auth, &id, &pairs(&[("status_ids[]", s1s.as_str())]), now_ms())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(removed["removed"].as_array().unwrap().is_empty());

    // Edges: unknown status, foreign collection, oversized batch.
    // (A syntactically huge but valid id: missing rows 404. An unparsable
    // id is dropped from `status_ids[]`, like list membership ids.)
    let (status, _) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, &id, &pairs(&[("status_ids[]", "18446744073709551615")]))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let bob_auth = user_auth(bob_id, READ_WRITE);
    let (status, _) = body(gc::items_add(&ictx(store, &common::server_info()), &bob_auth, &id, &pairs(&[("status_ids[]", s2s.as_str())]))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let big: Vec<(String, String)> = (0..501).map(|n| ("status_ids[]".to_string(), Id::new(1_000_000 + n).to_string())).collect();
    let (status, err) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, &id, &big)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err["error"], "Validation failed: Too many status ids (maximum is 500)");
}

#[tokio::test]
async fn revoke_clears_items_and_enqueues_bounded_job() {
    let srv = TestServer::new(true);
    let (alice_tok, alice_id) = user_token_and_id(&srv, "alice").await;
    let s1 = post_status(&srv, &alice_tok, "one", "public").await;
    let s2 = post_status(&srv, &alice_tok, "two", "public").await;

    let store = &*srv.store;
    let alice_auth = user_auth(alice_id, READ_WRITE);
    let (_, created) = body(gc::collections_create(store, &alice_auth, &pairs(&[("title", "c")]), now_ms())).await;
    let id = created["id"].as_str().unwrap().to_string();
    let s1s = s1.to_string();
    let s2s = s2.to_string();
    let (status, _) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, &id, &pairs(&[("status_ids[]", s1s.as_str()), ("status_ids[]", s2s.as_str())]))).await;
    assert_eq!(status, StatusCode::OK);

    // Revoke.
    let (status, revoked) = body(gc::collections_revoke(store, &alice_auth, &id, now_ms())).await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert_eq!(revoked["revoked"], true);
    assert_eq!(revoked["items_count"], 0);

    // Members gone, counts stay rendered.
    let server = common::server_info();
    let (status, items) = body(gc::items_index(&ictx(store, &server), &opt(&alice_auth), &id, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(items.as_array().unwrap().is_empty());
    let (status, shown) = body(gc::collections_show(store, &opt(&alice_auth), &id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown["items_count"], 0);

    // One bounded fan-out job per mutation, committed with the mutation.
    let jobs = store.claim_due(now_ms(), 100).expect("claim");
    let fanouts: Vec<_> = jobs.iter().filter(|j| j.kind == JobKind::CollectionFanout).collect();
    assert!(fanouts.len() >= 2, "add + revoke jobs, got {}", fanouts.len());
    for job in &fanouts {
        let bytes = serde_json::to_vec(&job.payload).unwrap();
        assert!(bytes.len() < 64 * 1024, "{}", bytes.len());
        assert!(job.payload["item_ids"].as_array().unwrap().len() <= 500);
    }
    let revoke_job = fanouts.iter().find(|j| j.payload["change"] == "revoke").expect("revoke job");
    assert_eq!(revoke_job.payload["item_ids"].as_array().unwrap().len(), 2);

    // Revoked collections refuse new items and edits.
    let (status, err) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, &id, &pairs(&[("status_ids[]", s1s.as_str())]))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err["error"], "Validation failed: Collection has been revoked");
    let (status, _) = body(gc::collections_update(store, &alice_auth, &id, &pairs(&[("title", "x")]), now_ms())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------------------------
// `hide_collections`: members hidden, counts stay.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hide_collections_members_hidden_counts_kept() {
    let srv = TestServer::new(true);
    let (alice_tok, alice_id) = user_token_and_id(&srv, "alice").await;
    let (_bob_tok, bob_id) = user_token_and_id(&srv, "bob").await;
    let s1 = post_status(&srv, &alice_tok, "hello", "public").await;

    let store = &*srv.store;
    let alice_auth = user_auth(alice_id, READ_WRITE);
    let bob_auth = user_auth(bob_id, READ_WRITE);
    let (_, created) = body(gc::collections_create(
        store, &alice_auth,
        &pairs(&[("title", "c"), ("public", "true")]), now_ms(),
    )).await;
    let id = created["id"].as_str().unwrap().to_string();
    let s1s = s1.to_string();
    let (status, _) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, &id, &pairs(&[("status_ids[]", s1s.as_str())]))).await;
    assert_eq!(status, StatusCode::OK);

    // Flag the owner.
    store
        .update_account_profile(
            alice_id,
            AccountProfileUpdate { hide_collections: Some(true), ..Default::default() },
            now_ms(),
        )
        .expect("flag hide_collections");

    let server = common::server_info();
    // A non-owner sees no members...
    let (status, items) = body(gc::items_index(&ictx(store, &server), &opt(&bob_auth), &id, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(items.as_array().unwrap().is_empty(), "{items}");
    let (status, items) = body(gc::items_index(&ictx(store, &server), &anon(), &id, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(items.as_array().unwrap().is_empty());
    // ...but the collection (with its count) stays listed.
    let (status, shown) = body(gc::collections_show(store, &opt(&bob_auth), &id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown["items_count"], 1);
    let (status, listed) = body(gc::account_collections(store, store, &anon(), &alice_id.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["items_count"], 1);
    // The owner still sees members.
    let (status, items) = body(gc::items_index(&ictx(store, &server), &opt(&alice_auth), &id, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(items.as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// `in_collections`, account `collections`, account `lists`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn in_collections_and_account_collections() {
    let srv = TestServer::new(true);
    let (alice_tok, alice_id) = user_token_and_id(&srv, "alice").await;
    let (_bob_tok, bob_id) = user_token_and_id(&srv, "bob").await;
    let s1 = post_status(&srv, &alice_tok, "post", "public").await;

    let store = &*srv.store;
    let alice_auth = user_auth(alice_id, READ_WRITE);
    let bob_auth = user_auth(bob_id, READ_WRITE);
    let (_, created) = body(gc::collections_create(store, &alice_auth, &pairs(&[("title", "pub"), ("public", "true")]), now_ms())).await;
    let c_public = created["id"].as_str().unwrap().to_string();
    let (_, created) = body(gc::collections_create(store, &alice_auth, &pairs(&[("title", "priv"), ("public", "false")]), now_ms())).await;
    let c_private = created["id"].as_str().unwrap().to_string();
    let s1s = s1.to_string();
    for cid in [&c_public, &c_private] {
        let (status, _) = body(gc::items_add(&ictx(store, &common::server_info()), &alice_auth, cid, &pairs(&[("status_ids[]", s1s.as_str())]))).await;
        assert_eq!(status, StatusCode::OK);
    }

    // `in_collections`: the owner's collections containing the status.
    let (status, rows) = body(gc::in_collections(store, &alice_auth, &s1.to_string())).await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert_eq!(rows.as_array().unwrap().len(), 2);
    // Bob's own (empty) answer; garbage ids 404.
    let (status, rows) = body(gc::in_collections(store, &bob_auth, &s1.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(rows.as_array().unwrap().is_empty());
    let (status, _) = body(gc::in_collections(store, &alice_auth, "nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Account `collections`: public only; unknown accounts 404.
    let (status, listed) = body(gc::account_collections(store, store, &anon(), &alice_id.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], c_public.as_str());
    let (status, _) = body(gc::account_collections(store, store, &anon(), "123456789")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn account_lists_subresource() {
    let srv = TestServer::new(true);
    let (alice_tok, alice_id) = user_token_and_id(&srv, "alice").await;
    let (_bob_tok, bob_id) = user_token_and_id(&srv, "bob").await;

    // Follow + list + membership through the real use cases/HTTP.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/accounts/{bob_id}/follow"), "", Some(&alice_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    let store = &*srv.store;
    let list = birchtree::application::lists::create(store, alice_id, "Close", None, None, now_ms())
        .expect("create list");
    birchtree::application::lists::add_members(store, alice_id, list.id, &[bob_id])
        .expect("add member");
    let other = birchtree::application::lists::create(store, alice_id, "Far", None, None, now_ms())
        .expect("create list");

    let alice_auth = user_auth(alice_id, &["read", "read:lists"]);
    let (status, rows) = body(gc::account_lists(store, &alice_auth, &bob_id.to_string())).await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], list.id.to_string());
    assert_eq!(rows[0]["title"], "Close");
    let _ = other;

    // Unknown accounts 404; scoped-out tokens 403.
    let (status, _) = body(gc::account_lists(store, &alice_auth, "123456789")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let scoped_out = user_auth(alice_id, &["write"]);
    let (status, _) = body(gc::account_lists(store, &scoped_out, &bob_id.to_string())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// `domain_blocks/preview`, `peers/search`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn domain_blocks_preview_counts_remote_only() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&*store, "alice");
    remote_account(&*store, "ray", "peer.example");
    remote_account(&*store, "sam", "peer.example");
    remote_account(&*store, "zed", "other.example");
    let auth = user_auth(alice.id, &["follow", "write", "write:blocks"]);

    let (status, preview) = body(gc::domain_blocks_preview(
        &*store, &auth, &pairs(&[("domain", "peer.example")]),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["domain"], "peer.example");
    assert_eq!(preview["accounts_count"], 2);

    // Case-insensitive; unknown domains count zero (never fabricated).
    let (status, preview) = body(gc::domain_blocks_preview(
        &*store, &auth, &pairs(&[("domain", "PEER.EXAMPLE")]),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["accounts_count"], 2);
    let (status, preview) = body(gc::domain_blocks_preview(
        &*store, &auth, &pairs(&[("domain", "unknown.example")]),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["accounts_count"], 0);
    // Local domains are not remote accounts.
    let (status, preview) = body(gc::domain_blocks_preview(
        &*store, &auth, &pairs(&[("domain", "example.social")]),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["accounts_count"], 0);

    // Edges: blank domain 422, scoped-out 403, app-only 422.
    let (status, err) = body(gc::domain_blocks_preview(&*store, &auth, &pairs(&[("domain", "  ")]))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err["error"], "Validation failed: Domain can't be blank");
    let (status, _) =
        body(gc::domain_blocks_preview(&*store, &user_auth(alice.id, &["read"]), &pairs(&[("domain", "peer.example")]))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) =
        body(gc::domain_blocks_preview(&*store, &app_auth(), &pairs(&[("domain", "peer.example")]))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn peers_search_filters_and_gates() {
    let (_dir, store) = common::temp_store();
    remote_account(&*store, "ray", "peer.example");
    remote_account(&*store, "sam", "other.example");

    let (status, all) = body(gc::peers_search(&*store, true, None)).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all, serde_json::json!(["other.example", "peer.example"]));

    let (status, filtered) = body(gc::peers_search(&*store, true, Some("q=peer"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered, serde_json::json!(["peer.example"]));

    let (status, empty) = body(gc::peers_search(&*store, true, Some("q=nope"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(empty.as_array().unwrap().is_empty());

    // Disabled API 404s, like `peers`.
    let (status, _) = body(gc::peers_search(&*store, false, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// v2 notifications family.
// ---------------------------------------------------------------------------

fn notify(
    store: &birchtree::persistence::redb::RedbStore,
    recipient: Id,
    kind: NotificationType,
    actor: Option<Id>,
    status: Option<Id>,
) {
    store
        .create_notification(NewNotification {
            recipient_id: recipient,
            r#type: kind,
            actor_id: actor,
            status_id: status,
            created_at_ms: now_ms(),
            filtered: false,
        })
        .expect("create notification");
}

#[tokio::test]
async fn v2_notifications_family_smoke() {
    let (_dir, store) = common::temp_store();
    let alice = common::mk_account(&*store, "alice");
    let bob = common::mk_account(&*store, "bob");
    let carol = common::mk_account(&*store, "carol");
    let status_a = Id::new(10_000_001);
    let status_b = Id::new(10_000_002);

    // follow(alice), mention(alice,sA), mention(carol,sB), follow(carol).
    notify(&*store, bob.id, NotificationType::Follow, Some(alice.id), None);
    notify(&*store, bob.id, NotificationType::Mention, Some(alice.id), Some(status_a));
    notify(&*store, bob.id, NotificationType::Mention, Some(carol.id), Some(status_b));
    notify(&*store, bob.id, NotificationType::Follow, Some(carol.id), None);

    let bob_auth = user_auth(bob.id, &["read", "read:notifications"]);
    let bob_write = user_auth(bob.id, &["write", "write:notifications"]);

    // Index groups by (type, anchor): 4 distinct groups here.
    let (status, groups) = body(gc::v2_index(&*store, &bob_auth, None)).await;
    assert_eq!(status, StatusCode::OK, "{groups}");
    let groups = groups.as_array().unwrap();
    assert_eq!(groups.len(), 4);
    for group in groups {
        let keys: std::collections::BTreeSet<_> =
            group.as_object().unwrap().keys().collect();
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec!["group_key", "most_recent_notification_id", "notifications_count", "page_max_id", "page_min_id", "sample_account_ids", "status_id", "type"]
        );
    }
    let mention_a = groups
        .iter()
        .find(|g| g["group_key"] == format!("mention-{}", status_a.value()))
        .expect("mention group for status A");
    assert_eq!(mention_a["type"], "mention");
    assert_eq!(mention_a["notifications_count"], 1);
    assert_eq!(mention_a["sample_account_ids"][0], alice.id.to_string());
    assert_eq!(mention_a["status_id"], status_a.to_string());

    // Same-type rows sharing one anchor merge into one group.
    notify(&*store, bob.id, NotificationType::Mention, Some(carol.id), Some(status_b));
    let (status, groups) = body(gc::v2_index(&*store, &bob_auth, None)).await;
    assert_eq!(status, StatusCode::OK);
    let groups = groups.as_array().unwrap();
    assert_eq!(groups.len(), 4, "{groups:?}");
    let mention_b = groups
        .iter()
        .find(|g| g["group_key"] == format!("mention-{}", status_b.value()))
        .unwrap();
    assert_eq!(mention_b["notifications_count"], 2);

    // Group accounts.
    let server = common::server_info();
    let key = format!("mention-{}", status_b.value());
    let (status, accounts) =
        body(gc::v2_group_accounts(&*store, &server, &bob_auth, &key)).await;
    assert_eq!(status, StatusCode::OK, "{accounts}");
    assert_eq!(accounts.as_array().unwrap().len(), 1);
    assert_eq!(accounts[0]["id"], carol.id.to_string());
    let (status, _) = body(gc::v2_group_accounts(&*store, &server, &bob_auth, "mention-1")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Dismiss removes the whole group; unknown keys 404.
    let (status, _) = body(gc::v2_dismiss(&*store, &bob_write, &key)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, groups) = body(gc::v2_index(&*store, &bob_auth, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(groups.as_array().unwrap().len(), 3);
    let (status, _) = body(gc::v2_dismiss(&*store, &bob_write, &key)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = body(gc::v2_dismiss(&*store, &bob_write, "bogus")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // unread_count mirrors the marker; policy mirrors the v1 shape.
    let (status, count) = body(gc::v2_unread_count(&*store, &bob_auth, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count["count"], 3);
    let (status, policy) = body(gc::v2_policy_show(&*store, &bob_auth)).await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    assert_eq!(policy["filter_private_mentions"], true);
    assert_eq!(policy["filter_not_following"], false);

    // Clear empties everything.
    let (status, _) = body(gc::v2_clear(&*store, &bob_write)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, groups) = body(gc::v2_index(&*store, &bob_auth, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(groups.as_array().unwrap().is_empty());
    let (status, count) = body(gc::v2_unread_count(&*store, &bob_auth, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count["count"], 0);

    // Scopes enforced.
    let scoped_out = user_auth(bob.id, &["write"]);
    let (status, _) = body(gc::v2_index(&*store, &scoped_out, None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// `reblogged_by` distributable-visibility scope.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reblogged_by_distributable_scope_real() {
    let srv = TestServer::new(true);
    let (alice_tok, _alice_id) = user_token_and_id(&srv, "alice").await;
    let (bob_tok, bob_id) = user_token_and_id(&srv, "bob").await;
    let (_carol_tok, carol_id) = user_token_and_id(&srv, "carol").await;
    let pub_status = post_status(&srv, &alice_tok, "public post", "public").await;

    // Bob reblogs through the real policy path (public wrapper).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", &format!("/api/v1/statuses/{pub_status}/reblog"), "", Some(&bob_tok)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    // A private-visibility wrapper cannot arise through the reblog policy
    // (`can_reblog` forbids it, mirroring upstream), but the store scan must
    // still scope such rows (federation ingest carries remote wrappers
    // verbatim). Plant one directly at the store layer.
    store_reblog(&srv, carol_id, pub_status);

    let store = &*srv.store;
    // The plain scan attributes both rebloggers...
    let plain = store.reblogged_by(pub_status, None, 40).expect("plain scan");
    assert!(plain.iter().any(|(who, _)| *who == bob_id), "{plain:?}");
    assert!(plain.iter().any(|(who, _)| *who == carol_id), "{plain:?}");
    // ...while the distributable scope keeps the public wrapper only.
    let scoped = store
        .reblogged_by_distributable(pub_status, None, 40)
        .expect("scoped scan");
    assert!(scoped.iter().any(|(who, _)| *who == bob_id), "{scoped:?}");
    assert!(!scoped.iter().any(|(who, _)| *who == carol_id), "{scoped:?}");
    assert!(scoped.len() < plain.len());
    // Empty limit stays empty.
    let empty: Vec<(Id, Id)> = store
        .reblogged_by_distributable(pub_status, None, 0)
        .expect("empty");
    assert!(empty.is_empty());
}

/// Plant a reblog wrapper with an explicit visibility, bypassing the
/// use-case policy (which forbids non-distributable wrappers locally).
fn store_reblog(srv: &TestServer, account_id: Id, status_id: Id) {
    use birchtree::domain::Visibility;
    srv.store
        .reblog_status(account_id, status_id, Visibility::Private, now_ms())
        .expect("plant wrapper");
}

// ---------------------------------------------------------------------------
// `CollectionFanout` job body.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn collection_fanout_notifies_authors_idempotently() {
    let srv = TestServer::new(true);
    let (alice_tok, alice_id) = user_token_and_id(&srv, "alice").await;
    let (_bob_tok, bob_id) = user_token_and_id(&srv, "bob").await;
    let bob_post = post_status(&srv, &_bob_tok, "bob post", "public").await;
    let alice_post = post_status(&srv, &alice_tok, "alice post", "public").await;

    let store = &*srv.store;
    let alice_auth = user_auth(alice_id, READ_WRITE);
    let (_, created) = body(gc::collections_create(store, &alice_auth, &pairs(&[("title", "c")]), now_ms())).await;
    let cid: Id = created["id"].as_str().unwrap().parse().unwrap();

    // Alice adds bob's post + her own post.
    let bob_post_s = bob_post.to_string();
    let alice_post_s = alice_post.to_string();
    let cid_s = cid.to_string();
    let (status, _) = body(gc::items_add(
        &ictx(store, &common::server_info()),
        &alice_auth,
        &cid_s,
        &pairs(&[("status_ids[]", bob_post_s.as_str()), ("status_ids[]", alice_post_s.as_str())]),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);

    // Run the enqueued add job.
    let jobs = store.claim_due(now_ms(), 100).expect("claim");
    let add = jobs
        .iter()
        .find(|j| j.kind == JobKind::CollectionFanout && j.payload["change"] == "add")
        .expect("add job");
    collections::handle_fanout(store, store, add).expect("fanout");
    // Retry is idempotent (dedup index: same rows, no duplicates).
    collections::handle_fanout(store, store, add).expect("fanout retry");

    let bobs = store
        .notifications_page(bob_id, 40, None, None, None)
        .expect("bob notifications");
    assert_eq!(bobs.len(), 1, "{bobs:?}");
    assert_eq!(bobs[0].r#type, NotificationType::AddedToCollection);
    assert_eq!(bobs[0].actor_id, Some(alice_id));
    assert_eq!(bobs[0].status_id, Some(bob_post));
    // Self-add notifies nobody (no self-notifications).
    let alices = store
        .notifications_page(alice_id, 40, None, None, None)
        .expect("alice notifications");
    assert!(alices.is_empty(), "{alices:?}");

    // Remove fans out as `collection_update`.
    let (status, _) = body(gc::items_remove(store, &alice_auth, &cid_s, &pairs(&[("status_ids[]", bob_post_s.as_str())]), now_ms())).await;
    assert_eq!(status, StatusCode::OK);
    let jobs = store.claim_due(now_ms(), 100).expect("claim");
    let remove = jobs
        .iter()
        .find(|j| j.kind == JobKind::CollectionFanout && j.payload["change"] == "remove")
        .expect("remove job");
    collections::handle_fanout(store, store, remove).expect("fanout");
    let bobs = store
        .notifications_page(bob_id, 40, None, None, None)
        .expect("bob notifications");
    assert_eq!(bobs.len(), 2, "{bobs:?}");
    assert!(bobs.iter().any(|n| n.r#type == NotificationType::CollectionUpdate));

    // Wrong-kind jobs and malformed payloads are honest errors, never silent.
    let mut wrong: Job = (*remove).clone();
    wrong.kind = JobKind::RelayForward;
    assert!(collections::handle_fanout(store, store, &wrong).is_err());
    let mut bad: Job = (*remove).clone();
    bad.kind = JobKind::CollectionFanout;
    bad.payload = serde_json::json!({});
    assert!(collections::handle_fanout(store, store, &bad).is_err());
}

// ---------------------------------------------------------------------------
// Route table.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn routes_table_is_complete_and_unique() {
    assert_eq!(gc::ROUTES.len(), 22, "{:?}", gc::ROUTES);
    let mut seen = std::collections::BTreeSet::new();
    for (method, path) in gc::ROUTES {
        assert!(path.starts_with("/api/"), "{method} {path}");
        assert!(seen.insert((*method, *path)), "duplicate {method} {path}");
    }
    // Static paths mount before dynamic ones on every shared prefix.
    let paths: Vec<&str> = gc::ROUTES.iter().map(|(_, p)| *p).collect();
    let peers = paths.iter().position(|p| *p == "/api/v1/instance/peers/search").unwrap();
    assert!(paths.iter().all(|p| *p != "/api/v1/instance/peers/:id"));
    let _ = peers;
    let clear = paths.iter().position(|p| *p == "/api/v2/notifications/clear").unwrap();
    let group = paths.iter().position(|p| *p == "/api/v2/notifications/:group_key/accounts").unwrap();
    assert!(clear < group);
}

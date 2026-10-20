//! Phase 4 list contract tests: CRUD, membership, the exact validation
//! bodies, and the follow-cascade (Mastodon drops `list_accounts` rows with
//! their follow).

mod common;

use axum::http::StatusCode;
use serde_json::Value;

use birchtree::application::ports::{AccountStore, RelationshipStore};
use birchtree::domain::Id;

use common::{TestServer, form_request, get_request, json, send};

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

async fn register_app(srv: &TestServer, scopes: &str) -> Value {
    let body = format!(
        "client_name=Phase4Lists&redirect_uris={}&scopes={}",
        enc("https://client.example/cb"),
        enc(scopes),
    );
    let (status, _h, bytes) =
        send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

async fn app_token(srv: &TestServer, app: &Value, scopes: &str) -> String {
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

async fn register_account(srv: &TestServer, app: &Value, username: &str) -> String {
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

/// Every route through one user token covering both list scopes.
async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(
        srv,
        "read write follow profile read:accounts write:accounts read:lists write:lists",
    )
    .await;
    register_account(srv, &app, username).await
}

#[tokio::test]
async fn list_crud_round_trip_with_exact_keys() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "alice").await;

    // Create.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=Friends", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let created = json(&bytes);
    assert_eq!(
        created.as_object().unwrap().keys().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
        vec!["exclusive", "id", "replies_policy", "title"]
    );
    assert_eq!(created["title"], "Friends");
    assert_eq!(created["replies_policy"], "list");
    assert_eq!(created["exclusive"], false);
    let id = created["id"].as_str().unwrap().to_string();

    // Index shows it.
    let (status, _h, bytes) = send(&srv.router, get_request("/api/v1/lists", Some(&token), None)).await;
    assert_eq!(status, StatusCode::OK);
    let list = json(&bytes);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id.as_str());

    // Show.
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/lists/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["title"], "Friends");

    // Update (title + replies_policy + exclusive).
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/lists/{id}"),
            "title=Best+friends&replies_policy=followed&exclusive=true",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let updated = json(&bytes);
    assert_eq!(updated["title"], "Best friends");
    assert_eq!(updated["replies_policy"], "followed");
    assert_eq!(updated["exclusive"], true);

    // Delete -> {} 200, then 404.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("DELETE", &format!("/api/v1/lists/{id}"), "", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes), serde_json::json!({}));
    let (status, _h, _bytes) = send(
        &srv.router,
        get_request(&format!("/api/v1/lists/{id}"), Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_validation_bodies_match_mastodon() {
    let srv = TestServer::new(true);
    let token = user_token(&srv, "alice").await;

    // Blank title.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=+&", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: Title can't be blank"
    );

    // Title too long (>256).
    let long = "x".repeat(257);
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            "/api/v1/lists",
            &format!("title={long}"),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: Title is too long (maximum is 256 characters)"
    );

    // Bad replies_policy on update.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=OK", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = json(&bytes)["id"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "PUT",
            &format!("/api/v1/lists/{id}"),
            "replies_policy=sometimes",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: Replies policy is not included in the list"
    );

    // Per-account limit (50).
    for n in 1..=49 {
        let (status, _h, _bytes) = send(
            &srv.router,
            form_request(
                "POST",
                "/api/v1/lists",
                &format!("title=list{n}"),
                Some(&token),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=one+too+many", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: You have reached the maximum number of lists"
    );
}

#[tokio::test]
async fn other_users_lists_are_404() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let bob = user_token(&srv, "bob").await;

    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=Alice+list", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = json(&bytes)["id"].as_str().unwrap().to_string();

    for (method, path) in [
        ("GET", format!("/api/v1/lists/{id}")),
        ("PUT", format!("/api/v1/lists/{id}")),
        ("DELETE", format!("/api/v1/lists/{id}")),
        ("GET", format!("/api/v1/lists/{id}/accounts")),
    ] {
        let (status, _h, _bytes) = send(
            &srv.router,
            form_request(method, &path, "title=x", Some(&bob)),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn membership_requires_follow_and_cascades_on_unfollow() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let bob = user_token(&srv, "bob").await;

    // Alice's account id (bob's token to look it up publicly).
    let bob_account_id = {
        let (status, _h, bytes) = send(
            &srv.router,
            get_request("/api/v1/accounts/verify_credentials", Some(&bob), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        json(&bytes)["id"].as_str().unwrap().to_string()
    };
    let alice_account_id = {
        // Register a second account and resolve alice via relationships is
        // overkill: create the follow from bob's side instead. Bob follows
        // alice by handle through the store.
        let store = &*srv.store;
        let account = store
            .account_by_handle("alice", "example.social")
            .unwrap()
            .expect("alice account");
        account.id.to_string()
    };

    // Not following yet -> 422.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=Members", Some(&alice)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list_id = json(&bytes)["id"].as_str().unwrap().to_string();
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/lists/{list_id}/accounts"),
            &format!("account_ids[]={bob_account_id}"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(
        json(&bytes)["error"],
        "Validation failed: Account must be following"
    );

    // Unknown account id -> 404.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/lists/{list_id}/accounts"),
            "account_ids[]=999999999",
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json(&bytes)["error"], "Record not found");

    // The list OWNER must follow the member (Mastodon `set_follow`).
    let alice_id = Id::new(alice_account_id.parse::<u64>().unwrap());
    let bob_id = Id::new(bob_account_id.parse::<u64>().unwrap());
    srv.store
        .follow(
            alice_id,
            bob_id,
            birchtree::application::ports::FollowOptions {
                show_reblogs: true,
                notify: false,
                languages: None,
            },
            false,
            1_700_000_000_000,
        )
        .unwrap();

    // Add (idempotent), list members, remove.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/lists/{list_id}/accounts"),
            &format!("account_ids[]={bob_account_id}"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    let (_status, _h, _bytes) = send(
        &srv.router,
        form_request(
            "POST",
            &format!("/api/v1/lists/{list_id}/accounts"),
            &format!("account_ids[]={bob_account_id}"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK); // adding again is a no-op

    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/lists/{list_id}/accounts"),
            Some(&alice),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let members = json(&bytes);
    assert_eq!(members.as_array().unwrap().len(), 1);
    assert_eq!(members[0]["id"], bob_account_id.as_str());
    // Account DTO key set sanity (REST::AccountSerializer essentials).
    for key in ["id", "username", "acct", "display_name", "note", "avatar", "header"] {
        assert!(members[0].get(key).is_some(), "missing {key}");
    }

    // Unfollow -> membership cascades away (list_accounts FK cascade).
    srv.store.unfollow(alice_id, bob_id, 1_700_000_000_001).unwrap();
    let (status, _h, bytes) = send(
        &srv.router,
        get_request(
            &format!("/api/v1/lists/{list_id}/accounts"),
            Some(&alice),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes).as_array().unwrap().len(), 0);

    // Removing again is idempotent.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request(
            "DELETE",
            &format!("/api/v1/lists/{list_id}/accounts"),
            &format!("account_ids[]={bob_account_id}"),
            Some(&alice),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete members: {}", String::from_utf8_lossy(&bytes));
    assert_eq!(json(&bytes), serde_json::json!({}));
}

#[tokio::test]
async fn list_scopes_and_auth_are_enforced() {
    let srv = TestServer::new(true);
    let app = register_app(&srv, "read").await;
    let read_only = app_token(&srv, &app, "read").await;

    // Wrong scope on a write.
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=x", Some(&read_only)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json(&bytes)["error"], "This action is outside the authorized scopes");

    // No token.
    let (status, _h, _bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/lists", "title=x", None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

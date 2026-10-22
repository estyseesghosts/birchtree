//! Phase 7.2 search-engine contract: `GET /api/v2/search` across the three
//! status backends (`embedded` default / `database` interim / `off`).
//!
//! The tests drive the real store plus the real indexing step
//! (`jobs::search_index::apply`, the same function the post-commit worker
//! runs) and then query through `application::search::search_with_mode`.
//! Job execution is simulated by calling `apply` directly: the test servers
//! run no background worker, and what matters here is that the committed
//! state converges the derived index — the worker itself just calls the
//! same function after commit.

mod common;

use axum::http::StatusCode;

use birchtree::application::search::search_with_mode;
use birchtree::application::statuses::{UpdateStatusCommand, delete_status, update_status};
use birchtree::domain::Id;
use birchtree::jobs::search_index as jobs;
use birchtree::search::{EmbeddedIndex, SearchMode, backfill_batch};

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

async fn register_app(srv: &TestServer, scopes: &str) -> serde_json::Value {
    let body = format!(
        "client_name=Phase72Search&redirect_uris={}&scopes={}",
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

async fn user_token(srv: &TestServer, username: &str) -> String {
    let app = register_app(
        srv,
        "read write read:accounts write:accounts read:search read:statuses write:statuses",
    )
    .await;
    let app_tok = app_token(srv, &app, "write:accounts").await;
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

async fn viewer_id(srv: &TestServer, token: &str) -> Id {
    let (status, _h, bytes) = send(
        &srv.router,
        get_request("/api/v1/accounts/verify_credentials", Some(token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)["id"].as_str().unwrap().parse::<Id>().unwrap()
}

async fn post_status(
    srv: &TestServer,
    token: &str,
    text: &str,
    visibility: Option<&str>,
) -> serde_json::Value {
    let mut body = format!("status={}", enc(text));
    if let Some(visibility) = visibility {
        body.push_str(&format!("&visibility={visibility}"));
    }
    let (status, _h, bytes) = send(
        &srv.router,
        form_request("POST", "/api/v1/statuses", &body, Some(token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    json(&bytes)
}

fn status_id(created: &serde_json::Value) -> Id {
    created["id"].as_str().unwrap().parse::<Id>().unwrap()
}

/// Simulate the post-commit worker for one status: run the same `apply` step
/// the durable `SearchIndexStatus` job runs after its mutation commits.
fn index_now(srv: &TestServer, index: &EmbeddedIndex, id: Id) {
    jobs::apply(&*srv.store, index, &jobs::index_payload(id)).expect("index apply");
}

fn search_statuses(
    srv: &TestServer,
    mode: SearchMode,
    index: Option<&EmbeddedIndex>,
    viewer: Option<Id>,
    query: &str,
) -> Vec<serde_json::Value> {
    let results = search_with_mode(
        mode,
        &*srv.store,
        index,
        viewer,
        query,
        Some("statuses"),
        0,
        20,
        false,
        None,
        None,
        None,
    )
    .expect("search");
    // The mode dispatch must not disturb the account/hashtag paths; statuses
    // carry real ids here for the assertions below.
    let _ = &results.accounts;
    let _ = &results.hashtags;
    results
        .statuses
        .iter()
        .map(|s| serde_json::json!({"id": s.id.to_string()}))
        .collect()
}

// ---------------------------------------------------------------------------
// Parity fixtures per mode.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn embedded_database_find_and_off_is_empty() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let viewer = viewer_id(&srv, &alice).await;
    let created = post_status(&srv, &alice, "the quokka hosts a brunch", None).await;
    let id = status_id(&created);

    let index = EmbeddedIndex::new();
    index_now(&srv, &index, id);

    // Embedded (default) and database both find the public status; prefix
    // matching agrees too (`quok` finds `quokka` on both paths).
    for query in ["quokka", "quok"] {
        let embedded = search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), query);
        assert_eq!(embedded.len(), 1, "embedded finds {query}");
        let database = search_statuses(&srv, SearchMode::Database, Some(&index), Some(viewer), query);
        assert_eq!(database.len(), 1, "database finds {query}");
    }
    // Off matches upstream with no engine: no status hits anywhere.
    let off = search_statuses(&srv, SearchMode::Off, Some(&index), Some(viewer), "quokka");
    assert!(off.is_empty(), "off mode matches upstream emptiness");
    // Anonymous callers get no statuses in any mode.
    for mode in [SearchMode::Embedded, SearchMode::Database, SearchMode::Off] {
        let anon = search_statuses(&srv, mode, Some(&index), None, "quokka");
        assert!(anon.is_empty(), "anonymous empty in {mode:?}");
    }
}

#[tokio::test]
async fn accounts_path_is_mode_independent() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let viewer = viewer_id(&srv, &alice).await;
    let index = EmbeddedIndex::new();

    for mode in [SearchMode::Embedded, SearchMode::Database, SearchMode::Off] {
        let results = search_with_mode(
            mode,
            &*srv.store,
            Some(&index),
            Some(viewer),
            "ali",
            Some("accounts"),
            0,
            20,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(results.accounts.len(), 1, "accounts in {mode:?}");
        assert_eq!(results.accounts[0].username, "alice");
        assert!(results.statuses.is_empty());
    }
}

#[tokio::test]
async fn hashtags_stay_on_the_database_path() {
    // Decision: hashtag search does NOT go through the engine in any mode —
    // upstream serves hashtags from the database, and the engine indexes
    // status text, not the tag registry.
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let viewer = viewer_id(&srv, &alice).await;
    post_status(&srv, &alice, "hello #Rustaceans world", None).await;
    let index = EmbeddedIndex::new();

    for mode in [SearchMode::Embedded, SearchMode::Database, SearchMode::Off] {
        let results = search_with_mode(
            mode,
            &*srv.store,
            Some(&index),
            Some(viewer),
            "rust",
            Some("hashtags"),
            0,
            20,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(results.hashtags, vec!["rustaceans".to_string()], "hashtags in {mode:?}");
    }
}

#[tokio::test]
async fn unknown_type_is_empty_in_every_mode() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let viewer = viewer_id(&srv, &alice).await;
    let index = EmbeddedIndex::new();

    for mode in [SearchMode::Embedded, SearchMode::Database, SearchMode::Off] {
        let results = search_with_mode(
            mode,
            &*srv.store,
            Some(&index),
            Some(viewer),
            "alice",
            Some("gibberish"),
            0,
            20,
            false,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(results.accounts.is_empty(), "accounts in {mode:?}");
        assert!(results.statuses.is_empty(), "statuses in {mode:?}");
        assert!(results.hashtags.is_empty(), "hashtags in {mode:?}");
    }
}

// ---------------------------------------------------------------------------
// Edit / delete / visibility-change consistency.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_delete_visibility_change_converge() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let viewer = viewer_id(&srv, &alice).await;
    let index = EmbeddedIndex::new();

    let created = post_status(&srv, &alice, "aardvark alpha draft", None).await;
    let id = status_id(&created);
    index_now(&srv, &index, id);
    assert_eq!(
        search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), "alpha").len(),
        1
    );

    // Edit: the re-applied job converges the index to the current text.
    update_status(
        &*srv.store,
        common::T0 + 1,
        UpdateStatusCommand {
            account_id: viewer,
            status_id: id,
            text: Some("aardvark beta revised".to_string()),
            sensitive: None,
            spoiler_text: None,
            language: None,
            media_ids: None,
            poll: None,
            quote_approval_policy: None,
        },
        "off",
        "off",
        None,
        "off",
    )
    .unwrap();
    index_now(&srv, &index, id);
    assert!(
        search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), "alpha").is_empty(),
        "old text leaves the index on edit"
    );
    assert_eq!(
        search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), "beta").len(),
        1,
        "new text enters the index on edit"
    );

    // Delete: the tombstone de-indexes.
    delete_status(&*srv.store, viewer, id, common::T0 + 2).unwrap();
    index_now(&srv, &index, id);
    assert!(
        search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), "beta").is_empty(),
        "deleted status leaves the index"
    );
    assert!(index.is_empty());
}

#[tokio::test]
async fn non_indexable_visibilities_never_enter() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let viewer = viewer_id(&srv, &alice).await;
    let index = EmbeddedIndex::new();

    // Private and direct statuses are never indexable (Pleroma `indexable?`,
    // tightened): even applying the job for them indexes nothing.
    for visibility in ["private", "direct"] {
        let created = post_status(
            &srv,
            &alice,
            &format!("s3cret {visibility} words"),
            Some(visibility),
        )
        .await;
        index_now(&srv, &index, status_id(&created));
    }
    assert!(index.is_empty(), "private/direct never enter the index");
    assert!(
        search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), "s3cret").is_empty()
    );
    // Unlisted statuses ARE indexable (distributable, like public).
    let created = post_status(&srv, &alice, "s3cret unlisted words", Some("unlisted")).await;
    index_now(&srv, &index, status_id(&created));
    assert_eq!(
        search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), "unlisted").len(),
        1,
        "unlisted is indexable"
    );
}

// ---------------------------------------------------------------------------
// Backfill: resume cursor + crash recovery.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backfill_resume_and_crash_recovery() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    let viewer = viewer_id(&srv, &alice).await;
    for n in 0..5 {
        post_status(&srv, &alice, &format!("backfillquokka number {n}"), None).await;
    }

    // Batched run with a small batch: the first batch is not done and hands
    // back a resume cursor.
    let index = EmbeddedIndex::new();
    let first = backfill_batch(&*srv.store, &index, None, 2).unwrap();
    assert!(!first.done, "five rows do not fit in one batch of two");
    assert_eq!(first.indexed, 2);
    let cursor = first.next_cursor.expect("resume cursor");
    // The cursor survives a job-payload round trip (this is what the chained
    // `SearchBackfill` job carries across a crash between batches).
    let payload = jobs::backfill_payload(Some(cursor), 2);
    let (parsed_cursor, parsed_batch) = jobs::parse_backfill_payload(&payload).unwrap();
    assert_eq!(parsed_cursor, Some(cursor));
    assert_eq!(parsed_batch, 2);

    // Retrying the same batch is idempotent: same cursor, no duplicates.
    let retry = backfill_batch(&*srv.store, &index, None, 2).unwrap();
    assert_eq!(retry.next_cursor, Some(cursor));
    assert_eq!(index.len(), 2, "re-indexing overwrites, never duplicates");

    // Resume from the cursor to completion.
    let mut cursor = Some(cursor);
    loop {
        let progress = backfill_batch(&*srv.store, &index, cursor, 2).unwrap();
        cursor = progress.next_cursor;
        if progress.done {
            break;
        }
    }
    assert_eq!(index.len(), 5, "backfill covers the corpus");
    assert_eq!(
        search_statuses(&srv, SearchMode::Embedded, Some(&index), Some(viewer), "backfillquokka")
            .len(),
        5
    );

    // Crash recovery: the process-wide index is in-memory derived state, so a
    // restart wipes it — re-seeding from `None` rebuilds everything (the
    // chain cursor paces batching within a run, not across a wipe).
    let wiped = EmbeddedIndex::new();
    let mut cursor = None;
    loop {
        let progress = backfill_batch(&*srv.store, &wiped, cursor, 2).unwrap();
        cursor = progress.next_cursor;
        if progress.done {
            break;
        }
    }
    assert_eq!(wiped.len(), 5, "restart from None rebuilds after a wipe");
}

// ---------------------------------------------------------------------------
// Job chaining contract (what the worker does with `run_backfill`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backfill_job_chains_until_done() {
    let srv = TestServer::new(true);
    let alice = user_token(&srv, "alice").await;
    for n in 0..3 {
        post_status(&srv, &alice, &format!("chainquokka {n}"), None).await;
    }
    let index = EmbeddedIndex::new();

    // One step with batch 2 of 3 rows: not done, and it hands the follow-up
    // payload to the enqueue callback (the handler persists it as the next
    // `SearchBackfill` job).
    let mut queued: Vec<(serde_json::Value, u64)> = Vec::new();
    let done = jobs::run_backfill(
        &*srv.store,
        &index,
        &jobs::backfill_payload(None, 2),
        common::T0,
        &mut |payload, due| {
            queued.push((payload, due));
            Ok(())
        },
    )
    .unwrap();
    assert!(!done);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].1, common::T0, "follow-up is due immediately");
    let (cursor, batch) = jobs::parse_backfill_payload(&queued[0].0).unwrap();
    assert!(cursor.is_some());
    assert_eq!(batch, 2);

    // The follow-up finishes the run and queues nothing further.
    let mut queued: Vec<(serde_json::Value, u64)> = Vec::new();
    let done = jobs::run_backfill(
        &*srv.store,
        &index,
        &jobs::backfill_payload(cursor, 2),
        common::T0,
        &mut |payload, due| {
            queued.push((payload, due));
            Ok(())
        },
    )
    .unwrap();
    assert!(done);
    assert!(queued.is_empty());
    assert_eq!(index.len(), 3);
}

//! Gap-closing round 2, Subagent D: D38 (async account-deletion worker +
//! federated `Delete` distribution) and D40 (trend-score pipeline,
//! link-preview crawler, tag history + follows; locale ordering-only).
//!
//! Store-level tests use direct `Store` calls for determinism; two wire
//! tests go through the real router for the tag payload shape and the
//! `?locale=` ordering contract.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use birchtree::application::ports::{
    AccountStatusesQuery, AccountStore, AdminStore, JobQueue, NewStatus, StatusStore, Store,
    TrendStore,
};
use birchtree::application::trends;
use birchtree::domain::{Id, Job, JobKind, Visibility};
use birchtree::federation::delivery::DeliveryError;
use birchtree::federation::net::{self, SafeHttpConfig};
use birchtree::federation::policy::Cidr;
use birchtree::federation::preview;
use birchtree::jobs::account_deletion;
use birchtree::persistence::redb::RedbStore;
use common::{mk_account, open_store};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn new_status(account_id: Id, text: &str, tags: Vec<String>, language: Option<String>) -> NewStatus {
    NewStatus {
        remote: None,
        account_id,
        uri_base: "https://example.social/users/test/statuses".to_string(),
        url_base: "https://example.social/@test".to_string(),
        source_text: text.to_string(),
        content_html: format!("<p>{text}</p>"),
        visibility: Visibility::Public,
        language,
        tags,
        ..Default::default()
    }
}

fn blobs_at(dir: &std::path::Path) -> birchtree::media::blob_store::FsBlobStore {
    birchtree::media::blob_store::FsBlobStore::open(dir.join("blobs")).expect("blob store")
}

/// Handle one claimed job the way the worker does.
fn handle(store: &RedbStore, dir: &std::path::Path, job: &Job) {
    let mail = birchtree::mail::LoggingMailSender;
    let blobs = blobs_at(dir);
    birchtree::jobs::handlers::handle(
        job,
        store,
        &mail,
        &blobs,
        &birchtree::application::ports::NoStreaming,
    )
    .expect("handle job");
    store.complete(job.id).expect("complete");
}

/// Claim and handle only `AccountDeletionBatch` jobs (other jobs — search
/// indexing, fan-out — stay pending, as in production where the worker
/// drains every kind). The claim window is wide on purpose: the follow-up
/// batch is newer than the hundreds of per-status jobs around it.
fn drain_deletion_batch(store: &RedbStore, dir: &std::path::Path) -> bool {
    let jobs = store.claim_due(now_ms(), 10_000).expect("claim");
    let mut ran = false;
    for job in jobs {
        if job.kind == JobKind::AccountDeletionBatch {
            handle(store, dir, &job);
            ran = true;
        }
    }
    ran
}

fn pending_deletion_batches(store: &RedbStore) -> Vec<Job> {
    store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|job| {
            job.kind == JobKind::AccountDeletionBatch && !job.state.is_terminal()
        })
        .collect()
}

fn federation_events(store: &RedbStore) -> Vec<Job> {
    store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|job| job.kind == JobKind::FederationEvent && !job.state.is_terminal())
        .collect()
}

// ----- D38 ------------------------------------------------------------------

#[test]
fn deletion_worker_removes_statuses_across_batches_and_restarts() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 120 statuses need 3 batches of 50 (50 + 50 + 20).
    const TOTAL: usize = 120;
    let mut status_ids = Vec::new();
    let (admin_id, alice_id, wrapper_id) = {
        let store = open_store(dir.path());
        let admin = mk_account(&store, "admin");
        let alice = mk_account(&store, "alice");
        let bob = mk_account(&store, "bob");
        for i in 0..TOTAL {
            let created = store
                .create_status(
                    new_status(alice.id, &format!("doomed {i}"), Vec::new(), None),
                    now_ms(),
                )
                .expect("create")
                .status;
            status_ids.push(created.id);
        }
        // Another account's reblog of a doomed status must go with it.
        let wrapper = store
            .reblog_status(bob.id, status_ids[0], Visibility::Public, now_ms())
            .expect("reblog");
        let now = now_ms();
        assert!(
            store
                .admin_delete_account(admin.id, alice.id, "destroy", now)
                .expect("delete account")
        );
        // Immediate suspend + disable, exactly like before the worker.
        let alice_row = store.account_by_id(alice.id).expect("read").expect("alice");
        assert!(alice_row.suspended);
        let user = store.user_by_account_id(alice.id).expect("read").expect("user");
        assert!(user.disabled);
        // One deletion batch plus the federated `Delete` of the actor commit
        // in the same transaction as the suspension. (Fixture setup already
        // enqueued `create`/`announce` events, so the actor event is
        // counted by name, not by total.)
        assert_eq!(pending_deletion_batches(&store).len(), 1);
        let actors = federation_events(&store)
            .iter()
            .filter(|job| {
                job.payload.get("event").and_then(|v| v.as_str()) == Some("delete_actor")
            })
            .count();
        assert_eq!(actors, 1);
        (admin.id, alice.id, wrapper.id)
    };

    // Batch 1 (50 tombstones), then a "restart": drop and reopen the store.
    for _ in 0..2 {
        {
            let store = open_store(dir.path());
            assert!(drain_deletion_batch(&store, dir.path()));
            assert_eq!(pending_deletion_batches(&store).len(), 1);
        }
    }
    // Batch 3 (the short tail) ends the chain.
    {
        let store = open_store(dir.path());
        assert!(drain_deletion_batch(&store, dir.path()));
        assert!(pending_deletion_batches(&store).is_empty());
        assert!(!drain_deletion_batch(&store, dir.path()));

        // Every status is actually gone (tombstoned), not just hidden.
        let live = store
            .statuses_by_account(
                alice_id,
                AccountStatusesQuery {
                    max_id: None,
                    since_id: None,
                    min_id: None,
                    limit: 500,
                    pinned_only: false,
                    exclude_replies: false,
                    exclude_reblogs: false,
                },
            )
            .expect("scan");
        assert!(live.is_empty());
        for id in &status_ids {
            let row = store.status_by_id(*id).expect("read").expect("row retained");
            assert!(row.is_deleted(), "status {id} tombstoned");
        }
        // The reblog wrapper cascaded with its original.
        let wrapper = store
            .status_by_id(wrapper_id)
            .expect("read")
            .expect("wrapper retained");
        assert!(wrapper.is_deleted());

        // One federated `Delete` per tombstoned status, plus the actor's.
        // (Fixture setup also enqueued `create`/`announce` events, so the
        // counts below filter by event name instead of asserting a total.)
        let events = federation_events(&store);
        let deletes = events
            .iter()
            .filter(|job| {
                job.payload.get("event").and_then(|v| v.as_str()) == Some("delete")
            })
            .count();
        assert_eq!(deletes, TOTAL, "one Delete event per status");
        let actors = events
            .iter()
            .filter(|job| {
                job.payload.get("event").and_then(|v| v.as_str()) == Some("delete_actor")
            })
            .count();
        assert_eq!(actors, 1, "one Delete event for the actor");
        let _ = admin_id;
    }
}

#[test]
fn deletion_batch_is_idempotent_and_ends_the_chain() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let admin = mk_account(&store, "admin");
    let alice = mk_account(&store, "alice");
    let mut status_ids = Vec::new();
    for i in 0..3 {
        status_ids.push(
            store
                .create_status(new_status(alice.id, &format!("gone {i}"), Vec::new(), None), now_ms())
                .expect("create")
                .status
                .id,
        );
    }
    store
        .admin_delete_account(admin.id, alice.id, "destroy", now_ms())
        .expect("delete");

    // A short page ends the chain: one batch, no follow-up.
    assert!(drain_deletion_batch(&store, dir.path()));
    assert!(pending_deletion_batches(&store).is_empty());

    // Replaying the first batch (a lease-retry after a crash) deletes
    // nothing new and enqueues nothing: `NotFound` is the desired end state.
    let replayed = account_deletion::run_batch(
        &store,
        &birchtree::application::ports::NoStreaming,
        &account_deletion::payload(alice.id, None),
        now_ms(),
    )
    .expect("replay");
    assert_eq!(replayed, 0);
    assert!(pending_deletion_batches(&store).is_empty());

    // Deleting an already-gone status answers `NotFound` (skipped, not fatal).
    assert!(matches!(
        store.delete_status(status_ids[0], alice.id, now_ms()),
        Err(birchtree::domain::Error::NotFound(_))
    ));
}

#[test]
fn delete_actor_shape_is_a_person_tombstone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let alice = mk_account(&store, "alice");
    let alice_row = store.account_by_id(alice.id).expect("read").expect("alice");
    let body = birchtree::activitypub::outgoing::delete_actor(&alice_row);
    assert_eq!(body.get("type").and_then(|v| v.as_str()), Some("Delete"));
    assert_eq!(
        body.get("actor").and_then(|v| v.as_str()),
        Some(alice_row.actor_uri.as_str())
    );
    let object = body.get("object").expect("object");
    assert_eq!(object.get("type").and_then(|v| v.as_str()), Some("Tombstone"));
    assert_eq!(
        object.get("formerType").and_then(|v| v.as_str()),
        Some("Person")
    );
    assert_eq!(
        object.get("id").and_then(|v| v.as_str()),
        Some(alice_row.actor_uri.as_str())
    );
}

// ----- D40: score pipeline ----------------------------------------------------

#[test]
fn trend_score_decays_with_age_and_grows_with_uses() {
    let now = now_ms();
    let day = 86_400_000u64;
    // More uses win at the same age (not recency).
    assert!(trends::trend_score(6, Some(now), now) > trends::trend_score(2, Some(now), now));
    // But a fresh few beat a stale many (not raw counts either).
    assert!(
        trends::trend_score(2, Some(now), now) > trends::trend_score(6, Some(now - 30 * day), now)
    );
    // No recency information: the raw count stands.
    assert_eq!(trends::trend_score(4, None, now), 4.0);
    // Older is never better, all else equal.
    assert!(
        trends::trend_score(3, Some(now - day), now) > trends::trend_score(3, Some(now - 2 * day), now)
    );
}

#[test]
fn approved_tags_order_by_score_not_recency() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let admin = mk_account(&store, "admin");
    let alice = mk_account(&store, "alice");
    let now = now_ms();
    for i in 0..6 {
        store
            .create_status(
                new_status(alice.id, &format!("steady {i} #steady"), vec!["steady".into()], None),
                now,
            )
            .expect("create");
    }
    for i in 0..2 {
        store
            .create_status(
                new_status(alice.id, &format!("fresh {i} #fresh"), vec!["fresh".into()], None),
                now,
            )
            .expect("create");
    }
    for name in ["steady", "fresh"] {
        let flag = store.ensure_trend_tag(name, now).expect("ensure");
        store
            .review_trend_tag(flag.id, true, admin.id, now)
            .expect("approve");
    }
    let approved = trends::approved_tags(&store, now).expect("approved");
    let names: Vec<&str> = approved.iter().map(|flag| flag.name.as_str()).collect();
    // Six uses outrank two, even though #fresh is newer-or-equal.
    assert_eq!(names, vec!["steady", "fresh"]);
}

#[test]
fn status_trends_order_by_engagement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let admin = mk_account(&store, "admin");
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let carol = mk_account(&store, "carol");
    let now = now_ms();
    let quiet = store
        .create_status(new_status(alice.id, "quiet post", Vec::new(), None), now)
        .expect("create")
        .status;
    let loud = store
        .create_status(new_status(alice.id, "loud post", Vec::new(), None), now)
        .expect("create")
        .status;
    store.favourite_status(bob.id, loud.id, now).expect("fav");
    store.favourite_status(carol.id, loud.id, now).expect("fav");
    for status in [&quiet, &loud] {
        store
            .review_trend_status(status.id, true, admin.id, now)
            .expect("approve");
    }
    let approved = trends::approved_statuses(&store, None, now).expect("approved");
    let ids: Vec<Id> = approved.iter().map(|status| status.id).collect();
    assert_eq!(ids, vec![loud.id, quiet.id]);
}

#[test]
fn link_trends_order_by_shares_and_serve_crawled_cards() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let admin = mk_account(&store, "admin");
    let alice = mk_account(&store, "alice");
    let now = now_ms();
    for i in 0..3 {
        store
            .create_status(
                new_status(alice.id, &format!("read https://example.com/popular ({i})"), Vec::new(), None),
                now,
            )
            .expect("create");
    }
    store
        .create_status(
            new_status(alice.id, "read https://example.com/quiet", Vec::new(), None),
            now,
        )
        .expect("create");
    // Materializing the candidates enqueues one bounded crawl job per URL.
    let candidates = trends::link_candidates(&store, now).expect("candidates");
    assert_eq!(candidates.len(), 2);
    let fetches = store
        .all_jobs()
        .expect("jobs")
        .into_iter()
        .filter(|job| job.kind == JobKind::PreviewCardFetch && !job.state.is_terminal())
        .count();
    assert_eq!(fetches, 2);
    for link in &candidates {
        store
            .review_trend_link(link.id, true, admin.id, now)
            .expect("approve");
    }
    // The crawler's write lands verbatim in the public card (no fabrication
    // anywhere else: the quiet link still serves nulls).
    let popular = candidates
        .iter()
        .find(|link| link.url == "https://example.com/popular")
        .expect("popular");
    store
        .update_trend_link_card(
            popular.id,
            Some("Popular Title".into()),
            Some("A popular page".into()),
            Some("Example".into()),
            Some("https://example.com".into()),
            Some("https://example.com/i.png".into()),
            now,
        )
        .expect("card update");
    let approved = trends::approved_links(&store, now).expect("approved");
    let urls: Vec<&str> = approved.iter().map(|link| link.url.as_str()).collect();
    assert_eq!(urls, vec!["https://example.com/popular", "https://example.com/quiet"]);
    let card = birchtree::api::mastodon::dto::trend_link_json(&approved[0]);
    assert_eq!(card["title"], serde_json::json!("Popular Title"));
    assert_eq!(card["image"], serde_json::json!("https://example.com/i.png"));
    assert_eq!(
        birchtree::api::mastodon::dto::trend_link_json(&approved[1])["title"],
        serde_json::Value::Null
    );
}

#[test]
fn tag_history_buckets_are_real_and_cover_seven_days() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let alice = mk_account(&store, "alice");
    let bob = mk_account(&store, "bob");
    let now = now_ms();
    for i in 0..2 {
        store
            .create_status(
                new_status(alice.id, &format!("alice {i} #hist"), vec!["hist".into()], None),
                now,
            )
            .expect("create");
    }
    store
        .create_status(
            new_status(bob.id, "bob #hist", vec!["hist".into()], None),
            now,
        )
        .expect("create");
    let history = trends::tag_history(&store, "hist", now).expect("history");
    assert_eq!(history.len(), 7);
    assert_eq!(history.iter().map(|day| day.uses).sum::<u64>(), 3);
    // Newest first, one day apart, all in the past-or-today.
    for pair in history.windows(2) {
        assert_eq!(pair[0].day_sec - pair[1].day_sec, 86_400);
    }
    // Everything posted today lands in today's bucket (2 authors).
    assert_eq!(history[0].uses, 3);
    assert_eq!(history[0].accounts, 2);
    // An unseen tag still yields the seven-day zero shape.
    let empty = trends::tag_history(&store, "never-used", now).expect("empty");
    assert_eq!(empty.len(), 7);
    assert!(empty.iter().all(|day| day.uses == 0 && day.accounts == 0));
}

// ----- D40: crawler -------------------------------------------------------------

#[test]
fn preview_parse_extracts_opengraph_and_falls_back() {
    let page = url::Url::parse("https://example.com/article").expect("url");
    let html = r#"<!doctype html><html><head>
<meta property="og:title" content="Card Title">
<meta property="og:description" content="Card description.">
<meta property="og:site_name" content="Example">
<meta property="og:image" content="/i/card.png">
</head><body><title>ignored</title></body></html>"#;
    let fields = preview::parse_card(html, &page);
    assert_eq!(fields.title.as_deref(), Some("Card Title"));
    assert_eq!(fields.description.as_deref(), Some("Card description."));
    assert_eq!(fields.provider_name.as_deref(), Some("Example"));
    assert_eq!(fields.provider_url.as_deref(), Some("https://example.com"));
    assert_eq!(fields.image_url.as_deref(), Some("https://example.com/i/card.png"));

    // Fallbacks: `<title>` and `meta[name=description]`; data-URI images refused.
    let html = r#"<html><head><title>Fallback Title</title>
<meta name="description" content="Fallback description.">
<meta property="og:image" content="data:image/png;base64,AAA">
</head></html>"#;
    let fields = preview::parse_card(html, &page);
    assert_eq!(fields.title.as_deref(), Some("Fallback Title"));
    assert_eq!(fields.description.as_deref(), Some("Fallback description."));
    assert_eq!(fields.image_url, None);

    // No tags at all: every field stays `None` (never fabricated).
    assert_eq!(
        preview::parse_card("<html><head></head><body>hi</body></html>", &page),
        preview::CardFields::default()
    );
}

fn preview_job(link_id: Id, url: &str) -> Job {
    Job::new(
        Id::new(424_242),
        JobKind::PreviewCardFetch,
        serde_json::json!({"link_id": link_id.value(), "url": url}),
        now_ms(),
        now_ms(),
    )
}

/// A canned HTTP origin on loopback: `/card` (OpenGraph HTML), `/missing`
/// (404), `/binary` (non-HTML).
async fn loopback_origin() -> (tokio::task::JoinHandle<()>, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 4096];
                let Ok(n) = socket.read(&mut buf).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let response = match path.as_str() {
                    "/card" => {
                        let body = "<html><head>\
                            <meta property=\"og:title\" content=\"Loopback Title\">\
                            <meta property=\"og:description\" content=\"Loopback description.\">\
                            <meta property=\"og:site_name\" content=\"Loopback\">\
                            <meta property=\"og:image\" content=\"/i/loop.png\">\
                            </head><body>hi</body></html>";
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    "/binary" => "HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: 4\r\nconnection: close\r\n\r\nPNG!".to_string(),
                    _ => "HTTP/1.1 404 Not Found\r\ncontent-type: text/html\r\ncontent-length: 9\r\nconnection: close\r\n\r\nnot found".to_string(),
                };
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (handle, port)
}

fn loopback_client() -> (reqwest::Client, SafeHttpConfig) {
    let cfg = SafeHttpConfig {
        allow_http: true,
        allowlist: vec![Cidr::parse("127.0.0.1/32").expect("cidr")],
        ..SafeHttpConfig::default()
    };
    let client = net::client(&cfg).expect("client");
    (client, cfg)
}

#[tokio::test]
async fn preview_fetch_stores_card_and_maps_failures() {
    let (handle, port) = loopback_origin().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let (client, cfg) = loopback_client();
    let now = now_ms();

    // Success: the card fields land verbatim; a second run is a no-op.
    let card_url = format!("http://127.0.0.1:{port}/card");
    let link = store.ensure_trend_links(&[card_url.clone()], now).expect("ensure")[0].clone();
    preview::fetch(&store, &client, &cfg, &preview_job(link.id, &card_url))
        .await
        .expect("fetch");
    let row = store.trend_link_by_id(link.id).expect("read").expect("row");
    assert_eq!(row.title.as_deref(), Some("Loopback Title"));
    assert_eq!(row.description.as_deref(), Some("Loopback description."));
    assert_eq!(row.provider_name.as_deref(), Some("Loopback"));
    assert_eq!(
        row.image_url.as_deref(),
        Some(format!("http://127.0.0.1:{port}/i/loop.png").as_str())
    );
    preview::fetch(&store, &client, &cfg, &preview_job(link.id, &card_url))
        .await
        .expect("idempotent refetch");

    // Terminal failures quarantine (no retry loop): 404, non-HTML, refused URL.
    let missing_url = format!("http://127.0.0.1:{port}/missing");
    let missing = store.ensure_trend_links(&[missing_url.clone()], now).expect("ensure")[0].clone();
    assert!(matches!(
        preview::fetch(&store, &client, &cfg, &preview_job(missing.id, &missing_url)).await,
        Err(DeliveryError::Quarantine(_))
    ));
    let binary_url = format!("http://127.0.0.1:{port}/binary");
    let binary = store.ensure_trend_links(&[binary_url.clone()], now).expect("ensure")[0].clone();
    assert!(matches!(
        preview::fetch(&store, &client, &cfg, &preview_job(binary.id, &binary_url)).await,
        Err(DeliveryError::Quarantine(_))
    ));
    // Without the loopback allowlist the SSRF policy refuses before
    // connect. A distinct URL is used so the row is fresh: reusing the
    // crawled URL above would hit the same row, which short-circuits to
    // success by design. (The refusal happens before connect, so the
    // server's 404 for the unknown path is never reached.)
    let refused_url = format!("http://127.0.0.1:{port}/refused");
    let refused = store.ensure_trend_links(&[refused_url.clone()], now).expect("ensure")[0].clone();
    assert!(matches!(
        preview::fetch(&store, &client, &SafeHttpConfig::default(), &preview_job(refused.id, &refused_url)).await,
        Err(DeliveryError::Quarantine(_))
    ));
    // Unknown row: terminal.
    assert!(matches!(
        preview::fetch(
            &store,
            &client,
            &cfg,
            &preview_job(Id::new(987_654_321), "http://127.0.0.1:9/none"),
        )
        .await,
        Err(DeliveryError::Quarantine(_))
    ));
    handle.abort();
}

// ----- D40: wire -----------------------------------------------------------------

/// Minimal HTTP harness: register an app + admin + user through the router.
struct Wire {
    srv: common::TestServer,
    admin_token: String,
    user_token: String,
}

async fn wire_setup() -> Wire {
    use common::{form_request, json, send, TEST_PUBLIC_URL};
    let _ = TEST_PUBLIC_URL;
    let srv = common::TestServer::new(true);
    let enc = |value: &str| {
        let mut out = String::new();
        for b in value.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    let scopes = "read write follow read:accounts write:accounts read:statuses write:statuses admin:read admin:write";
    let body = format!("client_name=W&redirect_uris={}&scopes={}", enc("https://client.example/cb"), enc(scopes));
    let (status, _, bytes) = send(&srv.router, form_request("POST", "/api/v1/apps", &body, None)).await;
    assert_eq!(status, StatusCode::OK);
    let app = json(&bytes);
    let app_token = |scope: &str| {
        let body = format!(
            "grant_type=client_credentials&client_id={}&client_secret={}&scope={}",
            enc(app["client_id"].as_str().unwrap()),
            enc(app["client_secret"].as_str().unwrap()),
            enc(scope),
        );
        let router = srv.router.clone();
        async move {
            let (status, _, bytes) = send(&router, form_request("POST", "/oauth/token", &body, None)).await;
            assert_eq!(status, StatusCode::OK);
            json(&bytes)["access_token"].as_str().unwrap().to_string()
        }
    };
    let register = |username: &str, router: &axum::Router, token: String| {
        let body = format!("username={username}&email={username}%40example.com&password=password123&agreement=true");
        let router = router.clone();
        async move {
            let (status, _, bytes) = send(&router, form_request("POST", "/api/v1/accounts", &body, Some(&token))).await;
            assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
            json(&bytes)["access_token"].as_str().unwrap().to_string()
        }
    };
    let app_tok = app_token("write:accounts").await;
    let admin_token = register("root", &srv.router, app_tok.clone()).await;
    let user_token = register("alice", &srv.router, app_tok).await;
    let store: &dyn Store = &*srv.store;
    let root = store.account_by_handle("root", "example.social").unwrap().expect("root");
    store
        .set_user_role(root.id, birchtree::domain::UserRole::Admin)
        .unwrap();
    Wire { srv, admin_token, user_token }
}

#[tokio::test]
async fn trend_tag_wire_has_history_and_real_relationships() {
    use common::{form_request, get_request, json, send};
    let w = wire_setup().await;
    // Post through the application path so tags normalize like production.
    let body = "status=hello%20%23wired";
    let (status, _, bytes) = send(
        &w.srv.router,
        form_request("POST", "/api/v1/statuses", body, Some(&w.user_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    // Admin queue names the numeric id; approve it.
    let (status, _, bytes) = send(
        &w.srv.router,
        get_request("/api/v1/admin/trends/tags", Some(&w.admin_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let queue = json(&bytes);
    let id = queue[0]["id"].as_str().unwrap().to_string();
    let (status, _, _) = send(
        &w.srv.router,
        form_request("POST", &format!("/api/v1/admin/trends/tags/{id}/approve"), "", Some(&w.admin_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Follow the tag, then read the public trend authenticated.
    let (status, _, _) = send(
        &w.srv.router,
        form_request("POST", "/api/v1/tags/wired/follow", "", Some(&w.user_token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, bytes) = send(
        &w.srv.router,
        get_request("/api/v1/trends/tags", Some(&w.user_token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let payload = json(&bytes);
    assert_eq!(payload.as_array().unwrap().len(), 1);
    assert_eq!(payload[0]["name"], serde_json::json!("wired"));
    assert_eq!(payload[0]["history"].as_array().unwrap().len(), 7);
    assert_eq!(payload[0]["following"], serde_json::json!(true));
    assert_eq!(payload[0]["featuring"], serde_json::json!(false));
    // Anonymous readers get history but no viewer keys.
    let (status, _, bytes) = send(&w.srv.router, get_request("/api/v1/trends/tags", None, None)).await;
    assert_eq!(status, StatusCode::OK);
    let payload = json(&bytes);
    assert_eq!(payload[0]["history"].as_array().unwrap().len(), 7);
    assert!(payload[0].get("following").is_none());
}

#[tokio::test]
async fn trend_status_locale_orders_without_filtering() {
    use common::{get_request, json, send};
    let w = wire_setup().await;
    let store: Arc<RedbStore> = w.srv.store.clone();
    let alice = store.account_by_handle("alice", "example.social").unwrap().expect("alice");
    let now = now_ms();
    // Two approved statuses, one French-tagged; scores are ~equal.
    let english = store
        .create_status(new_status(alice.id, "hello world", Vec::new(), Some("en".into())), now)
        .expect("create")
        .status;
    let french = store
        .create_status(new_status(alice.id, "bonjour monde", Vec::new(), Some("fr".into())), now)
        .expect("create")
        .status;
    let root = store.account_by_handle("root", "example.social").unwrap().expect("root");
    for status in [&english, &french] {
        store.review_trend_status(status.id, true, root.id, now).expect("approve");
    }
    let plain: Vec<String> = {
        let (status, _, bytes) = send(
            &w.srv.router,
            get_request("/api/v1/trends/statuses", None, None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        json(&bytes).as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(plain.len(), 2);
    // `?locale=fr` pulls the French status first; nothing is filtered out.
    let (status, _, bytes) = send(
        &w.srv.router,
        get_request("/api/v1/trends/statuses?locale=fr", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ordered: Vec<String> = json(&bytes).as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0], french.id.to_string());
    // `?locale=` names are matched case-insensitively and blank means off.
    let (status, _, bytes) = send(
        &w.srv.router,
        get_request("/api/v1/trends/statuses?locale=", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blank: Vec<String> = json(&bytes).as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(blank, plain);
}

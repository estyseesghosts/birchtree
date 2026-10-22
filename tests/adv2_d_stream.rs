//! Wave 2 D2 stage 1: audio/video fetches complete as not-cached.
//!
//! The job succeeds (no quarantine error in dashboards); the row records
//! the non-persist reason and clients play from `remote_url` (D1).

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::json;

use birchtree::activitypub::FedContext;
use birchtree::application::context::Context;
use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{MediaStore, NewMedia};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator};
use birchtree::federation::media;
use birchtree::federation::net::SafeHttpConfig;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};

use common::{TEST_PUBLIC_URL, mk_account, temp_store};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn loopback_policy() -> SafeHttpConfig {
    SafeHttpConfig {
        allow_http: true,
        allowlist: ["127.0.0.0/8", "::1/128"]
            .iter()
            .map(|c| birchtree::federation::policy::Cidr::parse(c).unwrap())
            .collect(),
        ..SafeHttpConfig::default()
    }
}

fn context(store: &std::sync::Arc<RedbStore>, dir: &std::path::Path) -> std::sync::Arc<Context> {
    let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(SystemClock);
    let ids: std::sync::Arc<dyn IdGenerator> =
        std::sync::Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = std::sync::Arc::new(
        birchtree::media::blob_store::FsBlobStore::open(dir.join("media")).expect("blobs"),
    );
    let server = std::sync::Arc::new(birchtree::api::mastodon::ServerInfo::from_config(&Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    }));
    let streams = Hub::new(store.clone(), clock.clone(), server, Limits::default());
    std::sync::Arc::new(Context::new(
        clock,
        ids,
        store.clone(),
        blobs,
        std::sync::Arc::new(streams),
    ))
}

async fn serve_bytes(path: &str, bytes: Vec<u8>) -> String {
    let app = Router::new().route(
        path,
        get(move || {
            let bytes = bytes.clone();
            async move { (StatusCode::OK, bytes) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://127.0.0.1:{port}{path}")
}

#[tokio::test]
async fn video_fetch_completes_successfully_as_not_cached() {
    let (dir, store) = temp_store();
    let ctx_store = std::sync::Arc::new(store);
    let federation: activitypub_federation::config::FederationConfig<FedContext> =
        common::federation_config_with(
            context(&ctx_store, dir.path()),
            TEST_PUBLIC_URL,
            &loopback_policy(),
            true,
        );
    let mut mp4 = vec![0u8, 0, 0, 0x18];
    mp4.extend_from_slice(b"ftypisom");
    mp4.extend_from_slice(&[0, 0, 0, 0]);
    mp4.extend_from_slice(b"isomiso2");
    mp4.resize(64 * 1024, 7);
    let url = serve_bytes("/v.mp4", mp4).await;
    let now = now_ms();
    let account = mk_account(&ctx_store, &format!("vid-{}", now % 100_000));
    let attachment = ctx_store
        .create_media(NewMedia {
            account_id: account.id,
            r#type: "unknown".to_string(),
            description: String::new(),
            license: None,
        })
        .expect("media");
    ctx_store
        .set_remote_media_source(attachment.id, &url)
        .expect("source");
    let job = Job::new(
        Id::new(now),
        JobKind::FetchRemoteMedia,
        json!({"media_id": attachment.id.value()}),
        now,
        now,
    );
    let outcome = media::fetch(&federation, &job).await;
    assert!(matches!(outcome, Ok(())), "completes, got {outcome:?}");
    let row = ctx_store
        .media_by_id(attachment.id)
        .expect("row")
        .expect("media");
    assert!(row.file_key.is_empty(), "nothing staged");
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("never cached")),
        "non-persist reason recorded: {:?}",
        row.error
    );
}

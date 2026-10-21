//! Work package 8.1: remote media is dereferenced with the instance actor's HTTP
//! signature (the crate's own signer), including every redirect hop, while the
//! URL policy still applies to each hop.
//!
//! Synthetic hosts only: the "remote" is a loopback server the test starts.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use activitypub_federation::config::FederationConfig;
use activitypub_federation::http_signatures::verify_signature;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::IntoResponse;
use axum::routing::get;

use birchtree::activitypub::instance::{self, InstanceActor};
use birchtree::activitypub::{FedContext, build_config};
use birchtree::application::context::Context;
use birchtree::application::federation_ports::FederationContentStore;
use birchtree::application::ports::{MediaStore, NewMedia};
use birchtree::config::Config;
use birchtree::domain::{Clock, Id, IdGenerator, Job, JobKind, SystemClock, TimestampIdGenerator};
use birchtree::federation::delivery::DeliveryError;
use birchtree::federation::media;
use birchtree::federation::net::SafeHttpConfig;
use birchtree::federation::policy::Cidr;
use birchtree::persistence::redb::RedbStore;
use birchtree::streaming::{Hub, Limits};
use common::{TEST_PUBLIC_URL, mk_account, temp_store};

/// What the fake remote saw: per request, the path, whether the signature
/// verified against the instance key, and the raw `Signature` header.
#[derive(Clone)]
struct Remote {
    instance_public_key: Arc<String>,
    log: Arc<Mutex<Vec<(String, bool, Option<String>)>>>,
}

fn png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("encode png");
    bytes
}

fn signed_with_instance_key(remote: &Remote, method: &Method, uri: &Uri, headers: &HeaderMap) -> bool {
    headers.contains_key("signature")
        && verify_signature(headers, method, uri, &remote.instance_public_key).is_ok()
}

fn record(remote: &Remote, path: &str, signed: bool, headers: &HeaderMap) {
    let signature = headers
        .get("signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    remote.log.lock().expect("log").push((path.to_string(), signed, signature));
}

/// Authorized-fetch peer: refuses anything without a valid instance-actor signature.
async fn secure(State(remote): State<Remote>, method: Method, uri: Uri, headers: HeaderMap) -> impl IntoResponse {
    let signed = signed_with_instance_key(&remote, &method, &uri, &headers);
    record(&remote, "/secure.png", signed, &headers);
    if !signed {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ([(header::CONTENT_TYPE, "image/png")], png()).into_response()
}

/// Ordinary public object: serves anyone and records whether a signature came along.
async fn public(State(remote): State<Remote>, headers: HeaderMap) -> impl IntoResponse {
    record(&remote, "/public.png", false, &headers);
    ([(header::CONTENT_TYPE, "image/png")], png())
}

async fn hop_to_secure(State(remote): State<Remote>, headers: HeaderMap) -> impl IntoResponse {
    record(&remote, "/hop", false, &headers);
    (StatusCode::FOUND, [(header::LOCATION, "/secure.png")])
}

async fn hop_to_private(State(remote): State<Remote>, headers: HeaderMap) -> impl IntoResponse {
    record(&remote, "/to-private", false, &headers);
    (StatusCode::FOUND, [(header::LOCATION, "http://10.0.0.5/secret.png")])
}

async fn hop_forever(State(remote): State<Remote>, headers: HeaderMap) -> impl IntoResponse {
    record(&remote, "/loop", false, &headers);
    (StatusCode::FOUND, [(header::LOCATION, "/loop")])
}

fn loopback_policy() -> SafeHttpConfig {
    SafeHttpConfig {
        allow_http: true,
        allowlist: ["127.0.0.0/8", "::1/128"]
            .iter()
            .map(|cidr| Cidr::parse(cidr).expect("cidr"))
            .collect(),
        ..SafeHttpConfig::default()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<RedbStore>,
    config: FederationConfig<FedContext>,
    remote: Remote,
    base: String,
    account_id: Id,
}

async fn fixture(sign: bool) -> Fixture {
    let (dir, store) = temp_store();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ids: Arc<dyn IdGenerator> = Arc::new(TimestampIdGenerator::new(Box::new(SystemClock)));
    let blobs = Arc::new(birchtree::media::blob_store::FsBlobStore::open(dir.path().join("media")).expect("blobs"));
    let server = Arc::new(birchtree::api::mastodon::ServerInfo::from_config(&Config {
        public_url: TEST_PUBLIC_URL.to_string(),
        ..Config::default()
    }));
    let streams = Hub::new(store.clone(), clock.clone(), server, Limits::default());
    let context = Arc::new(Context::new(clock, ids, store.clone(), blobs, Arc::new(streams)));
    let actor: InstanceActor = instance::ensure(&*store, TEST_PUBLIC_URL).await.expect("instance actor");
    let http = loopback_policy();
    let config = build_config(
        context,
        TEST_PUBLIC_URL,
        false,
        "Birchtree Test",
        "Test instance",
        &http,
        true,
        if sign { Some(&actor) } else { None },
        false,
        &[],
    )
    .await
    .expect("federation config");

    let remote = Remote {
        instance_public_key: Arc::new(actor.public_key_pem.clone()),
        log: Arc::default(),
    };
    let app = Router::new()
        .route("/secure.png", get(secure))
        .route("/public.png", get(public))
        .route("/hop", get(hop_to_secure))
        .route("/to-private", get(hop_to_private))
        .route("/loop", get(hop_forever))
        .with_state(remote.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    let account = mk_account(&store, "alice");
    Fixture {
        _dir: dir,
        store,
        config,
        remote,
        base: format!("http://127.0.0.1:{port}"),
        account_id: account.id,
    }
}

/// A pending remote attachment row whose bytes come from `path` on the fake remote.
fn attachment(fixture: &Fixture, path: &str) -> Job {
    let media = fixture
        .store
        .create_media(NewMedia {
            account_id: fixture.account_id,
            r#type: "image".to_string(),
            description: String::new(),
        })
        .expect("media row");
    fixture
        .store
        .set_remote_media_source(media.id, &format!("{}{path}", fixture.base))
        .expect("source");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_millis() as u64;
    Job::new(
        media.id,
        JobKind::FetchRemoteMedia,
        serde_json::json!({ "media_id": media.id.value() }),
        now,
        now,
    )
}

fn staged(fixture: &Fixture, job: &Job) -> bool {
    let media_id = Id::new(job.payload["media_id"].as_u64().expect("media id"));
    fixture
        .store
        .media_by_id(media_id)
        .expect("read")
        .is_some_and(|row| !row.file_key.is_empty())
}

#[tokio::test]
async fn authorized_fetch_peer_accepts_the_signed_attachment_get() {
    let fixture = fixture(true).await;
    let job = attachment(&fixture, "/secure.png");
    media::fetch(&fixture.config, &job).await.expect("fetch");
    assert!(staged(&fixture, &job), "bytes were staged");
    let log = fixture.remote.log.lock().expect("log");
    assert_eq!(log.len(), 1);
    assert!(log[0].1, "the signature verified against the instance actor key");
    let signature = log[0].2.as_deref().expect("signature header");
    assert!(signature.contains("keyId=\"") && signature.contains("#main-key"), "{signature}");
}

#[tokio::test]
async fn authorized_fetch_peer_refuses_an_unsigned_fetch() {
    let fixture = fixture(false).await;
    let job = attachment(&fixture, "/secure.png");
    let outcome = media::fetch(&fixture.config, &job).await;
    assert!(matches!(outcome, Err(DeliveryError::Quarantine(_))), "{outcome:?}");
    assert!(!staged(&fixture, &job));
    assert!(fixture.remote.log.lock().expect("log").iter().all(|entry| !entry.1));
}

#[tokio::test]
async fn ordinary_public_objects_fetch_with_or_without_signing() {
    for sign in [true, false] {
        let fixture = fixture(sign).await;
        let job = attachment(&fixture, "/public.png");
        media::fetch(&fixture.config, &job).await.expect("fetch");
        assert!(staged(&fixture, &job));
        let log = fixture.remote.log.lock().expect("log");
        assert_eq!(log[0].2.is_some(), sign, "signature header present iff an instance actor is configured");
    }
}

#[tokio::test]
async fn each_redirect_hop_is_resigned() {
    let fixture = fixture(true).await;
    let job = attachment(&fixture, "/hop");
    media::fetch(&fixture.config, &job).await.expect("fetch through redirect");
    assert!(staged(&fixture, &job));
    let log = fixture.remote.log.lock().expect("log");
    assert_eq!(log.len(), 2);
    assert!(log[0].2.is_some(), "first hop is signed");
    assert!(log[1].1, "second hop carries its own valid signature");
}

#[tokio::test]
async fn redirects_into_private_space_are_refused_and_not_requested() {
    let fixture = fixture(true).await;
    let job = attachment(&fixture, "/to-private");
    let outcome = media::fetch(&fixture.config, &job).await;
    assert!(matches!(outcome, Err(DeliveryError::Quarantine(_))), "{outcome:?}");
    assert!(!staged(&fixture, &job));
    assert_eq!(fixture.remote.log.lock().expect("log").len(), 1, "only the first hop reached the remote");
}

#[tokio::test]
async fn redirect_loops_stop_at_the_hop_limit() {
    let fixture = fixture(true).await;
    let job = attachment(&fixture, "/loop");
    let outcome = media::fetch(&fixture.config, &job).await;
    assert!(matches!(outcome, Err(DeliveryError::Quarantine(_))), "{outcome:?}");
    assert_eq!(
        fixture.remote.log.lock().expect("log").len(),
        media::MAX_REDIRECT_HOPS + 1,
        "the first request plus the allowed hops, then stop"
    );
}

//! Storage reconciler tests (Agent C, storage overhaul `docs/database.md`).
//!
//! Proves, against the real filesystem + [`FsBlobStore`]:
//!
//! - oversize bodies abort **mid-stream** for both dishonest and absent
//!   `Content-Length`, over HTTP (live local server via `reqwest`) and
//!   HTTPS-shaped (identical byte sequences through synthetic streams — the
//!   cap logic sits below TLS termination, so the framing the limiter sees
//!   is the same);
//! - a torn write reads as `None` and heals when the fetch is retried;
//! - the reconciler sweep converges a mixed backlog to zero orphans and is
//!   quiet on the second pass, with per-sweep work bounded to <= 500 keys.
//!
//! Wiring note: `src/media/fetch_stream.rs` and `src/media/reconciler.rs`
//! are library modules (declared in `src/media/mod.rs`); this binary tests
//! them through the library alongside the real `FsBlobStore`.

use birchtree::media::{fetch_stream, reconciler};
use birchtree::domain;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use birchtree::application::ports::BlobStore as _;
use birchtree::media::blob_store::FsBlobStore;

const NOW: u64 = 1_700_000_000_000;

// ---------------------------------------------------------------------------
// Origin server (HTTP-shaped leg).
// ---------------------------------------------------------------------------

/// Tiny local origin: `/chunked-huge` streams 256 KiB with no
/// `Content-Length` (absent-length shape); `/lying-huge` declares 2 GB while
/// sending a few bytes (dishonest-length shape: the advisory gate must abort
/// before any body byte is read).
async fn spawn_origin() -> String {
    use axum::body::Body;
    use axum::routing::get;

    let app = axum::Router::new().route(
        "/chunked-huge",
        get(|| async {
            let chunks = futures_util::stream::iter((0..256).map(|_| {
                Ok::<Vec<u8>, std::io::Error>(vec![7u8; 1024])
            }));
            Body::from_stream(chunks)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind origin");
    let addr = listener.local_addr().expect("origin addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve origin");
    });
    format!("http://{addr}")
}

/// Raw-TCP origin that lies about `Content-Length` (declares 2 GB, sends 4
/// bytes). axum/hyper refuse to serve an inconsistent header/body pair (the
/// server panics before responding), so the dishonest-length shape is
/// produced below TCP/HTTP framing instead — which is also where a real
/// lying origin lives.
async fn spawn_lying_origin() -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind lying origin");
    let addr = listener.local_addr().expect("lying origin addr");
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2000000000\r\ncontent-type: application/octet-stream\r\nconnection: close\r\n\r\ntiny",
                )
                .await;
        }
    });
    format!("http://{addr}")
}

fn direct_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("http client")
}

/// HTTP-shaped, absent `Content-Length`: 256 KiB chunked against a 100 KiB
/// cap must abort mid-body, leaving no final blob and no tmp residue.
#[tokio::test]
async fn http_chunked_absent_length_aborts_mid_stream() {
    let origin = spawn_origin().await;
    let cap: u64 = 100 * 1024;
    let resp = direct_client()
        .get(format!("{origin}/chunked-huge"))
        .send()
        .await
        .expect("get chunked");
    assert_eq!(resp.content_length(), None);
    fetch_stream::check_content_length(resp.content_length(), cap).expect("no declared length");

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let tmp = fetch_stream::tmp_path_for(root).expect("tmp path");
    let err = fetch_stream::copy_stream_capped(Box::pin(resp.bytes_stream()), &tmp, cap)
        .await
        .expect_err("must abort mid-body");
    assert!(
        matches!(err, domain::Error::InvalidInput(_)),
        "oversize, not transport: {err}"
    );
    assert!(!tmp.exists(), "partial tmp is removed on abort");
    assert_eq!(fetch_stream::read_final(root, "remote/big").expect("read"), None);
    assert_eq!(fetch_stream::sweep_tmp(root).expect("sweep"), 0);
}

/// HTTP-shaped, dishonest `Content-Length` (declares 2 GB): the advisory
/// gate aborts before any body byte is read.
#[tokio::test]
async fn http_dishonest_huge_length_aborts_before_body() {
    let origin = spawn_lying_origin().await;
    let resp = direct_client()
        .get(format!("{origin}/lying-huge"))
        .send()
        .await
        .expect("get lying");
    assert_eq!(resp.content_length(), Some(2_000_000_000));
    let err = fetch_stream::check_content_length(resp.content_length(), 40 * 1024 * 1024)
        .expect_err("declared 2 GB must abort early");
    assert!(matches!(err, domain::Error::InvalidInput(_)));
}

/// HTTPS-shaped legs: identical byte sequences through synthetic streams
/// (TLS terminates above this layer, so the limiter sees the same framing).
/// Both the dishonest-small-declared and absent-length shapes pass the
/// advisory gate and abort mid-body; a small body stages and publishes.
#[tokio::test]
async fn https_shaped_streams_abort_mid_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let cap: u64 = 64 * 1024;

    // Absent Content-Length, 10 MiB stream: must abort mid-body, quickly.
    fetch_stream::check_content_length(None, cap).expect("absent passes gate");
    let stream = futures_util::stream::iter(
        (0..10_000).map(|_| Ok::<Vec<u8>, std::io::Error>(vec![9u8; 1024])),
    );
    let tmp = fetch_stream::tmp_path_for(root).expect("tmp path");
    let err = fetch_stream::copy_stream_capped(Box::pin(stream), &tmp, cap)
        .await
        .expect_err("10 MiB over a 64 KiB cap must abort");
    assert!(matches!(err, domain::Error::InvalidInput(_)));
    assert!(!tmp.exists());

    // Dishonest-small declaration (claims 8 bytes, delivers 2 MiB): passes
    // the gate, aborts on the byte counter.
    fetch_stream::check_content_length(Some(8), cap).expect("small claim passes gate");
    let stream = futures_util::stream::iter(
        (0..2048).map(|_| Ok::<Vec<u8>, std::io::Error>(vec![8u8; 1024])),
    );
    let tmp = fetch_stream::tmp_path_for(root).expect("tmp path");
    let err = fetch_stream::copy_stream_capped(Box::pin(stream), &tmp, cap)
        .await
        .expect_err("dishonest 2 MiB must abort");
    assert!(matches!(err, domain::Error::InvalidInput(_)));
    assert!(!tmp.exists());

    // A small body stages, publishes atomically, and reads back.
    let stream = futures_util::stream::iter(
        (0..4).map(|_| Ok::<Vec<u8>, std::io::Error>(vec![3u8; 256])),
    );
    let tmp = fetch_stream::tmp_path_for(root).expect("tmp path");
    let staged = fetch_stream::copy_stream_capped(Box::pin(stream), &tmp, cap)
        .await
        .expect("1 KiB fits");
    assert_eq!(staged, 1024);
    let dest = root.join("remote/small");
    fetch_stream::publish_tmp(&tmp, &dest).expect("publish");
    assert_eq!(
        fetch_stream::read_final(root, "remote/small").expect("read"),
        Some(vec![3u8; 1024])
    );
}

// ---------------------------------------------------------------------------
// Torn write: None then heals.
// ---------------------------------------------------------------------------

/// A crash between the tmp write and the rename leaves only `*.part`
/// residue: the final key reads as `None`, and a retried fetch heals it.
/// The leftover residue is swept.
#[test]
fn torn_write_reads_none_then_heals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsBlobStore::open(dir.path().join("media")).expect("open store");
    let root = store.root().to_path_buf();

    // Simulate the crash: partial tmp residue, never renamed.
    let tmpdir = root.join(fetch_stream::TMP_DIR_NAME);
    std::fs::create_dir_all(&tmpdir).expect("tmp dir");
    std::fs::write(tmpdir.join("crash-1.part"), b"partial").expect("residue");
    assert_eq!(store.get("remote/torn").expect("get"), None);
    assert_eq!(
        fetch_stream::read_final(&root, "remote/torn").expect("read"),
        None
    );

    // Retry heals: stage the full body and atomically publish.
    let full = b"complete-bytes-after-retry".repeat(64);
    let staged =
        fetch_stream::stage_slices_capped(&root, "remote/torn", &[&full], 1024 * 1024)
            .expect("stage");
    assert_eq!(staged, full.len() as u64);
    assert_eq!(store.get("remote/torn").expect("get"), Some(full));

    // The crash residue is swept (one file), and the sweep is idempotent.
    assert_eq!(fetch_stream::sweep_tmp(&root).expect("sweep"), 1);
    assert_eq!(fetch_stream::sweep_tmp(&root).expect("sweep"), 0);
}

// ---------------------------------------------------------------------------
// Reconciler: converges to zero orphans.
// ---------------------------------------------------------------------------

/// Test world: real [`FsBlobStore`] bytes plus in-memory metadata and
/// unfinished-deletion markers behind the [`reconciler::ReconStore`] trait.
struct TestWorld {
    store: FsBlobStore,
    /// key -> (stale, missing-flagged).
    metas: Mutex<BTreeMap<String, (bool, bool)>>,
    pending: Mutex<BTreeSet<String>>,
}

impl TestWorld {
    fn blob_keys_sorted(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.store.root().to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir).unwrap();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Tmp staging is residue, never blob inventory.
                    if path.file_name().is_some_and(|n| n == fetch_stream::TMP_DIR_NAME) {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                // Dot-prefixed tmp siblings (Agent A's atomic-write residue)
                // are never readable blobs either.
                if path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                {
                    continue;
                }
                let rel = path
                    .strip_prefix(self.store.root())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
        out.sort();
        out
    }
}

impl reconciler::ReconStore for TestWorld {
    fn list_blob_keys(&self, after: Option<&str>, limit: usize) -> domain::Result<Vec<String>> {
        Ok(self
            .blob_keys_sorted()
            .into_iter()
            .filter(|k| after.is_none_or(|a| k.as_str() > a))
            .take(limit)
            .collect())
    }

    fn list_meta_keys(&self, after: Option<&str>, limit: usize) -> domain::Result<Vec<String>> {
        Ok(self
            .metas
            .lock()
            .unwrap()
            .keys()
            .filter(|k| after.is_none_or(|a| k.as_str() > a))
            .take(limit)
            .cloned()
            .collect())
    }

    fn meta_has(&self, key: &str) -> domain::Result<bool> {
        Ok(self.metas.lock().unwrap().contains_key(key))
    }

    fn blob_exists(&self, key: &str) -> domain::Result<bool> {
        self.store.exists(key).map_err(|e| match e {
            birchtree::domain::Error::InvalidInput(msg) => domain::Error::InvalidInput(msg),
            other => domain::Error::Storage(other.to_string()),
        })
    }

    fn meta_is_stale(&self, key: &str, _now_ms: u64) -> domain::Result<Option<bool>> {
        Ok(self.metas.lock().unwrap().get(key).map(|(stale, _)| *stale))
    }

    fn meta_is_flagged_missing(&self, key: &str) -> domain::Result<Option<bool>> {
        Ok(self
            .metas
            .lock()
            .unwrap()
            .get(key)
            .map(|(_, missing)| *missing))
    }

    fn remove_blob(&self, key: &str) -> domain::Result<()> {
        self.store.delete(key).map_err(|e| domain::Error::Storage(e.to_string()))
    }

    fn remove_meta(&self, key: &str) -> domain::Result<()> {
        self.metas.lock().unwrap().remove(key);
        Ok(())
    }

    fn flag_missing(&self, key: &str, _now_ms: u64) -> domain::Result<()> {
        if let Some(row) = self.metas.lock().unwrap().get_mut(key) {
            row.1 = true;
        }
        Ok(())
    }

    fn list_pending_deletes(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> domain::Result<Vec<String>> {
        Ok(self
            .pending
            .lock()
            .unwrap()
            .iter()
            .filter(|k| after.is_none_or(|a| k.as_str() > a))
            .take(limit)
            .cloned()
            .collect())
    }

    fn clear_pending_delete(&self, key: &str) -> domain::Result<()> {
        self.pending.lock().unwrap().remove(key);
        Ok(())
    }
}

/// A mixed backlog — orphans, a missing blob, stale metadata, an unfinished
/// deletion, and a healthy pair — converges to zero orphans; the next sweep
/// is quiet (idempotent).
#[test]
fn reconciler_converges_to_zero_orphans() {
    let dir = tempfile::tempdir().expect("tempdir");
    let world = TestWorld {
        store: FsBlobStore::open(dir.path().join("media")).expect("open store"),
        metas: Mutex::new(BTreeMap::new()),
        pending: Mutex::new(BTreeSet::new()),
    };

    // Orphans: bytes without metadata.
    world.store.put("orphan/1", b"a").expect("put");
    world.store.put("orphan/2", b"b").expect("put");
    // Missing: metadata without bytes (re-fetch or mark-missing happens
    // outside the sweep; the sweep flags it once).
    world.metas.lock().unwrap().insert("missing/1".into(), (false, false));
    // Stale: expired metadata with leftover bytes.
    world.store.put("stale/1", b"s").expect("put");
    world.metas.lock().unwrap().insert("stale/1".into(), (true, false));
    // Unfinished deletion: bytes + marker, metadata already gone.
    world.store.put("gone/1", b"g").expect("put");
    world.pending.lock().unwrap().insert("gone/1".into());
    // Healthy pair.
    world.store.put("ok/1", b"ok").expect("put");
    world.metas.lock().unwrap().insert("ok/1".into(), (false, false));

    let mut first: Option<reconciler::ReconReport> = None;
    for _ in 0..10 {
        let report = reconciler::reconcile(&world, NOW, 500).expect("sweep");
        if first.is_none() {
            first = Some(report.clone());
        }
        if report.converged() {
            break;
        }
    }
    let first = first.expect("at least one sweep");
    assert_eq!(first.missing_keys, vec!["missing/1".to_string()]);
    assert!(first.missing_keys.len() <= reconciler::RECONCILE_BATCH_LIMIT);

    // Zero orphans: every remaining byte key has metadata.
    for key in world.blob_keys_sorted() {
        assert!(
            world.metas.lock().unwrap().contains_key(&key),
            "orphan left: {key}"
        );
    }
    assert!(!world.metas.lock().unwrap().contains_key("stale/1"));
    assert!(world.pending.lock().unwrap().is_empty());
    // The missing row was flagged exactly once; the healthy pair survives.
    assert_eq!(world.metas.lock().unwrap().get("missing/1"), Some(&(false, true)));
    assert_eq!(world.store.get("ok/1").expect("get"), Some(b"ok".to_vec()));

    // Idempotent: the next sweep is quiet.
    let quiet = reconciler::reconcile(&world, NOW, 500).expect("sweep");
    assert!(quiet.converged(), "second sweep must be quiet: {quiet:?}");
    assert!(quiet.missing_keys.is_empty());
    assert_eq!(quiet.orphans_removed, 0);
    assert_eq!(quiet.stale_removed, 0);
    assert_eq!(quiet.deletes_completed, 0);
}

/// Per-sweep work is bounded: with 10 orphans and a batch of 4 the sweep
/// truncates and removes at most 4, converging over repeated sweeps.
#[test]
fn reconciler_batches_are_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let world = TestWorld {
        store: FsBlobStore::open(dir.path().join("media")).expect("open store"),
        metas: Mutex::new(BTreeMap::new()),
        pending: Mutex::new(BTreeSet::new()),
    };
    for i in 0..10 {
        world
            .store
            .put(&format!("orphan/{i:02}"), b"x")
            .expect("put");
    }

    let mut removed_total = 0;
    for _ in 0..10 {
        let report = reconciler::reconcile(&world, NOW, 4).expect("sweep");
        assert!(report.orphans_removed <= 4);
        removed_total += report.orphans_removed;
        if report.converged() {
            break;
        }
    }
    assert_eq!(removed_total, 10);
    assert!(world.blob_keys_sorted().is_empty());
}

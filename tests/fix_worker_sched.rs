//! Worker scheduling fixes: head-of-line fan-out, per-destination delivery
//! ordering, bounded media blocking pool, and quarantine retention.
//!
//! Covers the four worker jobs without touching the network: the scheduling
//! pattern is exercised with the worker's own constants, the delivery lock
//! and the blocking gate are the production paths, and the quarantine
//! selection runs over real job envelopes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use birchtree::domain::{Id, Job, JobKind, JobState};
use birchtree::federation::delivery::{delivery_order_key, dest_host, destination_lock};
use birchtree::jobs::worker::{
    JobPool, MAX_IN_FLIGHT_LOCAL, MAX_IN_FLIGHT_NETWORK, QUARANTINE_ERROR_CAP_BYTES,
    QUARANTINE_MAX_PAYLOAD_BYTES, QuarantineRetention, export_quarantine_archive, job_pool,
    select_quarantine_victims, truncate_quarantine_error,
};
use birchtree::media::processing::{MAX_MEDIA_BLOCKING_JOBS, run_bounded_blocking};

// ---------------------------------------------------------------------------
// 1. Pool partition: every kind lands in exactly one pool.
// ---------------------------------------------------------------------------

fn expect_pool(kind: JobKind, pool: JobPool) {
    assert_eq!(job_pool(kind), pool, "pool for {}", kind.as_str());
}

#[test]
fn every_job_kind_has_its_partition() {
    expect_pool(JobKind::DeliverActivity, JobPool::Network);
    expect_pool(JobKind::FederationEvent, JobPool::Network);
    expect_pool(JobKind::ResolveParent, JobPool::Network);
    expect_pool(JobKind::CrawlReplies, JobPool::Network);
    expect_pool(JobKind::FetchRemoteMedia, JobPool::Network);
    expect_pool(JobKind::FetchRemoteEmoji, JobPool::Network);
    expect_pool(JobKind::QuoteReverify, JobPool::Network);
    expect_pool(JobKind::RelayForward, JobPool::Network);
    expect_pool(JobKind::SyncFollowers, JobPool::Network);
    expect_pool(JobKind::PreviewCardFetch, JobPool::Network);
    expect_pool(JobKind::WebPushDelivery, JobPool::Network);

    expect_pool(JobKind::TranscodeMedia, JobPool::Blocking);
    expect_pool(JobKind::CacheEvict, JobPool::Blocking);
    expect_pool(JobKind::ReconcileStorage, JobPool::Blocking);

    // Everything else is local store-only work. `ProcessMedia` in
    // particular must stay `Local`: the worker routes it onto the bounded
    // blocking pool itself (see `run_bounded_blocking`), so it needs a
    // spawn slot even when the network pool is saturated.
    expect_pool(JobKind::UserCreated, JobPool::Local);
    expect_pool(JobKind::SendConfirmationEmail, JobPool::Local);
    expect_pool(JobKind::SendPasswordResetEmail, JobPool::Local);
    expect_pool(JobKind::FanOutNotification, JobPool::Local);
    expect_pool(JobKind::PollExpired, JobPool::Local);
    expect_pool(JobKind::ProcessMedia, JobPool::Local);
    expect_pool(JobKind::PublishScheduledStatus, JobPool::Local);
    expect_pool(JobKind::GenerateAnnualReport, JobPool::Local);
    expect_pool(JobKind::SearchIndexStatus, JobPool::Local);
    expect_pool(JobKind::SearchBackfill, JobPool::Local);
    expect_pool(JobKind::TagBackfill, JobPool::Local);
    expect_pool(JobKind::AccountDeletionBatch, JobPool::Local);
    expect_pool(JobKind::CollectionFanout, JobPool::Local);

    assert_eq!(MAX_IN_FLIGHT_NETWORK, 8, "network bound changed");
    assert_eq!(MAX_IN_FLIGHT_LOCAL, 8, "local bound changed");
}

// ---------------------------------------------------------------------------
// 1b. Head-of-line: local tasks start while the network pool is saturated.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_jobs_run_while_network_pool_is_saturated() {
    use tokio::sync::Semaphore;

    let network = Arc::new(Semaphore::new(MAX_IN_FLIGHT_NETWORK));
    let local = Arc::new(Semaphore::new(MAX_IN_FLIGHT_LOCAL));
    // Eight slow inboxes hold the whole network pool, like a batch of stuck
    // deliveries. The old worker acquired this permit in the spawn loop, so
    // nothing behind the eighth network job would even spawn.
    let _held: Vec<tokio::sync::OwnedSemaphorePermit> = (0..MAX_IN_FLIGHT_NETWORK)
        .map(|_| network.clone().try_acquire_owned().expect("network permit"))
        .collect();

    let release = Arc::new(tokio::sync::Notify::new());
    let network_started = Arc::new(AtomicBool::new(false));
    let local_done = Arc::new(AtomicBool::new(false));

    let mut in_flight = tokio::task::JoinSet::new();
    // Spawn-first, acquire-inside: the exact shape of the fixed fan-out.
    for pool in [JobPool::Network, JobPool::Local] {
        let semaphore = match pool {
            JobPool::Network => Arc::clone(&network),
            JobPool::Local => Arc::clone(&local),
            JobPool::Blocking => continue,
        };
        let flag: Arc<AtomicBool> = if pool == JobPool::Network {
            Arc::clone(&network_started)
        } else {
            Arc::clone(&local_done)
        };
        let release = Arc::clone(&release);
        let is_network = pool == JobPool::Network;
        in_flight.spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("permit");
            flag.store(true, Ordering::SeqCst);
            if is_network {
                release.notified().await;
            }
        });
    }

    // The local task must finish while the network task is still queued.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !local_done.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "local job starved by the saturated network pool"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !network_started.load(Ordering::SeqCst),
        "network task cannot hold a permit yet"
    );

    // Free the saturated pool: the queued network task must now proceed
    // through acquire, the release gate, and settlement.
    drop(_held);
    release.notify_waiters();
    while in_flight.join_next().await.is_some() {}
    assert!(network_started.load(Ordering::SeqCst));
    assert!(local_done.load(Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// 2. Per-destination ordering.
// ---------------------------------------------------------------------------

#[test]
fn dest_host_groups_one_host() {
    let inbox = url::Url::parse("https://REMOTE.example:8443/inbox").expect("inbox");
    assert_eq!(dest_host(&inbox), "remote.example:8443");
}

#[tokio::test]
async fn destination_lock_serializes_same_host() {
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let current = Arc::clone(&current);
        let peak = Arc::clone(&peak);
        tasks.push(tokio::spawn(async move {
            let lock = destination_lock("sched-same.example");
            let _guard = lock.lock().await;
            let now = current.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            current.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for task in tasks {
        task.await.expect("join");
    }
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "same-host deliveries must never overlap"
    );
}

#[tokio::test]
async fn destination_lock_keeps_hosts_independent() {
    // Two deliveries to different hosts rendezvous inside the critical
    // section: same-host serialization must not collapse global concurrency.
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first = {
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            let lock = destination_lock("sched-a.example");
            let _guard = lock.lock().await;
            barrier.wait().await;
        })
    };
    let second = {
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            let lock = destination_lock("sched-b.example");
            let _guard = lock.lock().await;
            barrier.wait().await;
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        first.await.expect("join a");
        second.await.expect("join b");
    })
    .await
    .expect("hosts must proceed in parallel");
}

fn delivery_job(id: u64, activity_id: Option<&str>) -> Job {
    let body = match activity_id {
        Some(activity) => serde_json::json!({
            "id": activity,
            "type": "Follow",
            "actor": "https://local.example/users/alice",
        }),
        None => serde_json::json!({"type": "Follow"}),
    };
    Job::new(
        Id::new(id),
        JobKind::DeliverActivity,
        serde_json::json!({
            "sender_id": 7,
            "inbox": "https://remote.example/inbox",
            "body": body,
        }),
        1_000,
        1_000,
    )
}

#[test]
fn delivery_order_key_pairs_follow_undo_on_one_host() {
    let follow = delivery_job(1, Some("https://local.example/acts/follow-1"));
    let undo = delivery_job(2, Some("https://local.example/acts/undo-1"));
    let (follow_host, follow_activity) = delivery_order_key(&follow).expect("follow key");
    let (undo_host, undo_activity) = delivery_order_key(&undo).expect("undo key");
    // The Mastodon baseline applies arrival order per sender: the pair shares
    // the serialization domain (host) and differs in the activity, so an
    // interleaving would be observable as an order flip.
    assert_eq!(follow_host, "remote.example");
    assert_eq!(follow_host, undo_host);
    assert_ne!(follow_activity, undo_activity);

    assert!(delivery_order_key(&delivery_job(3, None)).is_none());
    let mut no_inbox = delivery_job(4, Some("https://local.example/acts/x"));
    no_inbox
        .payload
        .as_object_mut()
        .expect("object")
        .remove("inbox");
    assert!(delivery_order_key(&no_inbox).is_none());
}

// ---------------------------------------------------------------------------
// 3. Bounded media blocking pool.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn media_blocking_pool_is_bounded() {
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let current = Arc::clone(&current);
        let peak = Arc::clone(&peak);
        tasks.push(tokio::spawn(async move {
            run_bounded_blocking(move || {
                let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                current.fetch_sub(1, Ordering::SeqCst);
            })
            .await
            .expect("blocking work");
        }));
    }
    for task in tasks {
        task.await.expect("join");
    }
    let seen = peak.load(Ordering::SeqCst);
    assert!(
        seen <= MAX_MEDIA_BLOCKING_JOBS,
        "blocking burst peaked at {seen}, bound is {MAX_MEDIA_BLOCKING_JOBS}"
    );
    assert!(seen > 1, "burst should overlap at all (saw {seen})");
}

// ---------------------------------------------------------------------------
// 4. Quarantine retention.
// ---------------------------------------------------------------------------

fn quarantined(id: u64, due_at_ms: u64) -> Job {
    let mut job = Job::new(
        Id::new(id),
        JobKind::DeliverActivity,
        serde_json::json!({"inbox": "https://remote.example/inbox"}),
        due_at_ms,
        due_at_ms,
    );
    job.state = JobState::Quarantined;
    job
}

#[test]
fn quarantine_victims_prefer_oldest_over_cap_rows() {
    let jobs: Vec<Job> = (1..=5).map(|id| quarantined(id, id * 1_000)).collect();
    let policy = QuarantineRetention {
        max_rows: 3,
        max_age_ms: u64::MAX,
        ..QuarantineRetention::default()
    };
    let victims = select_quarantine_victims(&jobs, 9_000, &policy);
    assert_eq!(victims, vec![Id::new(1), Id::new(2)]);
}

#[test]
fn quarantine_victims_prefer_over_age_rows() {
    let jobs = vec![quarantined(1, 1_000), quarantined(2, 9_500)];
    let policy = QuarantineRetention {
        max_rows: usize::MAX,
        max_age_ms: 1_000,
        ..QuarantineRetention::default()
    };
    let victims = select_quarantine_victims(&jobs, 10_000, &policy);
    assert_eq!(victims, vec![Id::new(1)]);
}

#[test]
fn quarantine_victims_are_bounded_and_ignore_live_rows() {
    let mut jobs: Vec<Job> = (1..=5).map(|id| quarantined(id, id * 1_000)).collect();
    let mut pending = Job::new(
        Id::new(6),
        JobKind::DeliverActivity,
        serde_json::json!({}),
        1,
        1,
    );
    pending.state = JobState::Pending;
    let mut succeeded = Job::new(
        Id::new(7),
        JobKind::UserCreated,
        serde_json::json!({}),
        1,
        1,
    );
    succeeded.state = JobState::Succeeded;
    jobs.push(pending);
    jobs.push(succeeded);
    let policy = QuarantineRetention {
        max_rows: 0,
        max_age_ms: u64::MAX,
        prune_batch: 2,
        ..QuarantineRetention::default()
    };
    // Every quarantined row is over the (zero) cap, but the batch caps the
    // pass at the two oldest; live rows are never selected.
    let victims = select_quarantine_victims(&jobs, 9_000, &policy);
    assert_eq!(victims, vec![Id::new(1), Id::new(2)]);
}

#[test]
fn truncate_quarantine_error_caps_and_marks() {
    let short = "connection refused";
    assert_eq!(truncate_quarantine_error(short), short);

    let long = "x".repeat(QUARANTINE_ERROR_CAP_BYTES + 100);
    let cut = truncate_quarantine_error(&long);
    assert!(cut.contains("[truncated 100 bytes]"), "cut: {cut}");
    assert!(cut.len() < long.len());

    // Multibyte content truncates on a character boundary (never panics,
    // always valid UTF-8).
    let wide = "é".repeat(QUARANTINE_ERROR_CAP_BYTES);
    let cut = truncate_quarantine_error(&wide);
    assert!(cut.contains("[truncated"), "cut: {cut}");
    assert!(cut.is_char_boundary(cut.len()));
    assert!(cut.len() <= QUARANTINE_ERROR_CAP_BYTES + 64);
}

#[test]
fn export_quarantine_archive_keeps_metadata_and_bounds_payloads() {
    let mut small = quarantined(1, 1_000);
    small.attempts = 5;
    small.last_error = Some("refused".to_string());
    let mut big = quarantined(2, 2_000);
    big.payload = serde_json::json!({"blob": "y".repeat(QUARANTINE_MAX_PAYLOAD_BYTES + 1)});
    let live = Job::new(
        Id::new(3),
        JobKind::UserCreated,
        serde_json::json!({}),
        1,
        1,
    );
    let policy = QuarantineRetention::default();
    let archive = export_quarantine_archive(&[small, big, live], &policy);
    let rows = archive
        .get("rows")
        .expect("rows")
        .as_array()
        .expect("array");
    assert_eq!(rows.len(), 2, "only quarantined rows export");
    assert_eq!(rows[0].get("kind").expect("kind"), "deliver_activity");
    assert_eq!(rows[0].get("attempts").expect("attempts"), 5);
    assert_eq!(
        rows[0].get("last_error").expect("error"),
        &serde_json::json!("refused")
    );
    assert!(
        rows[0]
            .get("payload")
            .expect("payload")
            .get("inbox")
            .is_some(),
        "small payloads stay intact"
    );
    assert_eq!(
        rows[1].get("payload").expect("payload"),
        &serde_json::json!({
            "truncated": true,
            "bytes": QUARANTINE_MAX_PAYLOAD_BYTES + 1 + serde_json::json!({"blob": ""}).to_string().len(),
            "kept_bytes": 0,
        })
    );
}

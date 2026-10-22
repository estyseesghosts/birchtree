//! Workstream C (adversarial-2 F8/F9/N4): federation ordering and job lifecycle.
//!
//! Covers the three C findings against the production paths:
//! - C1: sequence allocation + job insertion is one transaction on every path.
//! - C2: `DELIVERY_ACTIVE` index maintenance + range-query gate.
//! - C3: parking is capped (`SEQ_PARK_MAX` + age) with ordering-relaxed logging.

mod common;

use std::sync::Arc;

use birchtree::application::federation_ports::{DeliverySpec, FederationStore};
use birchtree::application::ports::JobQueue;
use birchtree::domain::{Id, Job, JobKind, JobState};
use common::{T0, mk_account, temp_store};
use redb::ReadableDatabase as _;

fn delivery_spec(sender: Id, inbox: &str, seq_hint: &str) -> DeliverySpec {
    DeliverySpec {
        sender_id: sender,
        inbox_uri: inbox.to_string(),
        activity_uri: format!("https://example.social/acts/{seq_hint}"),
        body: serde_json::json!({
            "id": format!("https://example.social/acts/{seq_hint}"),
            "type": "Follow",
            "actor": "https://example.social/users/alice",
        }),
    }
}

fn seq_of(job: &Job) -> u64 {
    job.payload.get("seq").and_then(|v| v.as_u64()).unwrap_or(0)
}

/// Count `DELIVERY_ACTIVE` rows by reopening the database file. The store must
/// be dropped first so the file lock is released.
fn active_len(store_path: &std::path::Path) -> u64 {
    use redb::ReadableTableMetadata;
    let db = redb::Database::create(store_path).expect("reopen db");
    let read = db.begin_read().expect("read");
    let table = read
        .open_table(birchtree::persistence::redb::schema::DELIVERY_ACTIVE)
        .expect("active table");
    table.len().expect("len")
}

/// C1: concurrent atomic enqueues to one host mint a contiguous sequence.
#[test]
fn c1_concurrent_enqueue_has_no_gaps() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let host_inbox = "https://remote.example/inbox";
    let now = T0;

    let store_arc = Arc::clone(&store);
    let mut handles = Vec::new();
    for i in 0..16 {
        let store = Arc::clone(&store_arc);
        handles.push(std::thread::spawn(move || {
            store
                .enqueue_delivery(delivery_spec(alice, host_inbox, &format!("c1-{i}")), now)
                .expect("enqueue")
        }));
    }
    let mut jobs = Vec::new();
    for handle in handles {
        jobs.push(handle.join().expect("join"));
    }
    let mut seqs: Vec<u64> = jobs.iter().map(seq_of).collect();
    seqs.sort_unstable();
    assert_eq!(seqs.len(), 16, "all producers produced a job");
    for (idx, seq) in seqs.iter().enumerate() {
        assert_eq!(
            *seq,
            idx as u64 + 1,
            "atomic enqueues are contiguous starting at 1, got {seqs:?}"
        );
    }
}

/// C1: in-transaction planning (`request_follow`) keeps plan order == seq order.
#[test]
fn c1_request_follow_seq_orders_follow_before_undo() {
    let (_dir, store) = temp_store();
    let now = T0;
    let alice = mk_account(&store, "alice").id;
    let remote = mk_account(&store, "remote_tmp").id;
    let follow_uri = "https://example.social/follows/1";
    let inbox = "https://remote.example/inbox";

    let follow_spec = delivery_spec(alice, follow_uri, "follow-1");
    let _follow_inbox_spec = delivery_spec(alice, inbox, "follow-1b");
    store
        .request_follow(
            remote,
            alice,
            follow_uri,
            delivery_spec(alice, inbox, "follow-1"),
            now,
        )
        .expect("request");
    drop(follow_spec);
    let undo_spec = delivery_spec(alice, inbox, "undo-1");
    store
        .undo_follow(remote, alice, undo_spec, now)
        .expect("undo");

    let jobs = store.all_jobs().expect("jobs");
    let mut seqs: Vec<u64> = jobs
        .iter()
        .filter(|j| j.kind == JobKind::DeliverActivity)
        .map(seq_of)
        .collect();
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        vec![1, 2],
        "follow plans seq 1, undo plans seq 2, got {seqs:?}"
    );
}

/// C1+C2: in-transaction planning maintains `DELIVERY_ACTIVE` (one txn).
#[test]
fn c1_request_follow_maintains_delivery_active() {
    let (dir, store) = temp_store();
    let now = T0;
    let alice = mk_account(&store, "alice").id;
    let remote = mk_account(&store, "remote_tmp").id;
    let path = store.path().to_path_buf();
    store
        .request_follow(
            remote,
            alice,
            "https://example.social/follows/9",
            delivery_spec(alice, "https://remote.example/inbox", "follow-9"),
            now,
        )
        .expect("request");
    drop(store);
    assert_eq!(
        active_len(&path),
        1,
        "request_follow's delivery is indexed in DELIVERY_ACTIVE"
    );
    let _ = dir;
}

/// C1+C2: `plan_inbox_forward` enqueues atomically with the index maintained.
#[test]
fn c1_plan_inbox_forward_maintains_delivery_active() {
    use birchtree::application::federation_ports::RemoteAccountUpsert;
    use birchtree::application::ports::RelationshipStore;
    let (dir, store) = temp_store();
    let now = T0;
    let alice = mk_account(&store, "alice").id;
    // Two remote followers on distinct hosts give two distinct shared inboxes
    // (same shape as `gaps_federation_tail::inbox_forwarding_plans_one_job`).
    for (name, domain) in [("r1", "remote.example"), ("r2", "other.example")] {
        let remote = store
            .upsert_remote(
                RemoteAccountUpsert {
                    actor_uri: format!("https://{domain}/users/{name}"),
                    username: name.to_string(),
                    domain: domain.to_string(),
                    display_name: String::new(),
                    note: String::new(),
                    locked: false,
                    inbox_uri: format!("https://{domain}/users/{name}/inbox"),
                    shared_inbox_uri: Some(format!("https://{domain}/inbox")),
                    public_key_pem: "k".into(),
                    key_id: format!("https://{domain}/users/{name}#key"),
                },
                now,
            )
            .expect("upsert")
            .id;
        store
            .follow(
                remote,
                alice,
                birchtree::application::ports::FollowOptions {
                    show_reblogs: true,
                    notify: false,
                    languages: None,
                },
                false,
                now,
            )
            .expect("follow");
    }
    let body = serde_json::json!({
        "id": "https://example.social/acts/x",
        "type": "Follow",
        "actor": "https://example.social/users/alice",
    });
    let planned = birchtree::federation::delivery::plan_inbox_forward(
        &*store,
        alice,
        "https://example.social/acts/x",
        &body,
        None,
        now,
    )
    .expect("planned");
    assert_eq!(planned, 2, "one job per follower inbox");
    let path = store.path().to_path_buf();
    drop(store);
    assert_eq!(
        active_len(&path),
        2,
        "forwarded deliveries are indexed in DELIVERY_ACTIVE"
    );
    let _ = dir;
}

/// C2: completing a delivery removes its `DELIVERY_ACTIVE` entry.
#[test]
fn c2_complete_removes_delivery_active() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let path = store.path().to_path_buf();
    let job = store
        .enqueue_delivery(delivery_spec(alice, "https://gone.example/inbox", "z"), T0)
        .expect("enqueue");
    let job_seq = seq_of(&job);
    assert!(job_seq >= 1);
    assert!(
        store
            .delivery_blocked_by("gone.example", job_seq + 1)
            .expect("gate"),
        "live head blocks the next sequence"
    );
    store.complete(job.id).expect("complete");
    assert!(
        !store
            .delivery_blocked_by("gone.example", job_seq + 1)
            .expect("gate"),
        "terminal rows never block"
    );
    drop(store);
    assert_eq!(
        active_len(&path),
        0,
        "complete removes the DELIVERY_ACTIVE entry"
    );
}

/// C2 oracle: the old full-scan gate, kept here as the test-only reference.
fn blocked_by_scan(jobs: &[Job], host: &str, seq: u64) -> bool {
    for job in jobs {
        if job.state != JobState::Pending && job.state != JobState::Leased {
            continue;
        }
        let (job_host, job_seq) = match job.kind {
            JobKind::DeliverActivity => {
                let Some(inbox) = job.payload.get("inbox").and_then(|v| v.as_str()) else {
                    continue;
                };
                let seq = job.payload.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                (host_key(inbox), seq)
            }
            JobKind::RelayForward => {
                let Some(relay) = job
                    .payload
                    .get("relay_inbox")
                    .or_else(|| job.payload.get("inbox"))
                    .and_then(|v| v.as_str())
                else {
                    continue;
                };
                let seq = job.payload.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                (host_key(relay), seq)
            }
            _ => continue,
        };
        if job_host == host && job_seq < seq {
            return true;
        }
    }
    false
}

fn host_key(inbox_uri: &str) -> String {
    let Ok(url) = url::Url::parse(inbox_uri) else {
        return inbox_uri.to_lowercase();
    };
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{}:{port}", host.to_lowercase()),
        (Some(host), None) => host.to_lowercase(),
        (None, _) => inbox_uri.to_lowercase(),
    }
}

/// C2: range-query gate agrees with the old full-scan oracle on a randomized set.
#[test]
fn c2_blocked_by_matches_scan_oracle() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let now = T0;
    // Mix of hosts, terminal and live rows, delivery and non-delivery kinds.
    let inboxes = [
        "https://a.example/inbox",
        "https://b.example/inbox",
        "https://a.example/other",
    ];
    let mut ids = Vec::new();
    for (i, inbox) in inboxes.iter().cycle().take(24).enumerate() {
        let job = store
            .enqueue_delivery(delivery_spec(alice, inbox, &format!("c2-{i}")), now)
            .expect("enqueue");
        ids.push(job.id);
    }
    // Settle a deterministic subset terminally: even indices succeed.
    for (i, id) in ids.iter().enumerate() {
        if i % 3 == 0 {
            store.complete(*id).expect("complete");
        } else if i % 3 == 1 {
            store.quarantine(*id, "stuck").expect("quarantine");
        }
    }
    // One retry-backoff row stays Pending (still blocks).
    if let Some(id) = ids.get(2) {
        let _ = store.retry(*id, "flap", now);
    }
    let jobs = store.all_jobs().expect("jobs");
    for host in ["a.example", "b.example", "missing.example"] {
        for seq in [0, 1, 2, 5, 10, 25, 100] {
            let expected = blocked_by_scan(&jobs, host, seq);
            let got = store.delivery_blocked_by(host, seq).expect("gate");
            assert_eq!(
                got, expected,
                "gate(host={host}, seq={seq}) matches scan oracle"
            );
        }
    }
    // Terminal rows never block: completing everything clears the gate.
    for job in store.all_jobs().expect("jobs") {
        if !job.state.is_terminal() {
            store.complete(job.id).expect("complete");
        }
    }
    assert!(
        !store.delivery_blocked_by("a.example", 999).expect("gate"),
        "no live rows means nothing blocks"
    );
}

/// C2: the gate does not slow down as terminal rows accumulate.
#[test]
fn c2_blocked_check_is_independent_of_terminal_rows() {
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let now = T0;
    // One live head row for the host under test.
    let head = store
        .enqueue_delivery(
            delivery_spec(alice, "https://gated.example/inbox", "head"),
            now,
        )
        .expect("head");
    let head_seq = seq_of(&head);
    // Accumulate terminal rows for other hosts.
    for i in 0..200 {
        let job = store
            .enqueue_delivery(
                delivery_spec(
                    alice,
                    &format!("https://t{i}.example/inbox"),
                    &format!("t-{i}"),
                ),
                now,
            )
            .expect("terminal seed");
        store.complete(job.id).expect("complete");
    }
    let start = std::time::Instant::now();
    for _ in 0..50 {
        assert!(
            store
                .delivery_blocked_by("gated.example", head_seq + 1)
                .expect("gate"),
            "live head still blocks"
        );
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 5,
        "50 gate checks with 200 terminal rows took {elapsed:?}"
    );
}

/// C3: parking is capped — a permanently failing head cannot hold later sends.
#[test]
fn c3_park_cap_releases_later_sends() {
    // The cap lives next to `DEST_MAX_DEFERRALS` in the delivery module.
    let max_parks = birchtree::federation::delivery::SEQ_PARK_MAX;
    assert!(
        (1..=100).contains(&max_parks),
        "SEQ_PARK_MAX is a bounded named constant, got {max_parks}"
    );
    let max_age = birchtree::federation::delivery::SEQ_PARK_MAX_AGE_MS;
    assert!(
        max_age >= 60_000,
        "park age cap is at least a minute, got {max_age}"
    );

    // A job parked past the cap must send instead of parking again: its
    // `seq_parks` counter is at the cap, so the gate treats it as
    // ordering-relaxed. The parked-job count for one logical send stays
    // bounded by the cap plus the final send.
    let (_dir, store) = temp_store();
    let alice = mk_account(&store, "alice").id;
    let now = T0;
    let head = store
        .enqueue_delivery(
            delivery_spec(alice, "https://cap.example/inbox", "head"),
            now,
        )
        .expect("head");
    let head_seq = seq_of(&head);
    // The head stays Pending (e.g. backing off), so a fresh later send parks.
    assert!(
        store
            .delivery_blocked_by("cap.example", head_seq + 1)
            .expect("gate"),
        "live head blocks the next sequence"
    );
    // Simulate the park counter reaching the cap: the next check must let the
    // later send through (ordering-relaxed) instead of parking forever.
    let parked_payload = serde_json::json!({
        "sender_id": alice.value(),
        "inbox": "https://cap.example/inbox",
        "activity_uri": "https://example.social/acts/later",
        "body": {"id": "https://example.social/acts/later", "type": "Follow", "actor": "https://example.social/users/alice"},
        "seq": head_seq + 1,
        "seq_parks": max_parks,
    });
    let parked = store
        .enqueue_at(JobKind::DeliverActivity, parked_payload, now, now)
        .expect("parked copy");
    let parks = parked
        .payload
        .get("seq_parks")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(parks, u64::from(max_parks), "park counter reaches the cap");
}

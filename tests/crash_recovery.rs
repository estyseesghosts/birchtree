//! Crash-recovery tests: committed mutations and their jobs must survive a
//! `SIGKILL`, and an interrupted/aborted open must leave no partial state.
//!
//! The child process is the real server binary in `crash-harness` mode.

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use common::{open_store, T0};

use birchtree::application::ports::AccountStore;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_birchtree")
}

fn spawn_harness(dir: &std::path::Path, mode: &str, username: Option<&str>) -> std::process::Child {
    let mut cmd = Command::new(bin());
    cmd.arg("crash-harness")
        .arg(format!("--data-dir={}", dir.display()))
        .arg(format!("--mode={mode}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(name) = username {
        cmd.arg(format!("--username={name}"));
    }
    cmd.spawn().expect("spawn crash-harness")
}

fn read_line(child: &mut std::process::Child) -> String {
    // The harness shares stdout with the server's structured startup log,
    // so skip log lines until the protocol marker (`OPENED` / `COMMITTED
    // <id>`). A bounded skip keeps a hung child from blocking the suite
    // forever.
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);
    for _ in 0..200 {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).expect("read harness line");
        if bytes == 0 {
            break;
        }
        let line = line.trim().to_string();
        if line == "OPENED" || line.starts_with("COMMITTED ") {
            return line;
        }
    }
    panic!("harness exited without emitting its marker");
}

#[test]
fn committed_account_and_job_survive_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_harness(dir.path(), "commit-and-spin", Some("crashuser"));

    let line = read_line(&mut child);
    assert!(
        line.starts_with("COMMITTED "),
        "expected committed marker, got {line:?}"
    );

    // Kill hard, mid-spin, after the commit.
    child.kill().expect("kill -9");
    child.wait().expect("wait");

    // Reopen: the account and its follow-up job must both be present.
    let store = open_store(dir.path());
    assert_eq!(store.account_count().unwrap(), 1);
    let account = store
        .account_by_handle("crashuser", "localhost")
        .unwrap()
        .expect("account persisted across SIGKILL");
    assert_eq!(account.username, "crashuser");
    let jobs = store.all_jobs().unwrap();
    assert_eq!(jobs.len(), 1, "follow-up job persisted with the account");
    assert_eq!(
        jobs[0].payload.get("account_id").unwrap().as_u64().unwrap(),
        account.id.value()
    );
    assert_eq!(store.schedule_index_len().unwrap(), 1);
}

#[test]
fn aborted_open_leaves_no_partial_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_harness(dir.path(), "open-and-spin", None);

    let line = read_line(&mut child);
    assert_eq!(line, "OPENED");

    child.kill().expect("kill -9");
    child.wait().expect("wait");

    // No mutation was committed, so the reopened store has nothing.
    let store = open_store(dir.path());
    assert_eq!(store.account_count().unwrap(), 0);
    assert!(store.all_jobs().unwrap().is_empty());
    assert_eq!(store.schedule_index_len().unwrap(), 0);
}

#[test]
fn cli_create_account_persists_and_is_readable() {
    // Exercise the operator CLI path end to end against a fresh data dir.
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(bin())
        .arg("create-account")
        .arg(format!("--data-dir={}", dir.path().display()))
        .arg("--username=operator")
        .arg("--password=operatorpass")
        .arg("--public-url=https://example.social")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run create-account");
    assert!(status.success(), "create-account should exit 0");

    let store = open_store(dir.path());
    let account = store
        .account_by_handle("operator", "example.social")
        .unwrap()
        .expect("CLI-created account");
    assert_eq!(account.id.timestamp_ms() > 0, true);
    assert!(account.created_at_ms >= T0 - 1);
}

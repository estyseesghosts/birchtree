//! Configurable status text limit (`--max-status-chars=`).
//!
//! Default 500 (Mastodon parity); configurable up to 3000 (Misskey parity).
//! These tests drive the application use case directly with different limits.

mod common;

use birchtree::application::statuses::{
    self, ABS_MAX_STATUS_LENGTH, CreateStatusCommand, DEFAULT_MAX_STATUS_LENGTH,
};
use birchtree::domain::Id;
use birchtree::persistence::redb::RedbStore;

use common::{T0, mk_account, temp_store};

const BASE: &str = "https://example.social";

fn cmd(account: Id, text: &str) -> CreateStatusCommand {
    CreateStatusCommand {
        account_id: account,
        text: Some(text.to_string()),
        in_reply_to_id: None,
        quoted_status_id: None,
        quote_approval_policy: None,
        sensitive: None,
        spoiler_text: None,
        visibility: Some("public".to_string()),
        language: None,
        media_ids: Vec::new(),
        poll: None,
        application_name: None,
        application_website: None,
        idempotency_key: None,
    }
}

fn post(store: &RedbStore, account: Id, text: &str, max_chars: usize) -> Result<Id, String> {
    statuses::create_status(store, BASE, T0, cmd(account, text), max_chars, "off", "off", None)
        .map(|r| r.status.id)
        .map_err(|e| e.to_string())
}

#[test]
fn default_limit_matches_mastodon_parity() {
    assert_eq!(DEFAULT_MAX_STATUS_LENGTH, 500);
    assert_eq!(ABS_MAX_STATUS_LENGTH, 3000);
}

#[test]
fn default_limit_rejects_2900_chars_with_mastodon_message() {
    let (_dir, store) = temp_store();
    let account = mk_account(&store, "alice").id;
    let text = "x".repeat(2900);
    let err = post(&store, account, &text, DEFAULT_MAX_STATUS_LENGTH).expect_err("must reject");
    assert!(
        err.contains("character limit of 500 exceeded"),
        "unexpected message: {err}"
    );
}

#[test]
fn raised_limit_accepts_2900_chars() {
    let (_dir, store) = temp_store();
    let account = mk_account(&store, "alice").id;
    let text = "x".repeat(2900);
    let id = post(&store, account, &text, 3000).expect("raised limit must accept");
    assert!(id.value() > 0);
}

#[test]
fn raised_limit_still_rejects_beyond_itself() {
    let (_dir, store) = temp_store();
    let account = mk_account(&store, "alice").id;
    let text = "x".repeat(3001);
    let err = post(&store, account, &text, 3000).expect_err("must reject");
    assert!(
        err.contains("character limit of 3000 exceeded"),
        "unexpected message: {err}"
    );
}

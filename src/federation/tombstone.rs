//! Remote tombstones: the Delete-beats-Create race (Phase 6).
//!
//! Mastodon files a `Tombstone` row when a `Delete` arrives, even when the
//! object was never seen (`delete.rb`: `Tombstone.find_or_create_by`, guarded
//! by the same-host check `non_matching_uri_hosts?`, under a per-URI lock so
//! a concurrent `Create` cannot slip in). A later `Create` for a tombstoned
//! URI is then dropped (`delete_arrived_first?`).
//!
//! Birchtree does the same, without the lock (each store call is one short
//! transaction; a `Create` that wins the race is caught the other way: the
//! `Delete` lands on the now-stored row and tombstones it):
//!
//! - [`file_remote_tombstone`]: a `Delete` for an object URI with no status
//!   row files a tombstone, same-host only (an actor may only tombstone its
//!   own host's objects). A URI that already has a row, or a tombstone, is a
//!   duplicate; another host's URI is ignored.
//! - [`ingest_checked`]: the `Create`-shaped ingest paths (inbox `Create`,
//!   `Update`-as-`Create`, quote instruments, object dereference) refuse a
//!   tombstoned URI instead of resurrecting it.
//!
//! A locally deleted status needs no remote tombstone: `delete_status` keeps
//! the `STATUS_BY_URI` mapping on the tombstoned row, so `ingest_note` finds
//! it and reports a duplicate. Our own deleted objects are served as
//! `Tombstone` documents (410) on the object route.

use activitypub_federation::protocol::verification::verify_domains_match;
use url::Url;

use crate::application::federation_inbound::{self as inbound, Applied, NoteIngest, RemotePoll};
use crate::application::ports::Store;
use crate::domain::Result;

/// The outcome of a tombstone-checked ingest.
#[derive(Debug)]
pub enum CheckedIngest {
    /// The URI is live: the ingest ran.
    Live(NoteIngest),
    /// The URI was deleted before it arrived: nothing was stored.
    Tombstoned,
}

/// File a tombstone for an object URI whose `Delete` found no status row.
///
/// `delete_outcome` is what `delete_note` returned for the URI: anything but
/// a `Duplicate` (unknown or already gone) passes through unchanged. A
/// duplicate with a status row still behind it (an already-tombstoned row)
/// stays a duplicate; a duplicate with no row files a tombstone when the
/// object lives on the deleting actor's host, and is ignored otherwise.
pub fn file_remote_tombstone(
    store: &dyn Store,
    object_uri: &str,
    actor: &Url,
    delete_outcome: Applied,
    now_ms: u64,
) -> Result<Applied> {
    if delete_outcome != Applied::Duplicate {
        return Ok(delete_outcome);
    }
    if store.status_by_uri(object_uri)?.is_some() {
        return Ok(Applied::Duplicate);
    }
    if store.remote_tombstone(object_uri)?.is_some() {
        return Ok(Applied::Duplicate);
    }
    let object_url = Url::parse(object_uri)
        .map_err(|e| crate::domain::Error::InvalidInput(format!("delete object is not a URL: {e}")))?;
    if verify_domains_match(&object_url, actor).is_err() {
        return Ok(Applied::Ignored("delete of an unknown object from another host".into()));
    }
    store.record_remote_tombstone(object_uri, now_ms)?;
    Ok(Applied::Applied)
}

/// Ingest a received Note unless its URI was deleted before it arrived.
///
/// A tombstoned URI reports `Tombstoned` (the caller logs it as a duplicate:
/// the delete already won); every other URI ingests as before.
pub fn ingest_checked(
    store: &dyn Store,
    note: inbound::RemoteNote,
    poll: Option<RemotePoll>,
    now_ms: u64,
) -> Result<CheckedIngest> {
    if store.remote_tombstone(&note.uri)?.is_some() {
        return Ok(CheckedIngest::Tombstoned);
    }
    Ok(CheckedIngest::Live(inbound::ingest_note(store, note, poll, now_ms)?))
}

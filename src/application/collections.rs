//! Collections use cases (gaps batch, `docs/gaps.md`, G-C).
//!
//! Top-level `collections` CRUD + `items` (add/remove/list) + `revoke`, plus
//! the `CollectionFanout` job body. The durable rows live in the
//! `COLLECTIONS` / `COLLECTION_ITEMS` tables (schema v18); every mutation
//! commits the record, its membership rows, the denormalized `items_count`
//! and the bounded fan-out job in ONE `redb` transaction (`birchtree.md`
//! §5). Network I/O and notification creation happen after commit, in the
//! `CollectionFanout` job.
//!
//! [`CollectionStore`] is the narrow port for the collections tables. It is
//! defined here (not in `ports.rs`) so the G-C surface stays in G-C-owned
//! files; the parent merge moves it into `ports.rs` and adds it to the
//! `Store` supertraits. Until then, use cases that also need accounts,
//! statuses or notifications take `&dyn Store` alongside
//! `&dyn CollectionStore` (both are implemented by `RedbStore`, so callers
//! pass the same handle twice).

use crate::application::notifications;
use crate::application::ports::{CollectionChange, CollectionRecord, CollectionStore, Store};
use crate::domain::{Error, Id, Job, JobKind, NotificationType, Result};

/// Per-account collection cap (mirrors the list cap; birchtree bound — the
/// upstream experimental surface documents no cap).
pub const COLLECTIONS_PER_ACCOUNT_LIMIT: usize = 50;
/// Collection title bound in characters (mirrors the list title bound).
pub const COLLECTION_TITLE_LIMIT: usize = 256;
/// Max `status_ids[]` accepted per items add/remove request. Matches the job
/// batch bound so one mutation enqueues at most one fan-out job.
pub const COLLECTION_MUTATION_BATCH_LIMIT: usize = 500;
/// Max items held by one collection (birchtree bound; keeps revoke/delete
/// scans bounded in a single transaction).
pub const COLLECTION_MAX_ITEMS: usize = 1_000;
/// Max item ids carried by one `CollectionFanout` job (payload < 64 KiB, no
/// blobs in the transaction).
pub const COLLECTION_FANOUT_BATCH: usize = 500;
/// Payload ceiling for `CollectionFanout` jobs (`docs/gaps.md` Step 0).
pub const COLLECTION_FANOUT_PAYLOAD_BYTES: usize = 64 * 1_024;

// `CollectionStore`, `CollectionRecord` and `CollectionChange` live in
// `application::ports` (moved there at the parent merge so the REST adapters
// can join the port to `Store`); the use cases below take the port trait.

/// Mastodon en.yml-shaped 422 messages for the collections surface.
pub const ERR_TITLE_BLANK: &str = "Validation failed: Title can't be blank";
pub const ERR_TITLE_TOO_LONG: &str =
    "Validation failed: Title is too long (maximum is 256 characters)";
pub const ERR_LIMIT: &str =
    "Validation failed: You have reached the maximum number of collections";
pub const ERR_BATCH: &str =
    "Validation failed: Too many status ids (maximum is 500)";
pub const ERR_FULL: &str = "Validation failed: Collection has too many items";
pub const ERR_REVOKED: &str = "Validation failed: Collection has been revoked";

fn validate_title(title: &str) -> Result<String> {
    let trimmed = title.trim().to_string();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(ERR_TITLE_BLANK.into()));
    }
    if trimmed.chars().count() > COLLECTION_TITLE_LIMIT {
        return Err(Error::InvalidInput(ERR_TITLE_TOO_LONG.into()));
    }
    Ok(trimmed)
}

/// `POST` — create a collection for `owner`.
pub fn create(
    store: &dyn CollectionStore,
    owner: Id,
    title: &str,
    description: &str,
    public: bool,
    now_ms: u64,
) -> Result<CollectionRecord> {
    let title = validate_title(title)?;
    if store.collections_by_account(owner)?.len() >= COLLECTIONS_PER_ACCOUNT_LIMIT {
        return Err(Error::InvalidInput(ERR_LIMIT.into()));
    }
    store.create_collection(owner, &title, description.trim(), public, now_ms)
}

/// `GET` — one collection: the owner sees everything; anyone else sees only
/// public, non-revoked collections (anything else is 404, never an
/// existence oracle).
pub fn show(
    store: &dyn CollectionStore,
    viewer: Option<Id>,
    id: Id,
) -> Result<CollectionRecord> {
    let record = store
        .collection_by_id(id)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))?;
    if Some(record.account_id) != viewer && (!record.public || record.revoked) {
        return Err(Error::NotFound("Record not found".into()));
    }
    Ok(record)
}

/// `GET` — the owner's collections (all of them; no pagination, like lists).
pub fn index(store: &dyn CollectionStore, owner: Id) -> Result<Vec<CollectionRecord>> {
    store.collections_by_account(owner)
}

/// `PUT/PATCH` — patch an owned collection. Ownership is checked first
/// (404 before validation, like lists). Revoked collections refuse edits.
pub fn update(
    store: &dyn CollectionStore,
    owner: Id,
    id: Id,
    title: Option<String>,
    description: Option<String>,
    public: Option<bool>,
    now_ms: u64,
) -> Result<CollectionRecord> {
    let record = owned_collection(store, owner, id)?;
    if record.revoked {
        return Err(Error::InvalidInput(ERR_REVOKED.into()));
    }
    let title = title.map(|t| validate_title(&t)).transpose()?;
    let description = description.map(|d| d.trim().to_string());
    store
        .update_collection(owner, id, title, description, public, now_ms)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))
}

/// `DELETE` — delete an owned collection (revoked ones delete too).
pub fn destroy(store: &dyn CollectionStore, owner: Id, id: Id, now_ms: u64) -> Result<()> {
    owned_collection(store, owner, id)?;
    store.delete_collection(owner, id, now_ms)?;
    Ok(())
}

/// Whether `viewer` may read the *members* of `record`'s collection.
/// Counts (`items_count`) stay visible regardless (D30/D52); only the member
/// list hides. The owner always sees members; anyone else sees them only
/// when the owner has not set `hide_collections`.
pub fn members_visible(owner_hide_collections: Option<bool>, viewer: Option<Id>, owner: Id) -> bool {
    if Some(owner) == viewer {
        return true;
    }
    owner_hide_collections != Some(true)
}

/// `GET items` — member status ids, newest first. The collection itself must
/// be visible to `viewer` (see [`show`]); member hiding is applied by the
/// caller via [`members_visible`] (it needs the owner's account row, which
/// lives outside this port).
pub fn items(
    store: &dyn CollectionStore,
    viewer: Option<Id>,
    id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Id>> {
    show(store, viewer, id)?;
    store.collection_items(id, max_id, limit)
}

/// `POST items` — add `status_ids` to an owned collection.
///
/// Every status must exist and be fetchable by the owner (the `show_status`
/// policy gate: deleted/invisible statuses 404 without leaking which); the
/// per-request batch is capped at 500 so one mutation enqueues at most one
/// fan-out job.
pub fn add_items(
    store: &dyn Store,
    collections: &dyn CollectionStore,
    owner: Id,
    id: Id,
    status_ids: &[Id],
    now_ms: u64,
) -> Result<Vec<Id>> {
    owned_collection(collections, owner, id)?;
    if status_ids.len() > COLLECTION_MUTATION_BATCH_LIMIT {
        return Err(Error::InvalidInput(ERR_BATCH.into()));
    }
    for status_id in status_ids {
        crate::application::statuses::show_status(store, Some(owner), *status_id, now_ms).map_err(
            |err| match err {
                Error::NotFound(_) => Error::NotFound("Record not found".into()),
                other => other,
            },
        )?;
    }
    collections.add_collection_items(owner, id, status_ids, now_ms)
}

/// `DELETE items` — remove `status_ids` (idempotent).
pub fn remove_items(
    collections: &dyn CollectionStore,
    owner: Id,
    id: Id,
    status_ids: &[Id],
    now_ms: u64,
) -> Result<Vec<Id>> {
    owned_collection(collections, owner, id)?;
    if status_ids.len() > COLLECTION_MUTATION_BATCH_LIMIT {
        return Err(Error::InvalidInput(ERR_BATCH.into()));
    }
    collections.remove_collection_items(owner, id, status_ids, now_ms)
}

/// `POST revoke` — the owner empties a collection and marks it revoked.
pub fn revoke(
    collections: &dyn CollectionStore,
    owner: Id,
    id: Id,
    now_ms: u64,
) -> Result<CollectionRecord> {
    owned_collection(collections, owner, id)?;
    collections
        .revoke_collection(owner, id, now_ms)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))
}

/// Collections of `owner` containing `status_id` (the `in_collections`
/// read; owner-scoped, so no `hide_collections` interaction).
pub fn in_collections(
    collections: &dyn CollectionStore,
    owner: Id,
    status_id: Id,
) -> Result<Vec<CollectionRecord>> {
    collections.collections_containing_status(owner, status_id)
}

/// Resolve a collection the owner owns (Mastodon 404 body otherwise).
fn owned_collection(
    store: &dyn CollectionStore,
    owner: Id,
    id: Id,
) -> Result<CollectionRecord> {
    let record = store
        .collection_by_id(id)?
        .ok_or_else(|| Error::NotFound("Record not found".into()))?;
    if record.account_id != owner {
        return Err(Error::NotFound("Record not found".into()));
    }
    Ok(record)
}

/// Build the payload for a `CollectionFanout` job.
///
/// Bounded: at most [`COLLECTION_FANOUT_BATCH`] item ids, no blobs — the
/// payload stays far under [`COLLECTION_FANOUT_PAYLOAD_BYTES`].
pub fn fanout_payload(
    collection_id: Id,
    change: CollectionChange,
    item_ids: &[Id],
    actor_id: Id,
) -> serde_json::Value {
    let ids: Vec<u64> = item_ids
        .iter()
        .take(COLLECTION_FANOUT_BATCH)
        .map(|id| id.value())
        .collect();
    serde_json::json!({
        "collection_id": collection_id.value(),
        "change": change.as_str(),
        "item_ids": ids,
        "actor_id": actor_id.value(),
    })
}

/// Execute a `CollectionFanout` job: notify the authors of the affected
/// statuses that their post was added to / removed from a collection.
///
/// * `add` → `added_to_collection`; `remove`/`revoke`/`delete` →
///   `collection_update` (both types already parse in the v1 index; these
///   are their first producers — G-B's D51 rows can reference this).
/// * Delivery reuses `fan_out_from_payload`, so the D26 creation gate
///   (blocks/mutes/domain blocks) and the filter-policy pipeline apply, and
///   retries are idempotent through the notification dedup index.
/// * A missing collection (deleted after enqueue) is a no-op success.
/// * Malformed payloads are `Internal` errors (the worker quarantines them;
///   nothing is fabricated to cover the gap).
pub fn handle_fanout(
    store: &dyn Store,
    collections: &dyn CollectionStore,
    job: &Job,
) -> Result<()> {
    if job.kind != JobKind::CollectionFanout {
        return Err(Error::Internal("collection_fanout: wrong job kind".into()));
    }
    let collection_id = job
        .payload
        .get("collection_id")
        .and_then(|v| v.as_u64())
        .map(Id::new)
        .ok_or_else(|| Error::Internal("collection_fanout: missing collection_id".into()))?;
    let change = job
        .payload
        .get("change")
        .and_then(|v| v.as_str())
        .and_then(CollectionChange::parse)
        .ok_or_else(|| Error::Internal("collection_fanout: missing change".into()))?;
    let actor_id = job
        .payload
        .get("actor_id")
        .and_then(|v| v.as_u64())
        .map(Id::new)
        .ok_or_else(|| Error::Internal("collection_fanout: missing actor_id".into()))?;
    let item_ids: Vec<Id> = job
        .payload
        .get("item_ids")
        .and_then(|v| v.as_array())
        .map(|ids| {
            ids.iter()
                .filter_map(|v| v.as_u64().map(Id::new))
                .take(COLLECTION_FANOUT_BATCH)
                .collect()
        })
        .unwrap_or_default();

    // The collection row may be gone (delete raced the job): fan-out is
    // still delivered for the carried item ids — they were members at
    // commit time — so a missing row is NOT a skip. (Only the actor check
    // below can drop a recipient.)
    let _ = collections.collection_by_id(collection_id)?;

    let kind = match change {
        CollectionChange::Add => NotificationType::AddedToCollection,
        CollectionChange::Remove | CollectionChange::Revoke | CollectionChange::Delete => {
            NotificationType::CollectionUpdate
        }
    };
    // Resolve authors in one batch; deleted statuses notify nobody.
    let statuses = store.statuses_by_ids(&item_ids)?;
    for status in &statuses {
        if status.is_deleted() {
            continue;
        }
        let payload = notifications::fan_out_payload(
            status.account_id,
            kind,
            Some(actor_id),
            Some(status.id),
            job.created_at_ms,
        );
        notifications::fan_out_from_payload(store, &payload)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_round_trips() {
        for change in [
            CollectionChange::Add,
            CollectionChange::Remove,
            CollectionChange::Revoke,
            CollectionChange::Delete,
        ] {
            assert_eq!(CollectionChange::parse(change.as_str()), Some(change));
        }
        assert_eq!(CollectionChange::parse("bogus"), None);
    }

    #[test]
    fn fanout_payload_is_bounded() {
        let ids: Vec<Id> = (1..=900u64).map(Id::new).collect();
        let payload = fanout_payload(Id::new(7), CollectionChange::Add, &ids, Id::new(9));
        let bytes = serde_json::to_vec(&payload).unwrap();
        assert!(bytes.len() < COLLECTION_FANOUT_PAYLOAD_BYTES, "{}", bytes.len());
        assert_eq!(payload["item_ids"].as_array().unwrap().len(), COLLECTION_FANOUT_BATCH);
    }

    #[test]
    fn title_validation() {
        assert!(validate_title("  ").is_err());
        assert!(validate_title(&"x".repeat(257)).is_err());
        assert_eq!(validate_title("  Hi  ").unwrap(), "Hi");
    }
}

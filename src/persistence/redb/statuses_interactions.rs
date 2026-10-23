//! Status interactions: favourites, bookmarks, reblogs, pins and their
//! reverse lookups.
//!
//! Each mutating method opens exactly ONE `redb` write transaction; the row,
//! the counters and the follow-up job commit (or abort) together. Read
//! helpers (`favourited_by`, `reblogged_by`, `favourited_statuses`,
//! `bookmarked_statuses`) open one read transaction each.
//!
//! Helpers owned here (`reblog_links`, `PinRecord`) are imported by the
//! sibling `statuses_*` modules (`statuses_read` decodes `PinRecord` for the
//! pinned timeline); scan caps and row helpers come from `statuses_read`,
//! stats and fan-out from `statuses_create`. Nothing is duplicated.

use redb::ReadableTable;
use serde::{Deserialize, Serialize};

use crate::domain::bookmark::Bookmark;
use crate::domain::favourite::Favourite;
use crate::domain::{
    Account, Error, Id, NotificationType, QuotePolicy, Result, Status, Visibility,
};

use super::schema::{
    self, ACCOUNTS, BOOKMARKS, BOOKMARKS_BY_STATUS, FAVOURITES, FAVOURITES_BY_STATUS,
    REBLOGS_BY_ACCOUNT, STATUS_PINS, STATUS_REBLOG_INDEX, STATUSES,
};
use super::statuses_create::{enqueue_fan_out, read_stats_txn, write_stats_txn};
use super::statuses_read::{
    INTERACTION_SCAN_CAP, PINS_SCAN_CAP, ensure_id_unused, read_status_txn, write_status_txn,
};
use super::{RedbStore, decode, encode};

/// Stored shape of a pin row in [`STATUS_PINS`](super::schema::STATUS_PINS).
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PinRecord {
    pub(super) pinned_at_ms: u64,
}

/// A reblog wrapper's `uri`/`url`. The wrapper is the booster's own status,
/// so its links live on the booster's actor URI — never on the boosted
/// status's domain. (An activity id on another host than its actor is
/// refused by every domain-checking receiver, including ourselves.)
pub(super) fn reblog_links(booster_actor_uri: &str, reblog_id: Id) -> (String, String) {
    let base = booster_actor_uri.trim_end_matches('/');
    let uri = format!("{base}/statuses/{reblog_id}");
    (uri.clone(), uri)
}

/// Add a favourite (idempotent: already-favourited returns the status
/// unchanged). NotFound when the status is missing or not visible.
pub(super) fn favourite_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let mut status = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    // Idempotent: the pair key exists → already favourited, no counter change.
    let already = {
        let table = write.open_table(FAVOURITES)?;
        table
            .get(schema::pair_key(account_id.value(), status_id.value()).as_slice())?
            .is_some()
    };
    if already {
        return Ok(status);
    }

    let favourite = Favourite {
        id: store.next_id(),
        account_id,
        status_id,
        created_at_ms: now_ms,
    };
    {
        let mut table = write.open_table(FAVOURITES)?;
        table.insert(
            schema::pair_key(account_id.value(), status_id.value()).as_slice(),
            encode(&favourite)?.as_slice(),
        )?;
    }
    {
        let mut table = write.open_table(FAVOURITES_BY_STATUS)?;
        table.insert(
            schema::pair_key(status_id.value(), favourite.id.value()).as_slice(),
            account_id.value(),
        )?;
    }

    status.favourites_count = status.favourites_count.saturating_add(1);
    write_status_txn(&write, &status)?;
    if status.account_id != account_id {
        enqueue_fan_out(
            store,
            &write,
            status.account_id,
            NotificationType::Favourite,
            Some(account_id),
            Some(status_id),
            now_ms,
        )?;
    }

    super::federation_content::federation_event_txn(
        &write,
        store.next_id(),
        account_id,
        serde_json::json!({"event": "like", "status_id": status_id.value()}),
        now_ms,
    )?;
    write.commit()?;
    Ok(status)
}

/// Remove a favourite (no-op when absent), returning the status.
pub(super) fn unfavourite_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    _now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let mut status = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    let favourite: Option<Favourite> = {
        let table = write.open_table(FAVOURITES)?;
        match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
            Some(guard) => Some(decode(guard.value())?),
            None => None,
        }
    };
    if let Some(favourite) = favourite {
        {
            let mut table = write.open_table(FAVOURITES)?;
            table.remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
        }
        {
            let mut table = write.open_table(FAVOURITES_BY_STATUS)?;
            table.remove(schema::pair_key(status_id.value(), favourite.id.value()).as_slice())?;
        }
        status.favourites_count = status.favourites_count.saturating_sub(1);
        write_status_txn(&write, &status)?;
        super::federation_content::federation_event_txn(
            &write,
            store.next_id(),
            account_id,
            serde_json::json!({"event": "unlike", "status_id": status_id.value()}),
            _now_ms,
        )?;
        // Notification deletion belongs to the notification read surface
        // (Phase 4); the interaction never rewrites notification rows here.
    }

    write.commit()?;
    Ok(status)
}

/// Add a bookmark (idempotent), returning the status.
pub(super) fn bookmark_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let status = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    let already = {
        let table = write.open_table(BOOKMARKS)?;
        table
            .get(schema::pair_key(account_id.value(), status_id.value()).as_slice())?
            .is_some()
    };
    if already {
        return Ok(status);
    }

    let bookmark = Bookmark {
        id: store.next_id(),
        account_id,
        status_id,
        created_at_ms: now_ms,
    };
    {
        let mut table = write.open_table(BOOKMARKS)?;
        table.insert(
            schema::pair_key(account_id.value(), status_id.value()).as_slice(),
            encode(&bookmark)?.as_slice(),
        )?;
    }
    {
        let mut table = write.open_table(BOOKMARKS_BY_STATUS)?;
        table.insert(
            schema::pair_key(status_id.value(), bookmark.id.value()).as_slice(),
            account_id.value(),
        )?;
    }

    write.commit()?;
    Ok(status)
}

/// Remove a bookmark (no-op when absent), returning the status.
pub(super) fn unbookmark_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    _now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let status = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    let bookmark: Option<Bookmark> = {
        let table = write.open_table(BOOKMARKS)?;
        match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
            Some(guard) => Some(decode(guard.value())?),
            None => None,
        }
    };
    if let Some(bookmark) = bookmark {
        {
            let mut table = write.open_table(BOOKMARKS)?;
            table.remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
        }
        {
            let mut table = write.open_table(BOOKMARKS_BY_STATUS)?;
            table.remove(schema::pair_key(status_id.value(), bookmark.id.value()).as_slice())?;
        }
    }

    write.commit()?;
    Ok(status)
}

/// Create (or return the existing) reblog by `account_id` of `status_id`.
// WHY single-txn: one begin_write owns the reblog row + counters + follow-up job atomically
// (architecture rule); splitting would move work across the commit boundary.
pub(super) fn reblog_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    visibility: Visibility,
    now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let mut target = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    // Idempotent: return the viewer's existing live reblog. A tombstoned
    // reblog (after an unreblog) does not satisfy the request — fall
    // through and mint a fresh one, overwriting the account->target map.
    let existing_id: Option<Id> = {
        let table = write.open_table(REBLOGS_BY_ACCOUNT)?;
        match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
            Some(guard) => Some(Id::new(guard.value())),
            None => None,
        }
    };
    if let Some(reblog_id) = existing_id
        && let Some(row) = read_status_txn(&write, reblog_id)?
        && !row.is_deleted()
    {
        return Ok(row);
    }

    let reblog_id = store.next_id();
    // The wrapper's links live on the booster's actor URI (see
    // `reblog_links`); the target's links stay untouched.
    let booster_actor_uri = {
        let accounts = write.open_table(ACCOUNTS)?;
        match accounts.get(account_id.value())? {
            Some(guard) => {
                let account: Account = decode(guard.value())?;
                account.actor_uri
            }
            None => return Err(Error::NotFound("reblogging account".into())),
        }
    };
    let (uri, url) = reblog_links(&booster_actor_uri, reblog_id);
    let reblog = Status {
        id: reblog_id,
        account_id,
        uri,
        url,
        source_text: String::new(),
        content_html: String::new(),
        created_at_ms: now_ms,
        edited_at_ms: None,
        deleted_at_ms: None,
        language: None,
        visibility,
        sensitive: false,
        spoiler_text: None,
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: Some(target.id),
        media_ids: Vec::new(),
        place_id: None,
        poll_id: None,
        quote_policy: QuotePolicy::Nobody,
        mentions: Vec::new(),
        silent_mentions: Vec::new(),
        tags: Vec::new(),
        application: None,
        replies_count: 0,
        reblogs_count: 0,
        favourites_count: 0,
        quotes_count: 0,
    };
    ensure_id_unused(&write, STATUSES, reblog_id)?;
    write_status_txn(&write, &reblog)?;
    {
        let mut table = write.open_table(STATUS_REBLOG_INDEX)?;
        table.insert(
            schema::pair_key(target.id.value(), reblog_id.value()).as_slice(),
            reblog_id.value(),
        )?;
    }
    {
        let mut table = write.open_table(REBLOGS_BY_ACCOUNT)?;
        table.insert(
            schema::pair_key(account_id.value(), target.id.value()).as_slice(),
            reblog_id.value(),
        )?;
    }

    target.reblogs_count = target.reblogs_count.saturating_add(1);
    write_status_txn(&write, &target)?;

    let mut stats = read_stats_txn(&write, account_id)?;
    stats.statuses_count = stats.statuses_count.saturating_add(1);
    stats.last_status_at_ms = Some(stats.last_status_at_ms.map_or(now_ms, |t| t.max(now_ms)));
    write_stats_txn(&write, account_id, &stats)?;

    if target.account_id != account_id {
        enqueue_fan_out(
            store,
            &write,
            target.account_id,
            NotificationType::Reblog,
            Some(account_id),
            Some(target.id),
            now_ms,
        )?;
    }

    super::federation_content::federation_event_txn(
        &write,
        store.next_id(),
        account_id,
        serde_json::json!({"event": "announce", "status_id": target.id.value()}),
        now_ms,
    )?;
    write.commit()?;
    Ok(reblog)
}

/// Delete the viewer's reblog of `status_id`, returning the original
/// (counters decremented; no-op when the viewer never reblogged).
pub(super) fn unreblog_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let mut target = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    // Never reblogged → no-op returning the original.
    let reblog_id: Option<Id> = {
        let table = write.open_table(REBLOGS_BY_ACCOUNT)?;
        match table.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())? {
            Some(guard) => Some(Id::new(guard.value())),
            None => None,
        }
    };
    let Some(reblog_id) = reblog_id else {
        return Ok(target);
    };
    let Some(mut reblog) = read_status_txn(&write, reblog_id)? else {
        return Ok(target);
    };
    if reblog.is_deleted() {
        // Self-heal a stale mapping (tombstoned wrapper, row still present).
        write
            .open_table(REBLOGS_BY_ACCOUNT)?
            .remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
        write.commit()?;
        return Ok(target);
    }

    reblog.deleted_at_ms = Some(now_ms);
    write_status_txn(&write, &reblog)?;
    target.reblogs_count = target.reblogs_count.saturating_sub(1);
    write_status_txn(&write, &target)?;

    // Drop the account->reblog mapping with the wrapper: `reblogged` flips
    // back to false, and a later re-reblog mints a fresh wrapper (the old
    // row stays tombstoned for referential integrity).
    write
        .open_table(REBLOGS_BY_ACCOUNT)?
        .remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;

    let mut stats = read_stats_txn(&write, reblog.account_id)?;
    stats.statuses_count = stats.statuses_count.saturating_sub(1);
    write_stats_txn(&write, reblog.account_id, &stats)?;

    super::federation_content::federation_event_txn(
        &write,
        store.next_id(),
        account_id,
        serde_json::json!({"event": "undo_announce", "status_id": status_id.value(), "announce_uri": reblog.uri.clone()}),
        now_ms,
    )?;
    write.commit()?;
    Ok(target)
}

/// Pin own status (limit enforced by the use case), returning the status.
pub(super) fn pin_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let status = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;
    if status.account_id != account_id {
        // `StatusPinValidator` adds the `ownership` error; Mastodon wraps
        // it via `RecordInvalid` -> 422.
        return Err(Error::InvalidInput(
            "Validation failed: Someone else's post cannot be pinned".to_string(),
        ));
    }
    if status.is_reblog() {
        return Err(Error::InvalidInput(
            "Validation failed: A boost cannot be pinned".to_string(),
        ));
    }
    if status.visibility == Visibility::Direct {
        return Err(Error::InvalidInput(
            "Validation failed: Posts that are only visible to mentioned users cannot be pinned"
                .to_string(),
        ));
    }

    // Idempotent: already pinned.
    let already = {
        let pins = write.open_table(STATUS_PINS)?;
        pins.get(schema::pair_key(account_id.value(), status_id.value()).as_slice())?
            .is_some()
    };
    if already {
        return Ok(status);
    }

    // Pin limit (Mastodon allows 5).
    let mut count = 0usize;
    {
        let pins = write.open_table(STATUS_PINS)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        for entry in pins.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, _value) = entry?;
            count += 1;
            if count >= PINS_SCAN_CAP {
                break;
            }
        }
    }
    if count >= 5 {
        return Err(Error::InvalidInput(
            "Validation failed: You have already pinned the maximum number of posts".to_string(),
        ));
    }

    let record = PinRecord {
        pinned_at_ms: now_ms,
    };
    {
        let mut pins = write.open_table(STATUS_PINS)?;
        pins.insert(
            schema::pair_key(account_id.value(), status_id.value()).as_slice(),
            encode(&record)?.as_slice(),
        )?;
    }

    write.commit()?;
    Ok(status)
}

/// Unpin own status (no-op when not pinned), returning the status.
pub(super) fn unpin_status_impl(
    store: &RedbStore,
    account_id: Id,
    status_id: Id,
    _now_ms: u64,
) -> Result<Status> {
    let write = store.begin_write()?;

    let status =
        read_status_txn(&write, status_id)?.ok_or_else(|| Error::NotFound("status".to_string()))?;
    if status.account_id != account_id {
        return Err(Error::NotFound("status".to_string()));
    }

    {
        let mut pins = write.open_table(STATUS_PINS)?;
        pins.remove(schema::pair_key(account_id.value(), status_id.value()).as_slice())?;
    }

    write.commit()?;
    Ok(status)
}

/// Update only the quote approval policy — no edit snapshot and no
/// `edited_at` bump (mirrors Mastodon's interaction-policy endpoint).
/// NotFound when missing/deleted; the caller enforces ownership.
pub(super) fn set_quote_policy_impl(
    store: &RedbStore,
    status_id: Id,
    policy: QuotePolicy,
) -> Result<Status> {
    let write = store.begin_write()?;

    let mut status = read_status_txn(&write, status_id)?
        .filter(|s| !s.is_deleted())
        .ok_or_else(|| Error::NotFound("status".to_string()))?;

    // Private/direct statuses can never be quotable, regardless of the
    // requested policy (mirrors the downgrade in the use case/serializer).
    status.quote_policy = match status.visibility {
        Visibility::Private | Visibility::Direct => QuotePolicy::Nobody,
        _ => policy,
    };
    write_status_txn(&write, &status)?;

    write.commit()?;
    Ok(status)
}

/// Accounts that favourited `status_id`, newest first, as
/// `(account_id, favourite_id)` pairs; the interaction id is the cursor.
pub(super) fn favourited_by_impl(
    store: &RedbStore,
    status_id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<(Id, Id)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;
    let table = read.open_table(FAVOURITES_BY_STATUS)?;

    // `(status || favourite_id)` ascending → collect (bounded), reverse
    // for newest-first, then apply the cursor.
    let mut rows: Vec<(Id, Id)> = Vec::new();
    {
        let lo = schema::pair_key(status_id.value(), 0);
        let hi = schema::pair_key(status_id.value(), u64::MAX);
        for entry in table.range(lo.as_slice()..=hi.as_slice())? {
            let (key, value) = entry?;
            let bytes = key.value();
            let favourite_id = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
            rows.push((Id::new(value.value()), Id::new(favourite_id)));
            if rows.len() >= INTERACTION_SCAN_CAP {
                break;
            }
        }
    }
    rows.reverse();
    Ok(rows
        .into_iter()
        .filter(|(_, favourite_id)| max_id.is_none_or(|m| *favourite_id < m))
        .take(limit)
        .collect())
}

/// Accounts that reblogged `status_id`, newest first, as
/// `(account_id, reblog_status_id)` pairs; the reblog id is the cursor.
pub(super) fn reblogged_by_impl(
    store: &RedbStore,
    status_id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<(Id, Id)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;

    // `(status || reblog_id)` ascending → collect (bounded), reverse.
    let reblog_ids: Vec<Id> = {
        let index = read.open_table(STATUS_REBLOG_INDEX)?;
        let lo = schema::pair_key(status_id.value(), 0);
        let hi = schema::pair_key(status_id.value(), u64::MAX);
        let mut ids = Vec::new();
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            ids.push(Id::new(value.value()));
            if ids.len() >= INTERACTION_SCAN_CAP {
                break;
            }
        }
        ids
    };

    let mut out = Vec::new();
    let statuses = read.open_table(STATUSES)?;
    for reblog_id in reblog_ids.into_iter().rev() {
        if max_id.is_some_and(|m| reblog_id >= m) {
            continue;
        }
        if out.len() >= limit {
            break;
        }
        let Some(guard) = statuses.get(reblog_id.value())? else {
            continue;
        };
        let reblog: Status = decode(guard.value())?;
        if reblog.is_deleted() {
            continue;
        }
        out.push((reblog.account_id, reblog_id));
    }
    Ok(out)
}

/// The viewer's favourites, newest first (cursor = favourite row id).
pub(super) fn favourited_statuses_impl(
    store: &RedbStore,
    account_id: Id,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Favourite>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;
    let table = read.open_table(FAVOURITES)?;
    let lo = schema::pair_key(account_id.value(), 0);
    let hi = schema::pair_key(account_id.value(), u64::MAX);
    let mut rows: Vec<Favourite> = Vec::new();
    for entry in table.range(lo.as_slice()..=hi.as_slice())? {
        let (_key, value) = entry?;
        rows.push(decode(value.value())?);
        if rows.len() >= INTERACTION_SCAN_CAP {
            break;
        }
    }
    rows.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(rows
        .into_iter()
        .filter(|fav| max_id.is_none_or(|m| fav.id < m))
        .filter(|fav| since_id.is_none_or(|m| fav.id > m))
        .filter(|fav| min_id.is_none_or(|m| fav.id > m))
        .take(limit)
        .collect())
}

/// The viewer's bookmarks, newest first (cursor = bookmark row id).
pub(super) fn bookmarked_statuses_impl(
    store: &RedbStore,
    account_id: Id,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Bookmark>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;
    let table = read.open_table(BOOKMARKS)?;
    let lo = schema::pair_key(account_id.value(), 0);
    let hi = schema::pair_key(account_id.value(), u64::MAX);
    let mut rows: Vec<Bookmark> = Vec::new();
    for entry in table.range(lo.as_slice()..=hi.as_slice())? {
        let (_key, value) = entry?;
        rows.push(decode(value.value())?);
        if rows.len() >= INTERACTION_SCAN_CAP {
            break;
        }
    }
    rows.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(rows
        .into_iter()
        .filter(|mark| max_id.is_none_or(|m| mark.id < m))
        .filter(|mark| since_id.is_none_or(|m| mark.id > m))
        .filter(|mark| min_id.is_none_or(|m| mark.id > m))
        .take(limit)
        .collect())
}

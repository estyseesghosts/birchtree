//! Durable remote-cache metadata + accounting (`docs/database.md` Step 0,
//! parent merge of Agent B's snippet).
//!
//! `REMOTE_CACHE` holds one JSON [`RemoteCacheEntry`][entry] per
//! `(source_url, variant)` (see [`schema::remote_cache_key`]); the value's
//! `blob_key` is the per-row store key (index ≠ bytes — no sharing, no
//! refcount in v1). `CACHE_ACCOUNTING` holds the stored-byte counters
//! (`"total"`, `"class:<name>"`); every mapping change updates the counters
//! in the same transaction. In-flight budget lives only in the process
//! ledger ([`CacheLedger`][ledger]); the durable counters report `0` for
//! in-flight (a restart drops reservations by design).
//!
//! [entry]: crate::media::remote_cache::RemoteCacheEntry
//! [ledger]: crate::media::remote_cache::CacheLedger
//! [`schema::remote_cache_key`]: crate::persistence::redb::schema::remote_cache_key

use redb::ReadableTable;

use crate::application::federation_ports::FederationContentStore;
use crate::application::ports::{BlobStore, CacheStore, MediaStore};
use crate::domain::{Error, Id, Result};
use crate::media::blob_store::FsBlobStore;
use crate::media::reconciler::ReconStore;
use crate::media::remote_cache::{self, CacheUsage, MediaClass, RemoteCacheEntry};

use super::schema::{self, CACHE_ACCOUNTING, REMOTE_CACHE};
use super::RedbStore;

/// Accounting key for the stored-byte total.
const TOTAL_KEY: &str = "total";

/// Accounting key for one class's stored bytes.
fn class_key(class: MediaClass) -> &'static str {
    match class {
        MediaClass::AvatarEmoji => "class:avatar_emoji",
        MediaClass::Preview => "class:preview",
        MediaClass::Full => "class:full",
        MediaClass::Stream => "class:stream",
    }
}

fn decode_counter(guard: Option<redb::AccessGuard<'_, &[u8]>>) -> Result<u64> {
    match guard {
        Some(g) => Ok(super::decode::<u64>(g.value())?),
        None => Ok(0),
    }
}

impl CacheStore for RedbStore {
    fn cache_get(&self, source_url: &str, variant: &str) -> Result<Option<RemoteCacheEntry>> {
        let read = self.begin_read()?;
        let table = read.open_table(REMOTE_CACHE)?;
        let key = schema::remote_cache_key(source_url, variant);
        match table.get(key.as_slice())? {
            Some(guard) => Ok(Some(super::decode::<RemoteCacheEntry>(guard.value())?)),
            None => Ok(None),
        }
    }

    fn cache_put_metadata(&self, entry: RemoteCacheEntry) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(REMOTE_CACHE)?;
            let key = entry.cache_key();
            // Decode into an owned value first: the guard borrows the table,
            // so no insert may happen while it is alive.
            let existing: Option<RemoteCacheEntry> = match table.get(key.as_slice())? {
                Some(guard) => Some(super::decode(guard.value())?),
                None => None,
            };
            match existing {
                // Never clobber live bytes with a shell: refresh validators.
                Some(mut existing) if existing.has_bytes => {
                    existing.etag = entry.etag.clone();
                    existing.last_use_ms = existing.last_use_ms.max(entry.last_use_ms);
                    table.insert(key.as_slice(), super::encode(&existing)?.as_slice())?;
                }
                _ => {
                    let mut shell = entry.clone();
                    shell.has_bytes = false;
                    shell.size = 0;
                    table.insert(key.as_slice(), super::encode(&shell)?.as_slice())?;
                }
            }
        }
        write.commit()?;
        Ok(())
    }

    fn cache_mark_stored(
        &self,
        source_url: &str,
        variant: &str,
        blob_key: &str,
        size: u64,
        etag: Option<String>,
        now_ms: u64,
    ) -> Result<()> {
        // Durable local media must never enter the evictable ledger.
        if !remote_cache::is_cache_key(blob_key) {
            return Err(Error::InvalidInput(format!(
                "refusing non-cache key in the remote cache: {blob_key}"
            )));
        }
        let write = self.begin_write()?;
        {
            let mut cache = write.open_table(REMOTE_CACHE)?;
            let mut counters = write.open_table(CACHE_ACCOUNTING)?;
            let key = schema::remote_cache_key(source_url, variant);
            let old_size = match cache.get(key.as_slice())? {
                Some(guard) => {
                    let old: RemoteCacheEntry = super::decode(guard.value())?;
                    if old.has_bytes { old.size } else { 0 }
                }
                None => 0,
            };
            let class = crate::media::remote_cache::class_for_variant(variant);
            let mut entry = RemoteCacheEntry::stored(
                source_url,
                variant,
                class,
                blob_key,
                size,
                now_ms,
                &crate::config::RemoteCacheConfig::default(),
            );
            entry.etag = etag;
            cache.insert(key.as_slice(), super::encode(&entry)?.as_slice())?;
            // Counters move with the mapping, saturating both ways.
            let total = decode_counter(counters.get(TOTAL_KEY)?)?;
            let class_total = decode_counter(counters.get(class_key(class))?)?;
            let total = total.saturating_add(size).saturating_sub(old_size);
            // Recompute the class counter from the same delta (never negative).
            let class_total = if size >= old_size {
                class_total.saturating_add(size - old_size)
            } else {
                class_total.saturating_sub(old_size - size)
            };
            counters.insert(TOTAL_KEY, super::encode(&total)?.as_slice())?;
            counters.insert(class_key(class), super::encode(&class_total)?.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn cache_mark_evicted(&self, source_url: &str, variant: &str) -> Result<Option<String>> {
        let write = self.begin_write()?;
        let out = {
            let mut cache = write.open_table(REMOTE_CACHE)?;
            let mut counters = write.open_table(CACHE_ACCOUNTING)?;
            let key = schema::remote_cache_key(source_url, variant);
            let existing: Option<RemoteCacheEntry> = match cache.get(key.as_slice())? {
                Some(guard) => Some(super::decode(guard.value())?),
                None => None,
            };
            match existing {
                Some(mut entry) if entry.has_bytes => {
                    let blob_key = entry.blob_key.clone();
                    let freed = entry.size;
                    entry.has_bytes = false;
                    entry.size = 0;
                    cache.insert(key.as_slice(), super::encode(&entry)?.as_slice())?;
                    let total = decode_counter(counters.get(TOTAL_KEY)?)?;
                    let class_total = decode_counter(counters.get(class_key(entry.class))?)?;
                    counters.insert(
                        TOTAL_KEY,
                        super::encode(&total.saturating_sub(freed))?.as_slice(),
                    )?;
                    counters.insert(
                        class_key(entry.class),
                        super::encode(&class_total.saturating_sub(freed))?.as_slice(),
                    )?;
                    Some(blob_key)
                }
                _ => None,
            }
        };
        write.commit()?;
        Ok(out)
    }

    fn cache_touch(&self, source_url: &str, variant: &str, now_ms: u64) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut cache = write.open_table(REMOTE_CACHE)?;
            let key = schema::remote_cache_key(source_url, variant);
            let existing: Option<RemoteCacheEntry> = match cache.get(key.as_slice())? {
                Some(guard) => Some(super::decode(guard.value())?),
                None => None,
            };
            if let Some(mut entry) = existing {
                entry.last_use_ms = now_ms;
                cache.insert(key.as_slice(), super::encode(&entry)?.as_slice())?;
            }
        }
        write.commit()?;
        Ok(())
    }

    fn cache_accounting(&self) -> Result<CacheUsage> {
        let read = self.begin_read()?;
        let counters = read.open_table(CACHE_ACCOUNTING)?;
        Ok(CacheUsage {
            stored_total: decode_counter(counters.get(TOTAL_KEY)?)?,
            stored_avatar_emoji: decode_counter(counters.get(class_key(MediaClass::AvatarEmoji))?)?,
            stored_preview: decode_counter(counters.get(class_key(MediaClass::Preview))?)?,
            stored_full: decode_counter(counters.get(class_key(MediaClass::Full))?)?,
            // In-flight budget is process-ledger state (restart drops it).
            inflight_total: 0,
            inflight_avatar_emoji: 0,
            inflight_preview: 0,
            inflight_full: 0,
        })
    }

    fn cache_evict_batch(
        &self,
        need: MediaClass,
        bytes_to_free: u64,
        now_ms: u64,
        cap: usize,
    ) -> Result<Vec<String>> {
        let cap = cap.clamp(1, 500);
        if bytes_to_free == 0 {
            return Ok(Vec::new());
        }
        // Candidate classes: the need class first, then the rest by recency
        // pressure — never the avatar reserve unless the need *is* avatar.
        let mut classes = vec![need];
        for class in MediaClass::all() {
            if class == need {
                continue;
            }
            if need != MediaClass::AvatarEmoji && class == MediaClass::AvatarEmoji {
                continue;
            }
            classes.push(class);
        }
        // One read transaction over the byte-holding rows. The batch that is
        // *written* below is bounded by `cap`. Rows that are not cache keys
        // are never victims, so durable media cannot be reached from here.
        let candidates: Vec<(Vec<u8>, RemoteCacheEntry)> = {
            let read = self.begin_read()?;
            let table = read.open_table(REMOTE_CACHE)?;
            let mut out = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let Some(row) = decode_row(value.value()) else {
                    continue;
                };
                if row.has_bytes && remote_cache::is_cache_key(&row.blob_key) {
                    out.push((key.value().to_vec(), row));
                }
            }
            out
        };
        // Expired first (`false` sorts before `true`), then LRU, per class.
        let mut victims: Vec<Vec<u8>> = Vec::new();
        let mut freed = 0u64;
        for class in &classes {
            let mut in_class: Vec<&(Vec<u8>, RemoteCacheEntry)> = candidates
                .iter()
                .filter(|(_, e)| e.class == *class)
                .collect();
            in_class.sort_by_key(|(_, e)| (!e.is_expired(now_ms), e.last_use_ms));
            for (key, row) in in_class {
                if freed >= bytes_to_free {
                    break;
                }
                victims.push(key.clone());
                freed = freed.saturating_add(row.size);
            }
            if freed >= bytes_to_free {
                break;
            }
        }
        victims.truncate(cap);
        if victims.is_empty() {
            return Ok(Vec::new());
        }
        // One write txn: mapping + accounting together; caller deletes keys.
        let write = self.begin_write()?;
        let mut blob_keys = Vec::new();
        {
            let mut cache = write.open_table(REMOTE_CACHE)?;
            let mut counters = write.open_table(CACHE_ACCOUNTING)?;
            let mut total = decode_counter(counters.get(TOTAL_KEY)?)?;
            let mut per_class: [u64; 4] = [
                decode_counter(counters.get(class_key(MediaClass::AvatarEmoji))?)?,
                decode_counter(counters.get(class_key(MediaClass::Preview))?)?,
                decode_counter(counters.get(class_key(MediaClass::Full))?)?,
                0,
            ];
            for key in &victims {
                let existing: Option<RemoteCacheEntry> = match cache.get(key.as_slice())? {
                    Some(guard) => Some(super::decode(guard.value())?),
                    None => None,
                };
                let Some(mut entry) = existing else {
                    continue;
                };
                if !entry.has_bytes {
                    continue;
                }
                let freed = entry.size;
                total = total.saturating_sub(freed);
                let idx = match entry.class {
                    MediaClass::AvatarEmoji => 0,
                    MediaClass::Preview => 1,
                    MediaClass::Full => 2,
                    MediaClass::Stream => 3,
                };
                per_class[idx] = per_class[idx].saturating_sub(freed);
                blob_keys.push(entry.blob_key.clone());
                entry.has_bytes = false;
                entry.size = 0;
                cache.insert(key.as_slice(), super::encode(&entry)?.as_slice())?;
            }
            counters.insert(TOTAL_KEY, super::encode(&total)?.as_slice())?;
            counters.insert(
                class_key(MediaClass::AvatarEmoji),
                super::encode(&per_class[0])?.as_slice(),
            )?;
            counters.insert(
                class_key(MediaClass::Preview),
                super::encode(&per_class[1])?.as_slice(),
            )?;
            counters.insert(
                class_key(MediaClass::Full),
                super::encode(&per_class[2])?.as_slice(),
            )?;
        }
        write.commit()?;
        Ok(blob_keys)
    }
}


/// Decode one stored row; a corrupted row is `None` (callers skip it, and
/// [`RedbStore::cache_accounting_rebuild`] removes it).
fn decode_row(bytes: &[u8]) -> Option<RemoteCacheEntry> {
    super::decode::<RemoteCacheEntry>(bytes).ok()
}

/// Class index into the four-slot counter array.
fn class_slot(class: MediaClass) -> usize {
    match class {
        MediaClass::AvatarEmoji => 0,
        MediaClass::Preview => 1,
        MediaClass::Full => 2,
        MediaClass::Stream => 3,
    }
}

/// What a [`RedbStore::cache_accounting_rebuild`] pass changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountingRepair {
    /// Stored bytes the counters claimed before the rebuild (zero when the
    /// counters were unreadable).
    pub before_total: u64,
    /// Stored bytes after the rebuild (the sum of the byte-holding rows).
    pub after_total: u64,
    /// Rows that could not be decoded and were removed.
    pub corrupt_rows_removed: usize,
    /// Byte-holding rows that point outside the cache namespace and were
    /// detached from the ledger.
    pub foreign_rows_detached: usize,
}

impl RedbStore {
    /// One page of ledger rows in table order, strictly after `after`.
    /// Corrupted rows are skipped. The cursor is the raw table key, so a
    /// caller can resume a scan across batches (startup restore).
    pub fn cache_rows_page(
        &self,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, RemoteCacheEntry)>> {
        let limit = limit.clamp(1, 500);
        let read = self.begin_read()?;
        let table = read.open_table(REMOTE_CACHE)?;
        let mut out = Vec::new();
        let range = match after {
            Some(cursor) => table.range::<&[u8]>((
                std::ops::Bound::Excluded(cursor),
                std::ops::Bound::Unbounded,
            ))?,
            None => table.range::<&[u8]>(..)?,
        };
        for entry in range {
            let (key, value) = entry?;
            if let Some(row) = decode_row(value.value()) {
                out.push((key.value().to_vec(), row));
            }
            if out.len() >= limit {
                break;
            }
        }
        self.note_scanned(out.len());
        Ok(out)
    }

    /// Evict byte-holding rows past their TTL, oldest first, at most `cap`
    /// per call (one write transaction: mappings and counters together).
    /// Returns the blob keys the caller must now delete from the cache pool.
    pub fn cache_expire_batch(&self, now_ms: u64, cap: usize) -> Result<Vec<String>> {
        let cap = cap.clamp(1, 500);
        let mut expired: Vec<(u64, Vec<u8>)> = Vec::new();
        {
            let read = self.begin_read()?;
            let table = read.open_table(REMOTE_CACHE)?;
            for entry in table.iter()? {
                let (key, value) = entry?;
                let Some(row) = decode_row(value.value()) else {
                    continue;
                };
                if row.is_expired(now_ms) && remote_cache::is_cache_key(&row.blob_key) {
                    expired.push((row.expiry_ms, key.value().to_vec()));
                }
            }
        }
        expired.sort();
        expired.truncate(cap);
        if expired.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<Vec<u8>> = expired.into_iter().map(|(_, key)| key).collect();
        self.evict_rows(&keys)
    }

    /// Clear the byte mapping of the given ledger rows in one transaction,
    /// moving the counters with them. Returns the blob keys to delete.
    fn evict_rows(&self, keys: &[Vec<u8>]) -> Result<Vec<String>> {
        let write = self.begin_write()?;
        let mut blob_keys = Vec::new();
        {
            let mut cache = write.open_table(REMOTE_CACHE)?;
            let mut counters = write.open_table(CACHE_ACCOUNTING)?;
            let mut total = decode_counter(counters.get(TOTAL_KEY).ok().flatten()).unwrap_or(0);
            let mut per_class: [u64; 4] = [0; 4];
            for class in MediaClass::all() {
                per_class[class_slot(class)] =
                    decode_counter(counters.get(class_key(class)).ok().flatten()).unwrap_or(0);
            }
            for key in keys {
                let existing: Option<RemoteCacheEntry> = match cache.get(key.as_slice())? {
                    Some(guard) => decode_row(guard.value()),
                    None => None,
                };
                let Some(mut entry) = existing else {
                    continue;
                };
                if !entry.has_bytes {
                    continue;
                }
                total = total.saturating_sub(entry.size);
                let slot = class_slot(entry.class);
                per_class[slot] = per_class[slot].saturating_sub(entry.size);
                blob_keys.push(entry.blob_key.clone());
                entry.has_bytes = false;
                entry.size = 0;
                cache.insert(key.as_slice(), super::encode(&entry)?.as_slice())?;
            }
            counters.insert(TOTAL_KEY, super::encode(&total)?.as_slice())?;
            for class in [MediaClass::AvatarEmoji, MediaClass::Preview, MediaClass::Full] {
                counters.insert(
                    class_key(class),
                    super::encode(&per_class[class_slot(class)])?.as_slice(),
                )?;
            }
        }
        write.commit()?;
        Ok(blob_keys)
    }

    /// Recompute the stored-byte counters from the ledger rows themselves
    /// (the rows are the source of truth; the counters are a cache of their
    /// sum). Corrupted rows are removed and rows pointing outside the cache
    /// namespace are detached first. Idempotent; one write transaction.
    pub fn cache_accounting_rebuild(&self) -> Result<AccountingRepair> {
        let mut repair = AccountingRepair {
            before_total: self.cache_accounting().map(|usage| usage.stored_total).unwrap_or(0),
            ..AccountingRepair::default()
        };
        let write = self.begin_write()?;
        {
            let mut cache = write.open_table(REMOTE_CACHE)?;
            let mut counters = write.open_table(CACHE_ACCOUNTING)?;
            let mut total = 0u64;
            let mut per_class: [u64; 4] = [0; 4];
            let mut corrupt: Vec<Vec<u8>> = Vec::new();
            let mut detach: Vec<(Vec<u8>, RemoteCacheEntry)> = Vec::new();
            for entry in cache.iter()? {
                let (key, value) = entry?;
                match decode_row(value.value()) {
                    None => corrupt.push(key.value().to_vec()),
                    Some(row) if row.has_bytes && !remote_cache::is_cache_key(&row.blob_key) => {
                        detach.push((key.value().to_vec(), row));
                    }
                    Some(row) if row.has_bytes => {
                        total = total.saturating_add(row.size);
                        let slot = class_slot(row.class);
                        per_class[slot] = per_class[slot].saturating_add(row.size);
                    }
                    Some(_) => {}
                }
            }
            for key in &corrupt {
                cache.remove(key.as_slice())?;
            }
            repair.corrupt_rows_removed = corrupt.len();
            repair.foreign_rows_detached = detach.len();
            for (key, mut row) in detach {
                row.has_bytes = false;
                row.size = 0;
                cache.insert(key.as_slice(), super::encode(&row)?.as_slice())?;
            }
            counters.insert(TOTAL_KEY, super::encode(&total)?.as_slice())?;
            for class in [MediaClass::AvatarEmoji, MediaClass::Preview, MediaClass::Full] {
                counters.insert(
                    class_key(class),
                    super::encode(&per_class[class_slot(class)])?.as_slice(),
                )?;
            }
            repair.after_total = total;
        }
        write.commit()?;
        Ok(repair)
    }

    /// The ledger row that claims `blob_key`, with its table key.
    ///
    /// Attachment bytes use the deterministic key `remote/<media id>`, so the
    /// fast path is two point lookups (media id -> source URL -> row); any
    /// other key falls back to a scan of the ledger.
    pub fn cache_row_by_blob_key(
        &self,
        blob_key: &str,
    ) -> Result<Option<(Vec<u8>, RemoteCacheEntry)>> {
        if let Some(id) = blob_key
            .strip_prefix("remote/")
            .and_then(|digits| digits.parse::<u64>().ok())
            && let Some(source) = self.remote_media_source(Id::new(id))?
            && let Some(row) = self.cache_get(&source, "original")?
            && row.blob_key == blob_key
        {
            return Ok(Some((row.cache_key(), row)));
        }
        let read = self.begin_read()?;
        let table = read.open_table(REMOTE_CACHE)?;
        for entry in table.iter()? {
            let (key, value) = entry?;
            if let Some(row) = decode_row(value.value())
                && row.blob_key == blob_key
            {
                return Ok(Some((key.value().to_vec(), row)));
            }
        }
        Ok(None)
    }

    /// The attachment row that references `blob_key` (as its file, preview
    /// or cover), with the ledger variant that key is accounted under: bytes
    /// that are in use even when the ledger lost track of them.
    pub fn media_references_blob(&self, blob_key: &str) -> Result<Option<(Id, &'static str)>> {
        let Some(rest) = blob_key.strip_prefix("remote/") else {
            return Ok(None);
        };
        let digits: &str = rest.split('.').next().unwrap_or_default();
        let Ok(id) = digits.parse::<u64>() else {
            return Ok(None);
        };
        let Some(media) = self.media_by_id(Id::new(id))? else {
            return Ok(None);
        };
        let variant: Option<&'static str> = if media.file_key == blob_key {
            Some("original")
        } else if media.preview_key.as_deref() == Some(blob_key) {
            Some("preview")
        } else if media.thumbnail_key.as_deref() == Some(blob_key) {
            Some("thumbnail")
        } else {
            None
        };
        Ok(variant.map(|variant| (media.id, variant)))
    }

    fn recon_pool(&self) -> Result<&FsBlobStore> {
        self.cache_pool()
            .ok_or_else(|| Error::Internal("cache pool is not attached to the store".into()))
    }
}

/// The reconciler's view of the remote cache: ledger rows (`REMOTE_CACHE`)
/// against the bytes in the **cache** pool. The durable pool is never
/// visible here, so an orphan sweep can only ever delete cache bytes.
///
/// A blob counts as claimed when a byte-holding ledger row names it, or when
/// a committed attachment row references it (in-use bytes the ledger lost
/// track of are adopted by the maintenance pass, never deleted). Blobs
/// younger than the store's orphan grace period are not listed at all.
impl ReconStore for RedbStore {
    fn list_blob_keys(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>> {
        self.recon_pool()?.list_keys(after, limit, self.orphan_grace())
    }

    fn list_meta_keys(&self, after: Option<&str>, limit: usize) -> Result<Vec<String>> {
        let read = self.begin_read()?;
        let table = read.open_table(REMOTE_CACHE)?;
        let mut keys: Vec<String> = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let Some(row) = decode_row(value.value()) else {
                continue;
            };
            if row.has_bytes && after.is_none_or(|cursor| row.blob_key.as_str() > cursor) {
                keys.push(row.blob_key);
            }
        }
        keys.sort();
        keys.truncate(limit);
        Ok(keys)
    }

    fn meta_has(&self, key: &str) -> Result<bool> {
        if let Some((_, row)) = self.cache_row_by_blob_key(key)?
            && row.has_bytes
        {
            return Ok(true);
        }
        Ok(self.media_references_blob(key)?.is_some())
    }

    fn blob_exists(&self, key: &str) -> Result<bool> {
        self.recon_pool()?.exists(key)
    }

    fn meta_is_stale(&self, key: &str, now_ms: u64) -> Result<Option<bool>> {
        Ok(self
            .cache_row_by_blob_key(key)?
            .map(|(_, row)| row.is_expired(now_ms)))
    }

    fn meta_is_flagged_missing(&self, key: &str) -> Result<Option<bool>> {
        Ok(self
            .cache_row_by_blob_key(key)?
            .map(|(_, row)| !row.has_bytes))
    }

    fn remove_blob(&self, key: &str) -> Result<()> {
        if !remote_cache::is_cache_key(key) {
            return Err(Error::InvalidInput(format!(
                "refusing to delete a non-cache key: {key}"
            )));
        }
        self.recon_pool()?.delete(key)
    }

    fn remove_meta(&self, key: &str) -> Result<()> {
        if let Some((table_key, _)) = self.cache_row_by_blob_key(key)? {
            self.evict_rows(&[table_key])?;
        }
        Ok(())
    }

    fn flag_missing(&self, key: &str, _now_ms: u64) -> Result<()> {
        // Bytes are gone: drop the byte mapping (the shell with the source
        // URL stays, so the next request refetches) and fix the counters.
        self.remove_meta(key)
    }

    fn list_pending_deletes(&self, _after: Option<&str>, _limit: usize) -> Result<Vec<String>> {
        // Eviction commits the mapping change first and deletes the blob
        // second, so an interrupted delete leaves an orphan blob (found by
        // the orphan scan), not a marker row.
        Ok(Vec::new())
    }

    fn clear_pending_delete(&self, _key: &str) -> Result<()> {
        Ok(())
    }
}

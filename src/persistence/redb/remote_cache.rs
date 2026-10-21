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

use crate::application::ports::CacheStore;
use crate::domain::Result;
use crate::media::remote_cache::{CacheUsage, MediaClass, RemoteCacheEntry};

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
        // Bounded forward scan (one read txn): at most `cap` byte-holding rows.
        let candidates: Vec<(Vec<u8>, RemoteCacheEntry)> = {
            let read = self.begin_read()?;
            let table = read.open_table(REMOTE_CACHE)?;
            let mut out = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let row: RemoteCacheEntry = super::decode(value.value())?;
                if row.has_bytes {
                    out.push((key.value().to_vec(), row));
                }
                if out.len() >= cap {
                    break;
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

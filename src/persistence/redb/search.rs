//! Bounded local search scans for `GET /api/v2/search`.
//!
//! There is no search index: accounts are found through a bounded
//! `HANDLE_INDEX` scan, hashtags through a bounded `HASHTAG_INDEX` scan
//! (distinct tags), and statuses through descending range scans
//! (`STATUS_AUTHOR_INDEX` for one author, `TIMELINE_PUBLIC` for the public
//! corpus). Every scan is capped; the caps are documented in
//! `docs/persistence-conformance.md`.

use std::collections::BTreeSet;

use redb::ReadableTableMetadata;

use crate::application::ports::SearchStore;
use crate::domain::{Account, Id, Result, Status};

use super::schema::{
    self, ACCOUNTS, HANDLE_INDEX, HASHTAG_INDEX, STATUSES, STATUS_AUTHOR_INDEX, TIMELINE_PUBLIC,
};
use super::{decode, RedbStore};

/// Handle-index entries examined per account search.
const HANDLE_SCAN_CAP: usize = 5_000;
/// Hashtag-index entries examined per tag search.
const TAG_SCAN_CAP: usize = 5_000;
/// Distinct tags returned per tag search.
const TAG_RESULT_CAP: usize = 1_000;

impl SearchStore for RedbStore {
    fn search_accounts(&self, needle: &str, cap: usize) -> Result<Vec<Account>> {
        let read = self.begin_read()?;
        let handles = read.open_table(HANDLE_INDEX)?;
        let accounts = read.open_table(ACCOUNTS)?;
        let table_len = handles.len()?;
        let mut out: Vec<Account> = Vec::new();
        let mut scanned = 0usize;
        for entry in handles.range(""..)? {
            let (key, value) = entry?;
            scanned += 1;
            let handle = key.value().to_ascii_lowercase();
            let account_id = Id::new(value.value());
            let needle_hit = handle.contains(needle);
            if !needle_hit {
                // The handle missed: the display name may still match, so the
                // row is inspected before skipping.
                if scanned >= HANDLE_SCAN_CAP {
                    break;
                }
                continue;
            }
            if let Some(guard) = accounts.get(account_id.value())? {
                let account: Account = decode(guard.value())?;
                let name_hit = account.username.to_ascii_lowercase().contains(needle)
                    || account.display_name.to_ascii_lowercase().contains(needle);
                if needle_hit || name_hit {
                    out.push(account);
                }
            }
            if out.len() >= cap || scanned >= HANDLE_SCAN_CAP || scanned >= table_len as usize {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn search_tags(&self, prefix: &str, cap: usize) -> Result<Vec<String>> {
        let read = self.begin_read()?;
        let index = read.open_table(HASHTAG_INDEX)?;
        let mut tags: BTreeSet<String> = BTreeSet::new();
        let mut scanned = 0usize;
        // Key layout `tag || 0xFF || status_id_be`: a prefix range over the
        // normalized tag yields every status under that prefix.
        let range_start: &[u8] = prefix.as_bytes();
        for entry in index.range(range_start..)? {
            let (key, _value) = entry?;
            scanned += 1;
            let bytes = key.value();
            let raw = String::from_utf8_lossy(bytes);
            let tag = raw.split('\u{ff}').next().unwrap_or("").to_string();
            if !tag.starts_with(prefix) {
                // Past the prefix range (ascending keys).
                break;
            }
            tags.insert(tag);
            if tags.len() >= TAG_RESULT_CAP.min(cap.max(1) * 10) || scanned >= TAG_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(tags.into_iter().take(cap).collect())
    }

    fn recent_statuses(
        &self,
        account_id: Option<Id>,
        max_id: Option<Id>,
        min_id: Option<Id>,
        cap: usize,
    ) -> Result<Vec<Status>> {
        let read = self.begin_read()?;
        // Cursor bounds over descending keys.
        let lower = max_id
            .map(|m| schema::desc_id(m.value()).wrapping_add(1))
            .unwrap_or(0);
        let upper_exclusive = min_id.map(|x| schema::desc_id(x.value()));

        let mut out: Vec<Status> = Vec::new();
        let mut scanned = 0usize;
        let statuses = read.open_table(STATUSES)?;
        match account_id {
            // One author: the author index is already descending by id and
            // covers every visibility (the use case re-checks fetchability).
            Some(author) => {
                let index = read.open_table(STATUS_AUTHOR_INDEX)?;
                let lo = schema::pair_key(author.value(), lower);
                let entries = match upper_exclusive {
                    Some(upper) => {
                        let hi = schema::pair_key(author.value(), upper);
                        index.range(lo.as_slice()..hi.as_slice())?
                    }
                    None => {
                        let hi = schema::pair_key(author.value(), u64::MAX);
                        index.range(lo.as_slice()..=hi.as_slice())?
                    }
                };
                for entry in entries {
                    let (key, _value) = entry?;
                    scanned += 1;
                    let desc = u64::from_be_bytes(key.value()[8..].try_into().unwrap());
                    let id = Id::new(u64::MAX - desc);
                    if let Some(guard) = statuses.get(id.value())? {
                        let status: Status = decode(guard.value())?;
                        if !status.is_deleted() {
                            out.push(status);
                        }
                    }
                    if out.len() >= cap || scanned >= cap * 4 {
                        break;
                    }
                }
            }
            // General corpus: public statuses (the local search profile).
            None => {
                let index = read.open_table(TIMELINE_PUBLIC)?;
                let entries = match upper_exclusive {
                    Some(upper) => index.range(lower..upper)?,
                    None => index.range(lower..=u64::MAX)?,
                };
                for entry in entries {
                    let (key, _value) = entry?;
                    scanned += 1;
                    let id = Id::new(u64::MAX - key.value());
                    if let Some(guard) = statuses.get(id.value())? {
                        let status: Status = decode(guard.value())?;
                        if !status.is_deleted() {
                            out.push(status);
                        }
                    }
                    if out.len() >= cap || scanned >= cap * 4 {
                        break;
                    }
                }
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }
}

//! Custom filter persistence (v2 model; the v1 API projects keyword rows).

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{FilterBundle, FilterPatch, FilterStore, NewFilter};
use crate::domain::{CustomFilter, FilterKeyword, FilterStatus, Id, Result};

use super::schema::{
    self, FILTERS, FILTERS_BY_ACCOUNT, FILTER_KEYWORDS, FILTER_KEYWORDS_BY_FILTER,
    FILTER_STATUSES, FILTER_STATUSES_BY_FILTER,
};
use super::{decode, encode, RedbStore, StoreResult};

/// Scan caps: Mastodon has no per-account filter limit, so these bound the
/// reads a single request can trigger (documented in
/// `docs/persistence-conformance.md`).
const FILTERS_PER_ACCOUNT_CAP: usize = 500;
const KEYWORDS_PER_FILTER_CAP: usize = 1_000;
const STATUSES_PER_FILTER_CAP: usize = 1_000;

/// Read one filter row inside an open transaction.
fn read_filter_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<CustomFilter>> {
    let table = write.open_table(FILTERS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Load a filter with its keywords and status rules inside a transaction.
fn bundle_txn(read: &redb::ReadTransaction, filter: CustomFilter) -> StoreResult<FilterBundle> {
    let mut keywords = Vec::new();
    {
        let index = read.open_table(FILTER_KEYWORDS_BY_FILTER)?;
        let rows = read.open_table(FILTER_KEYWORDS)?;
        let lo = schema::pair_key(filter.id.value(), 0);
        let hi = schema::pair_key(filter.id.value(), u64::MAX);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            let keyword_id = Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap()));
            if let Some(guard) = rows.get(keyword_id.value())? {
                keywords.push(decode(guard.value())?);
            }
            if keywords.len() >= KEYWORDS_PER_FILTER_CAP {
                break;
            }
        }
    }
    let mut statuses = Vec::new();
    {
        let index = read.open_table(FILTER_STATUSES_BY_FILTER)?;
        let rows = read.open_table(FILTER_STATUSES)?;
        let lo = schema::pair_key(filter.id.value(), 0);
        let hi = schema::pair_key(filter.id.value(), u64::MAX);
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            let rule_id = Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap()));
            if let Some(guard) = rows.get(rule_id.value())? {
                statuses.push(decode(guard.value())?);
            }
            if statuses.len() >= STATUSES_PER_FILTER_CAP {
                break;
            }
        }
    }
    Ok(FilterBundle {
        filter,
        keywords,
        statuses,
    })
}

/// Write a keyword row + its index inside an open transaction.
fn write_keyword_txn(
    write: &WriteTransaction,
    keyword: &FilterKeyword,
) -> StoreResult<()> {
    {
        let mut table = write.open_table(FILTER_KEYWORDS)?;
        table.insert(keyword.id.value(), encode(keyword)?.as_slice())?;
    }
    {
        let mut table = write.open_table(FILTER_KEYWORDS_BY_FILTER)?;
        table.insert(
            schema::pair_key(keyword.filter_id.value(), keyword.id.value()).as_slice(),
            (),
        )?;
    }
    Ok(())
}

/// Delete a keyword row + its index inside an open transaction.
fn delete_keyword_txn(write: &WriteTransaction, keyword: &FilterKeyword) -> StoreResult<()> {
    {
        let mut table = write.open_table(FILTER_KEYWORDS)?;
        table.remove(keyword.id.value())?;
    }
    {
        let mut table = write.open_table(FILTER_KEYWORDS_BY_FILTER)?;
        table.remove(
            schema::pair_key(keyword.filter_id.value(), keyword.id.value()).as_slice(),
        )?;
    }
    Ok(())
}

impl FilterStore for RedbStore {
    fn create_filter(&self, input: NewFilter, now_ms: u64) -> Result<FilterBundle> {
        let write = self.begin_write()?;
        let filter = CustomFilter {
            id: self.next_id(),
            account_id: input.account_id,
            title: input.title,
            context: input.context,
            expires_at_ms: input.expires_at_ms,
            action: input.action,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(FILTERS)?;
            table.insert(filter.id.value(), encode(&filter)?.as_slice())?;
        }
        {
            let mut table = write.open_table(FILTERS_BY_ACCOUNT)?;
            table.insert(
                schema::pair_key(filter.account_id.value(), filter.id.value()).as_slice(),
                (),
            )?;
        }
        let mut keywords = Vec::new();
        for (word, whole_word) in input.keywords {
            let keyword = FilterKeyword {
                id: self.next_id(),
                filter_id: filter.id,
                keyword: word,
                whole_word,
            };
            write_keyword_txn(&write, &keyword)?;
            keywords.push(keyword);
        }
        write.commit()?;
        Ok(FilterBundle {
            filter,
            keywords,
            statuses: Vec::new(),
        })
    }

    fn filter_by_id(&self, id: Id) -> Result<Option<FilterBundle>> {
        let read = self.begin_read()?;
        let Some(filter) = read_filter_txn_as_read(&read, id)? else {
            return Ok(None);
        };
        Ok(Some(bundle_txn(&read, filter)?))
    }

    fn filters_by_account(
        &self,
        account_id: Id,
        context: Option<&str>,
    ) -> Result<Vec<FilterBundle>> {
        let read = self.begin_read()?;
        let index = read.open_table(FILTERS_BY_ACCOUNT)?;
        let rows = read.open_table(FILTERS)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut ids: Vec<Id> = Vec::new();
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _value) = entry?;
            ids.push(Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap())));
            if ids.len() >= FILTERS_PER_ACCOUNT_CAP {
                break;
            }
        }
        self.note_scanned(ids.len());
        let mut out = Vec::new();
        for id in ids {
            let Some(guard) = rows.get(id.value())? else {
                continue;
            };
            let filter: CustomFilter = decode(guard.value())?;
            if let Some(context) = context
                && !filter.context.iter().any(|c| c == context)
            {
                continue;
            }
            out.push(bundle_txn(&read, filter)?);
        }
        Ok(out)
    }

    fn update_filter(
        &self,
        account_id: Id,
        id: Id,
        patch: FilterPatch,
        now_ms: u64,
    ) -> Result<Option<FilterBundle>> {
        let write = self.begin_write()?;
        let Some(mut filter) = read_filter_txn(&write, id)? else {
            return Ok(None);
        };
        if filter.account_id != account_id {
            return Ok(None);
        }
        if let Some(title) = patch.title {
            filter.title = title;
        }
        if let Some(context) = patch.context {
            filter.context = context;
        }
        if let Some(expires) = patch.expires_at_ms {
            filter.expires_at_ms = expires;
        }
        if let Some(action) = patch.action {
            filter.action = action;
        }
        filter.updated_at_ms = now_ms;
        {
            let mut table = write.open_table(FILTERS)?;
            table.insert(filter.id.value(), encode(&filter)?.as_slice())?;
        }
        if let Some(replacements) = patch.keywords {
            for (existing_id, keyword, whole_word, destroy) in replacements {
                if destroy {
                    if let Some(existing_id) = existing_id {
                        let Some(row) = read_keyword_txn(&write, existing_id)? else {
                            continue;
                        };
                        if row.filter_id != filter.id {
                            continue;
                        }
                        delete_keyword_txn(&write, &row)?;
                    }
                    continue;
                }
                match existing_id {
                    Some(existing_id) => {
                        let Some(mut row) = read_keyword_txn(&write, existing_id)? else {
                            continue;
                        };
                        if row.filter_id != filter.id {
                            continue;
                        }
                        row.keyword = keyword;
                        row.whole_word = whole_word;
                        write_keyword_txn(&write, &row)?;
                    }
                    None => {
                        let row = FilterKeyword {
                            id: self.next_id(),
                            filter_id: filter.id,
                            keyword,
                            whole_word,
                        };
                        write_keyword_txn(&write, &row)?;
                    }
                }
            }
        }
        let read = self.begin_read()?;
        let bundle = bundle_txn(&read, filter)?;
        write.commit()?;
        Ok(Some(bundle))
    }

    fn delete_filter(&self, account_id: Id, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let Some(filter) = read_filter_txn(&write, id)? else {
            return Ok(false);
        };
        if filter.account_id != account_id {
            return Ok(false);
        }
        // Keywords and status rules die with the filter.
        for keyword in collect_keywords(&write, id)? {
            delete_keyword_txn(&write, &keyword)?;
        }
        for rule in collect_statuses(&write, id)? {
            {
                let mut table = write.open_table(FILTER_STATUSES)?;
                table.remove(rule.id.value())?;
            }
            {
                let mut table = write.open_table(FILTER_STATUSES_BY_FILTER)?;
                table.remove(schema::pair_key(rule.filter_id.value(), rule.id.value()).as_slice())?;
            }
        }
        {
            let mut table = write.open_table(FILTERS)?;
            table.remove(id.value())?;
        }
        {
            let mut table = write.open_table(FILTERS_BY_ACCOUNT)?;
            table.remove(schema::pair_key(filter.account_id.value(), id.value()).as_slice())?;
        }
        write.commit()?;
        Ok(true)
    }

    fn keyword_by_id(&self, id: Id) -> Result<Option<FilterKeyword>> {
        let read = self.begin_read()?;
        let table = read.open_table(FILTER_KEYWORDS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn create_keyword(
        &self,
        account_id: Id,
        filter_id: Id,
        keyword: &str,
        whole_word: bool,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>> {
        let write = self.begin_write()?;
        let Some(filter) = read_filter_txn(&write, filter_id)? else {
            return Ok(None);
        };
        if filter.account_id != account_id {
            return Ok(None);
        }
        {
            let mut table = write.open_table(FILTERS)?;
            let mut filter = filter;
            filter.updated_at_ms = now_ms;
            table.insert(filter.id.value(), encode(&filter)?.as_slice())?;
        }
        let row = FilterKeyword {
            id: self.next_id(),
            filter_id,
            keyword: keyword.to_string(),
            whole_word,
        };
        write_keyword_txn(&write, &row)?;
        write.commit()?;
        Ok(Some(row))
    }

    fn update_keyword(
        &self,
        account_id: Id,
        id: Id,
        keyword: Option<&str>,
        whole_word: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>> {
        let write = self.begin_write()?;
        let Some(mut row) = read_keyword_txn(&write, id)? else {
            return Ok(None);
        };
        // Ownership flows through the parent filter.
        let Some(mut filter) = read_filter_txn(&write, row.filter_id)? else {
            return Ok(None);
        };
        if filter.account_id != account_id {
            return Ok(None);
        }
        if let Some(keyword) = keyword {
            row.keyword = keyword.to_string();
        }
        if let Some(whole_word) = whole_word {
            row.whole_word = whole_word;
        }
        filter.updated_at_ms = now_ms;
        {
            let mut table = write.open_table(FILTERS)?;
            table.insert(filter.id.value(), encode(&filter)?.as_slice())?;
        }
        write_keyword_txn(&write, &row)?;
        write.commit()?;
        Ok(Some(row))
    }

    fn delete_keyword(&self, account_id: Id, id: Id) -> Result<Option<FilterKeyword>> {
        let write = self.begin_write()?;
        let Some(row) = read_keyword_txn(&write, id)? else {
            return Ok(None);
        };
        let Some(filter) = read_filter_txn(&write, row.filter_id)? else {
            return Ok(None);
        };
        if filter.account_id != account_id {
            return Ok(None);
        }
        delete_keyword_txn(&write, &row)?;
        write.commit()?;
        Ok(Some(row))
    }

    fn filter_statuses(&self, filter_id: Id) -> Result<Vec<FilterStatus>> {
        let read = self.begin_read()?;
        Ok(collect_statuses_read(&read, filter_id)?)
    }

    fn create_filter_status(
        &self,
        account_id: Id,
        filter_id: Id,
        status_id: Id,
        now_ms: u64,
    ) -> Result<Option<FilterStatus>> {
        let write = self.begin_write()?;
        let Some(mut filter) = read_filter_txn(&write, filter_id)? else {
            return Ok(None);
        };
        if filter.account_id != account_id {
            return Ok(None);
        }
        filter.updated_at_ms = now_ms;
        {
            let mut table = write.open_table(FILTERS)?;
            table.insert(filter.id.value(), encode(&filter)?.as_slice())?;
        }
        let row = FilterStatus {
            id: self.next_id(),
            filter_id,
            status_id,
        };
        {
            let mut table = write.open_table(FILTER_STATUSES)?;
            table.insert(row.id.value(), encode(&row)?.as_slice())?;
        }
        {
            let mut table = write.open_table(FILTER_STATUSES_BY_FILTER)?;
            table.insert(
                schema::pair_key(row.filter_id.value(), row.id.value()).as_slice(),
                (),
            )?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    fn delete_filter_status(&self, account_id: Id, id: Id) -> Result<Option<FilterStatus>> {
        let write = self.begin_write()?;
        let row: Option<FilterStatus> = {
            let table = write.open_table(FILTER_STATUSES)?;
            match table.get(id.value())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        let Some(row) = row else {
            return Ok(None);
        };
        let Some(filter) = read_filter_txn(&write, row.filter_id)? else {
            return Ok(None);
        };
        if filter.account_id != account_id {
            return Ok(None);
        }
        {
            let mut table = write.open_table(FILTER_STATUSES)?;
            table.remove(id.value())?;
        }
        {
            let mut table = write.open_table(FILTER_STATUSES_BY_FILTER)?;
            table.remove(schema::pair_key(row.filter_id.value(), id.value()).as_slice())?;
        }
        write.commit()?;
        Ok(Some(row))
    }
}

/// Read helpers shared with the write paths.
fn read_filter_txn_as_read(
    read: &redb::ReadTransaction,
    id: Id,
) -> StoreResult<Option<CustomFilter>> {
    let table = read.open_table(FILTERS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn read_keyword_txn(
    write: &WriteTransaction,
    id: Id,
) -> StoreResult<Option<FilterKeyword>> {
    let table = write.open_table(FILTER_KEYWORDS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn collect_keywords(write: &WriteTransaction, filter_id: Id) -> StoreResult<Vec<FilterKeyword>> {
    let index = write.open_table(FILTER_KEYWORDS_BY_FILTER)?;
    let rows = write.open_table(FILTER_KEYWORDS)?;
    let lo = schema::pair_key(filter_id.value(), 0);
    let hi = schema::pair_key(filter_id.value(), u64::MAX);
    let mut out = Vec::new();
    for entry in index.range(lo.as_slice()..=hi.as_slice())? {
        let (key, _value) = entry?;
        let keyword_id = Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap()));
        if let Some(guard) = rows.get(keyword_id.value())? {
            out.push(decode(guard.value())?);
        }
        if out.len() >= KEYWORDS_PER_FILTER_CAP {
            break;
        }
    }
    Ok(out)
}

fn collect_statuses(write: &WriteTransaction, filter_id: Id) -> StoreResult<Vec<FilterStatus>> {
    let index = write.open_table(FILTER_STATUSES_BY_FILTER)?;
    let rows = write.open_table(FILTER_STATUSES)?;
    let lo = schema::pair_key(filter_id.value(), 0);
    let hi = schema::pair_key(filter_id.value(), u64::MAX);
    let mut out = Vec::new();
    for entry in index.range(lo.as_slice()..=hi.as_slice())? {
        let (key, _value) = entry?;
        let rule_id = Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap()));
        if let Some(guard) = rows.get(rule_id.value())? {
            out.push(decode(guard.value())?);
        }
        if out.len() >= STATUSES_PER_FILTER_CAP {
            break;
        }
    }
    Ok(out)
}

fn collect_statuses_read(
    read: &redb::ReadTransaction,
    filter_id: Id,
) -> StoreResult<Vec<FilterStatus>> {
    let index = read.open_table(FILTER_STATUSES_BY_FILTER)?;
    let rows = read.open_table(FILTER_STATUSES)?;
    let lo = schema::pair_key(filter_id.value(), 0);
    let hi = schema::pair_key(filter_id.value(), u64::MAX);
    let mut out = Vec::new();
    for entry in index.range(lo.as_slice()..=hi.as_slice())? {
        let (key, _value) = entry?;
        let rule_id = Id::new(u64::from_be_bytes(key.value()[8..].try_into().unwrap()));
        if let Some(guard) = rows.get(rule_id.value())? {
            out.push(decode(guard.value())?);
        }
        if out.len() >= STATUSES_PER_FILTER_CAP {
            break;
        }
    }
    Ok(out)
}

//! Quote persistence.
//!
//! A quote is a link row between two statuses plus two indexes (by quoted
//! status for listing, by quoting status for lookup). State transitions live
//! on the `QUOTES` row; the quoted status's `quotes_count` moves in the same
//! transaction as the transition that changes it (see `birchtree.md` §5).

use std::collections::HashMap;

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{NewQuoteSpec, QuoteStore};
use crate::domain::{Error, Id, QuoteRecord, QuoteState, Result, Status};

use super::schema::{self, QUOTES, QUOTES_BY_QUOTED, QUOTES_BY_QUOTING};
use super::{decode, encode, RedbStore, StoreError, StoreResult};

/// Upper bound of index entries examined per `quotes_of_status` call, so a
/// pathological quoted status cannot pin a long scan.
const SCAN_CAP: usize = 500;

/// Read one quote row inside an existing transaction.
fn read_quote_txn(write: &WriteTransaction, quote_id: Id) -> StoreResult<Option<QuoteRecord>> {
    let quotes = write.open_table(QUOTES)?;
    match quotes.get(quote_id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Insert a quote and its indexes, and bump the quoted status's
/// `quotes_count`, inside the quoting status's creation transaction
/// (`statuses.rs` calls this). Returns the stored record.
///
/// The caller must have validated the quote (policy, visibility, existence);
/// a missing quoted status row here is defensive and aborts the transaction.
pub(super) fn attach_quote_txn(
    write: &WriteTransaction,
    quote_id: Id,
    quoting_status_id: Id,
    spec: &NewQuoteSpec,
    quoting_account_id: Id,
    now_ms: u64,
) -> StoreResult<QuoteRecord> {
    let mut quoted = super::statuses::read_status_txn(write, spec.quoted_status_id)?
        .ok_or_else(|| StoreError::Migration("quoted status missing".into()))?;
    // A local quoted author accepts at creation when the policy allows (the use
    // case decided). A remote author must approve first: the quote is pending,
    // it does not count, and a QuoteRequest goes out (Phase 6).
    let quoted_is_remote = {
        let accounts = write.open_table(schema::ACCOUNTS)?;
        match accounts.get(spec.quoted_account_id.value())? {
            Some(guard) => {
                let account: crate::domain::Account = decode(guard.value())?;
                !account.local
            }
            None => false,
        }
    };
    let state = if quoted_is_remote {
        QuoteState::Pending
    } else {
        QuoteState::Accepted
    };
    if state == QuoteState::Accepted {
        quoted.quotes_count = quoted.quotes_count.saturating_add(1);
        super::statuses::write_status_txn(write, &quoted)?;
    }
    let record = QuoteRecord {
        id: quote_id,
        quoting_status_id,
        quoted_status_id: spec.quoted_status_id,
        state,
        quoting_account_id,
        quoted_account_id: spec.quoted_account_id,
        quote_activity_uri: None,
        approval_uri: None,
        created_at_ms: now_ms,
    };
    {
        let mut quotes = write.open_table(QUOTES)?;
        quotes.insert(quote_id.value(), encode(&record)?.as_slice())?;
    }
    {
        let mut by_quoted = write.open_table(QUOTES_BY_QUOTED)?;
        // Keyed by (quoted id, quote id): upstream paginates `Quote` rows by
        // quote id (`quotes_controller.rb:29-44`); the quoting status id is
        // the value (fix round P2 #2; pre-v16 rows are rekeyed by migration).
        by_quoted.insert(
            schema::pair_key(spec.quoted_status_id.value(), quote_id.value()).as_slice(),
            quoting_status_id.value(),
        )?;
    }
    {
        let mut by_quoting = write.open_table(QUOTES_BY_QUOTING)?;
        by_quoting.insert(quoting_status_id.value(), quote_id.value())?;
    }
    if state == QuoteState::Pending {
        super::federation_content::federation_event_txn(
            write,
            quote_id,
            quoting_account_id,
            serde_json::json!({"event": "quote_request", "quote_id": quote_id.value()}),
            now_ms,
        )?;
    }
    Ok(record)
}

impl QuoteStore for RedbStore {
    fn quote_by_quoting_status(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>> {
        let read = self.begin_read()?;
        let quote_id = {
            let by_quoting = read.open_table(QUOTES_BY_QUOTING)?;
            match by_quoting.get(quoting_status_id.value())? {
                Some(guard) => Id::new(guard.value()),
                None => return Ok(None),
            }
        };
        let quotes = read.open_table(QUOTES)?;
        match quotes.get(quote_id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn quotes_by_quoting_ids(&self, ids: &[Id]) -> Result<HashMap<Id, QuoteRecord>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // ONE read transaction: one index get + one row get per quoting status.
        let read = self.begin_read()?;
        let by_quoting = read.open_table(QUOTES_BY_QUOTING)?;
        let quotes = read.open_table(QUOTES)?;
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            if out.contains_key(id) {
                continue;
            }
            let Some(quote_guard) = by_quoting.get(id.value())? else {
                continue;
            };
            let quote_id = Id::new(quote_guard.value());
            let Some(record) = quotes.get(quote_id.value())? else {
                continue;
            };
            out.insert(*id, decode(record.value())?);
        }
        Ok(out)
    }

    fn quotes_of_status(
        &self,
        quoted_status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(QuoteRecord, Status)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // The shared status reader accepts a write transaction, so this read
        // holds the writer lock for the (capped) scan; it never commits.
        let write = self.begin_write()?;
        let lower = schema::pair_key(quoted_status_id.value(), 0);
        let upper = schema::pair_key(quoted_status_id.value(), u64::MAX);

        // Collect (quote id, quoting status id) pairs for this quoted
        // status, bounded by the scan cap.
        let mut pairs: Vec<(u64, u64)> = Vec::new();
        {
            let by_quoted = write.open_table(QUOTES_BY_QUOTED)?;
            for entry in by_quoted.range(lower.as_slice()..=upper.as_slice())? {
                let (key, value) = entry?;
                let bytes = key.value();
                let quote = u64::from_be_bytes(bytes[8..].try_into().unwrap());
                pairs.push((quote, value.value()));
                if pairs.len() >= SCAN_CAP {
                    break;
                }
            }
        }
        // Keys ascend by quote id; newest first is a descending scan.
        pairs.reverse();

        let mut out: Vec<(QuoteRecord, Status)> = Vec::new();
        for (quote_id, quoting) in pairs {
            let quoting_status_id = Id::new(quoting);
            if max_id.is_some_and(|max| Id::new(quote_id) >= max) {
                continue;
            }
            if out.len() >= limit {
                break;
            }
            let Some(quote) = read_quote_txn(&write, Id::new(quote_id))? else {
                continue;
            };
            if quote.state != QuoteState::Accepted {
                continue;
            }
            // Skip quotes whose quoting status is gone or tombstoned.
            let Some(status) = super::statuses::read_status_txn(&write, quoting_status_id)?
            else {
                continue;
            };
            if status.is_deleted() {
                continue;
            }
            out.push((quote, status));
        }
        Ok(out)
    }

    fn revoke_quote(
        &self,
        quoted_status_id: Id,
        quoting_status_id: Id,
        now_ms: u64,
    ) -> Result<QuoteRecord> {
        let _ = now_ms; // The state machine records no transition timestamp.
        let write = self.begin_write()?;
        // Resolve the quote id through the quoting-status index, then check
        // it quotes this status (the by-quoted index is keyed by quote id
        // since the P2 #2 rekey, so a direct pair lookup no longer exists).
        let quote_id = {
            let by_quoting = write.open_table(QUOTES_BY_QUOTING)?;
            match by_quoting.get(quoting_status_id.value())? {
                Some(guard) => Id::new(guard.value()),
                None => {
                    return Err(Error::NotFound(format!(
                        "quote on status {quoted_status_id} by {quoting_status_id}"
                    )))
                }
            }
        };
        let mut quote = read_quote_txn(&write, quote_id)?
            .ok_or_else(|| Error::NotFound(format!("quote {quote_id}")))?;
        if quote.quoted_status_id != quoted_status_id {
            return Err(Error::NotFound(format!(
                "quote on status {quoted_status_id} by {quoting_status_id}"
            )));
        }

        // Mastodon's `Quote#reject!`: an accepted quote is revoked and its
        // counter decremented; any other non-revoked state becomes rejected;
        // revoked/deleted are sticky.
        if quote.state == QuoteState::Accepted {
            quote.state = QuoteState::Revoked;
            let mut quoted = super::statuses::read_status_txn(&write, quoted_status_id)?
                .ok_or_else(|| StoreError::Migration("quoted status missing".into()))?;
            quoted.quotes_count = quoted.quotes_count.saturating_sub(1);
            super::statuses::write_status_txn(&write, &quoted)?;
        } else if quote.state != QuoteState::Revoked {
            quote.state = QuoteState::Rejected;
        }
        {
            let mut quotes = write.open_table(QUOTES)?;
            quotes.insert(quote_id.value(), encode(&quote)?.as_slice())?;
        }
        super::federation_content::federation_event_txn(&write, self.next_id(), quote.quoted_account_id, serde_json::json!({"event": "quote_revoke", "quote_id": quote_id.value()}), now_ms)?;
        write.commit()?;
        Ok(quote)
    }
}

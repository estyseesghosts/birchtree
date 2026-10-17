//! Quote persistence.
//!
//! A quote is a link row between two statuses plus two indexes (by quoted
//! status for listing, by quoting status for lookup). State transitions live
//! on the `QUOTES` row; the quoted status's `quotes_count` moves in the same
//! transaction as the transition that changes it (see `birchtree.md` §5).

use redb::{ReadableDatabase, ReadableTable, WriteTransaction};

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
    quoted.quotes_count = quoted.quotes_count.saturating_add(1);
    super::statuses::write_status_txn(write, &quoted)?;

    // Local quotes are accepted at creation when the quoted status's policy
    // allows (the use case decides); federation states are Phase 6.
    let record = QuoteRecord {
        id: quote_id,
        quoting_status_id,
        quoted_status_id: spec.quoted_status_id,
        state: QuoteState::Accepted,
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
        by_quoted.insert(
            schema::pair_key(spec.quoted_status_id.value(), quoting_status_id.value()).as_slice(),
            quote_id.value(),
        )?;
    }
    {
        let mut by_quoting = write.open_table(QUOTES_BY_QUOTING)?;
        by_quoting.insert(quoting_status_id.value(), quote_id.value())?;
    }
    Ok(record)
}

impl QuoteStore for RedbStore {
    fn quote_by_quoting_status(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>> {
        let read = self.db().begin_read()?;
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
        let write = self.db().begin_write()?;
        let lower = schema::pair_key(quoted_status_id.value(), 0);
        let upper = schema::pair_key(quoted_status_id.value(), u64::MAX);

        // Collect (quoting status id, quote id) pairs for this quoted status,
        // bounded by the scan cap.
        let mut pairs: Vec<(u64, u64)> = Vec::new();
        {
            let by_quoted = write.open_table(QUOTES_BY_QUOTED)?;
            for entry in by_quoted.range(lower.as_slice()..=upper.as_slice())? {
                let (key, value) = entry?;
                let quoting = u64::from_be_bytes(key.value()[8..].try_into().unwrap());
                pairs.push((quoting, value.value()));
                if pairs.len() >= SCAN_CAP {
                    break;
                }
            }
        }
        // Keys ascend by quoting id; newest first is a descending scan.
        pairs.reverse();

        let mut out: Vec<(QuoteRecord, Status)> = Vec::new();
        for (quoting, quote_id) in pairs {
            let quoting_status_id = Id::new(quoting);
            if max_id.is_some_and(|max| quoting_status_id >= max) {
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
        let write = self.db().begin_write()?;
        let lookup = schema::pair_key(quoted_status_id.value(), quoting_status_id.value());
        let quote_id = {
            let by_quoted = write.open_table(QUOTES_BY_QUOTED)?;
            match by_quoted.get(lookup.as_slice())? {
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
        write.commit()?;
        Ok(quote)
    }
}

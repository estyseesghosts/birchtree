//! Federation content: statuses by URI, quote states from the wire, reply links,
//! reports, remote media staging, the thread-refresh cooldown, and the event
//! jobs that outbound federation reads. Each mutation commits in one `redb`
//! transaction with the job it causes (`birchtree.md` §5); no network I/O here.

use redb::{ReadableTable, WriteTransaction};

use crate::application::federation_ports::{FederationContentStore, NewQuote, NewReport};
use crate::domain::federation::RefreshRecord;
use crate::domain::{
    Account, Error, Id, Job, JobKind, MediaAttachment, QuoteRecord, QuoteState, Report, Result, Status,
};

use super::schema::{
    self, ACCOUNTS, ACTIVITY_DEDUP, QUOTES, QUOTES_BY_QUOTED, QUOTES_BY_QUOTING, QUOTE_BY_APPROVAL,
    REMOTE_MEDIA_SOURCE, REMOTE_REFRESH, REPORTS, STATUSES, STATUS_BY_URI, STATUS_PARENT_INDEX,
    UNRESOLVED_PARENT,
};
use super::{decode, encode, RedbStore, StoreResult};

/// Enqueue the federation event for a local actor's change, in the caller's
/// transaction. Remote actors cause no outbound event, so the call is a no-op
/// for them.
pub(super) fn federation_event_txn(
    write: &WriteTransaction,
    job_id: Id,
    actor_id: Id,
    event: serde_json::Value,
    now_ms: u64,
) -> StoreResult<()> {
    let local = {
        let accounts = write.open_table(ACCOUNTS)?;
        match accounts.get(actor_id.value())? {
            Some(guard) => {
                let account: Account = decode(guard.value())?;
                account.local
            }
            None => false,
        }
    };
    if !local {
        return Ok(());
    }
    // The sender travels with the event: the worker signs as this actor.
    let mut event = event;
    if let Some(object) = event.as_object_mut() {
        object.insert("actor_id".into(), serde_json::json!(actor_id.value()));
    }
    let job = Job::new(job_id, JobKind::FederationEvent, event, now_ms, now_ms);
    super::jobs::insert_job(write, &job)
}

fn status_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<Status>> {
    let table = write.open_table(STATUSES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn status_by_id_read(read: &redb::ReadTransaction, id: Id) -> StoreResult<Option<Status>> {
    let table = read.open_table(STATUSES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn quote_txn(write: &WriteTransaction, id: Id) -> StoreResult<Option<QuoteRecord>> {
    let table = write.open_table(QUOTES)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

impl FederationContentStore for RedbStore {
    fn status_by_uri(&self, uri: &str) -> Result<Option<Status>> {
        let read = self.begin_read()?;
        let by_uri = read.open_table(STATUS_BY_URI)?;
        let Some(id) = by_uri.get(uri)?.map(|g| g.value()) else {
            return Ok(None);
        };
        let statuses = read.open_table(STATUSES)?;
        match statuses.get(id)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn record_activity(&self, activity_uri: &str, now_ms: u64) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut dedup = write.open_table(ACTIVITY_DEDUP)?;
            dedup.insert(activity_uri, now_ms)?;
        }
        write.commit()?;
        Ok(())
    }

    fn activity_recorded(&self, activity_uri: &str) -> Result<bool> {
        let read = self.begin_read()?;
        let table = read.open_table(ACTIVITY_DEDUP)?;
        Ok(table.get(activity_uri)?.is_some())
    }

    fn quote_by_approval_uri(&self, approval_uri: &str) -> Result<Option<QuoteRecord>> {
        let read = self.begin_read()?;
        let by_approval = read.open_table(QUOTE_BY_APPROVAL)?;
        let Some(id) = by_approval.get(approval_uri)?.map(|g| g.value()) else {
            return Ok(None);
        };
        let quotes = read.open_table(QUOTES)?;
        match quotes.get(id)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn quote_by_quoting(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>> {
        let read = self.begin_read()?;
        let by_quoting = read.open_table(QUOTES_BY_QUOTING)?;
        let Some(id) = by_quoting.get(quoting_status_id.value())?.map(|g| g.value()) else {
            return Ok(None);
        };
        let quotes = read.open_table(QUOTES)?;
        match quotes.get(id)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn quote_by_id(&self, quote_id: Id) -> Result<Option<QuoteRecord>> {
        let read = self.begin_read()?;
        let quotes = read.open_table(QUOTES)?;
        match quotes.get(quote_id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn report_by_id(&self, report_id: Id) -> Result<Option<Report>> {
        let read = self.begin_read()?;
        let reports = read.open_table(REPORTS)?;
        match reports.get(report_id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn create_quote(&self, input: NewQuote, now_ms: u64) -> Result<QuoteRecord> {
        let write = self.begin_write()?;
        let quote_id = self.next_id();
        let record = QuoteRecord {
            id: quote_id,
            quoting_status_id: input.quoting_status_id,
            quoted_status_id: input.quoted_status_id,
            state: input.state,
            quoting_account_id: input.quoting_account_id,
            quoted_account_id: input.quoted_account_id,
            quote_activity_uri: input.quote_activity_uri.clone(),
            approval_uri: input.approval_uri.clone(),
            created_at_ms: now_ms,
        };
        {
            let mut quotes = write.open_table(QUOTES)?;
            quotes.insert(quote_id.value(), encode(&record)?.as_slice())?;
        }
        {
            let mut by_quoted = write.open_table(QUOTES_BY_QUOTED)?;
            by_quoted.insert(
                schema::pair_key(input.quoted_status_id.value(), input.quoting_status_id.value()).as_slice(),
                quote_id.value(),
            )?;
        }
        {
            let mut by_quoting = write.open_table(QUOTES_BY_QUOTING)?;
            by_quoting.insert(input.quoting_status_id.value(), quote_id.value())?;
        }
        if let Some(approval) = &input.approval_uri {
            let mut by_approval = write.open_table(QUOTE_BY_APPROVAL)?;
            by_approval.insert(approval.as_str(), quote_id.value())?;
        }
        if input.state == QuoteState::Accepted {
            let mut quoted = status_txn(&write, input.quoted_status_id)?
                .ok_or_else(|| Error::NotFound("quoted status".into()))?;
            quoted.quotes_count = quoted.quotes_count.saturating_add(1);
            super::statuses::write_status_txn(&write, &quoted)?;
        }
        write.commit()?;
        Ok(record)
    }

    fn set_quote_state(
        &self,
        quote_id: Id,
        state: QuoteState,
        approval_uri: Option<String>,
        quote_activity_uri: Option<String>,
    ) -> Result<QuoteRecord> {
        let write = self.begin_write()?;
        let mut quote = quote_txn(&write, quote_id)?.ok_or_else(|| Error::NotFound(format!("quote {quote_id}")))?;
        let was = quote.state;
        // Revoked and deleted are sticky: a later Accept or Reject does not revive them.
        if matches!(was, QuoteState::Revoked | QuoteState::Deleted) && state != was {
            drop(write);
            return Ok(quote);
        }
        quote.state = state;
        if approval_uri.is_some() {
            quote.approval_uri = approval_uri.clone();
        }
        if quote_activity_uri.is_some() {
            quote.quote_activity_uri = quote_activity_uri;
        }
        if let Some(approval) = &approval_uri {
            let mut by_approval = write.open_table(QUOTE_BY_APPROVAL)?;
            by_approval.insert(approval.as_str(), quote_id.value())?;
        }
        if was != QuoteState::Accepted && state == QuoteState::Accepted {
            let mut quoted = status_txn(&write, quote.quoted_status_id)?
                .ok_or_else(|| Error::NotFound("quoted status".into()))?;
            quoted.quotes_count = quoted.quotes_count.saturating_add(1);
            super::statuses::write_status_txn(&write, &quoted)?;
        } else if was == QuoteState::Accepted && state != QuoteState::Accepted {
            let mut quoted = status_txn(&write, quote.quoted_status_id)?
                .ok_or_else(|| Error::NotFound("quoted status".into()))?;
            quoted.quotes_count = quoted.quotes_count.saturating_sub(1);
            super::statuses::write_status_txn(&write, &quoted)?;
        }
        {
            let mut quotes = write.open_table(QUOTES)?;
            quotes.insert(quote_id.value(), encode(&quote)?.as_slice())?;
        }
        write.commit()?;
        Ok(quote)
    }

    fn link_reply(&self, child_id: Id, parent_id: Id) -> Result<()> {
        let write = self.begin_write()?;
        let Some(mut child) = status_txn(&write, child_id)? else {
            return Err(Error::NotFound(format!("status {child_id}")));
        };
        // A reply is linked once and never changed into a root.
        if child.in_reply_to_id.is_some() {
            drop(write);
            return Ok(());
        }
        let mut parent = status_txn(&write, parent_id)?
            .filter(|p| !p.is_deleted())
            .ok_or_else(|| Error::NotFound(format!("status {parent_id}")))?;
        {
            let mut index = write.open_table(STATUS_PARENT_INDEX)?;
            index.insert(
                schema::pair_key(parent_id.value(), child_id.value()).as_slice(),
                child_id.value(),
            )?;
        }
        parent.replies_count = parent.replies_count.saturating_add(1);
        super::statuses::write_status_txn(&write, &parent)?;
        child.in_reply_to_id = Some(parent_id);
        child.in_reply_to_account_id = Some(parent.account_id);
        super::statuses::write_status_txn(&write, &child)?;
        {
            let mut table = write.open_table(UNRESOLVED_PARENT)?;
            table.remove(child_id.value())?;
        }
        write.commit()?;
        Ok(())
    }

    fn set_unresolved_parent(&self, status_id: Id, parent_uri: Option<&str>) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(UNRESOLVED_PARENT)?;
            match parent_uri {
                Some(uri) => {
                    table.insert(status_id.value(), uri)?;
                }
                None => {
                    table.remove(status_id.value())?;
                }
            }
        }
        write.commit()?;
        Ok(())
    }

    fn unresolved_parent(&self, status_id: Id) -> Result<Option<String>> {
        let read = self.begin_read()?;
        let table = read.open_table(UNRESOLVED_PARENT)?;
        Ok(table.get(status_id.value())?.map(|g| g.value().to_string()))
    }

    fn account_status_ids(&self, account_id: Id) -> Result<Vec<Id>> {
        let read = self.begin_read()?;
        let index = read.open_table(schema::STATUS_AUTHOR_INDEX)?;
        let prefix = account_id.value().to_be_bytes();
        let mut ids = Vec::new();
        for entry in index.range(prefix.as_slice()..)? {
            let (key, value) = entry?;
            if !key.value().starts_with(&prefix) {
                break;
            }
            let id = Id::new(value.value());
            if let Some(status) = status_by_id_read(&read, id)? {
                if !status.is_deleted() {
                    ids.push(id);
                }
            }
            if ids.len() >= 1000 {
                break;
            }
        }
        Ok(ids)
    }

    fn remote_refresh(&self, status_id: Id) -> Result<Option<RefreshRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(REMOTE_REFRESH)?;
        match table.get(status_id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn set_remote_refresh(&self, status_id: Id, record: RefreshRecord) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(REMOTE_REFRESH)?;
            table.insert(status_id.value(), encode(&record)?.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn insert_report(&self, input: NewReport, now_ms: u64) -> Result<Report> {
        let write = self.begin_write()?;
        let report = Report {
            id: self.next_id(),
            reporter_id: input.reporter_id,
            target_account_id: input.target_account_id,
            status_ids: input.status_ids,
            comment: input.comment,
            forwarded: input.forwarded,
            activity_uri: input.activity_uri,
            created_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(REPORTS)?;
            table.insert(report.id.value(), encode(&report)?.as_slice())?;
        }
        if report.forwarded && report.activity_uri.is_none() {
            // A local report forwarded to the target's server: a Flag goes out.
            federation_event_txn(
                &write,
                self.next_id(),
                report.reporter_id,
                serde_json::json!({"event": "flag", "report_id": report.id.value()}),
                now_ms,
            )?;
        }
        write.commit()?;
        Ok(report)
    }

    fn enqueue_event(&self, actor_id: Id, event: serde_json::Value, now_ms: u64) -> Result<()> {
        let write = self.begin_write()?;
        federation_event_txn(&write, self.next_id(), actor_id, event, now_ms)?;
        write.commit()?;
        Ok(())
    }

    fn set_remote_media_source(&self, media_id: Id, url: &str) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(REMOTE_MEDIA_SOURCE)?;
            table.insert(media_id.value(), url)?;
        }
        write.commit()?;
        Ok(())
    }

    fn remote_media_source(&self, media_id: Id) -> Result<Option<String>> {
        let read = self.begin_read()?;
        let table = read.open_table(REMOTE_MEDIA_SOURCE)?;
        Ok(table.get(media_id.value())?.map(|g| g.value().to_string()))
    }

    fn stage_remote_media(
        &self,
        media_id: Id,
        file_key: &str,
        mime_type: &str,
        file_size: u64,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>> {
        let write = self.begin_write()?;
        let Some(mut media) = super::media::read_media_txn(&write, media_id)? else {
            return Ok(None);
        };
        if media.state != crate::domain::MediaState::Pending || !media.file_key.is_empty() {
            // Already staged: the job is a replay.
            drop(write);
            return Ok(Some(media));
        }
        media.file_key = file_key.to_string();
        media.mime_type = mime_type.to_string();
        media.file_size = file_size;
        media.updated_at_ms = now_ms;
        super::media::write_media_txn(&write, &media)?;
        let job = Job::new(
            media_id,
            JobKind::ProcessMedia,
            serde_json::json!({ "media_id": media_id.value(), "delay": 0 }),
            now_ms,
            now_ms,
        );
        super::jobs::insert_job(&write, &job)?;
        write.commit()?;
        Ok(Some(media))
    }

    fn mark_remote_media_failed(&self, media_id: Id, reason: &str, now_ms: u64) -> Result<()> {
        let write = self.begin_write()?;
        if let Some(mut media) = super::media::read_media_txn(&write, media_id)? {
            media.state = crate::domain::MediaState::Failed;
            media.error = Some(reason.chars().take(500).collect());
            media.updated_at_ms = now_ms;
            super::media::write_media_txn(&write, &media)?;
        }
        write.commit()?;
        Ok(())
    }
}

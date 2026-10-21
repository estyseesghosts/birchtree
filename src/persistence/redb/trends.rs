//! Trend candidate and approval-flag persistence (Phase 7.3).
//!
//! One transaction commits each review (flag row + audit row); candidate
//! materialization (`ensure_*`) is idempotent, so concurrent derivations
//! never duplicate rows. Reads are bounded: tag usage scans the hashtag
//! index once, link/publisher rows are newest-first in-memory pages over a
//! table that only grows with distinct URLs seen in recent statuses.

use redb::ReadableTable;

use crate::application::ports::{AnnualReportStore, TrendStore};
use crate::domain::{
    AdminAuditRecord, AnnualReport, Error, Id, Job, JobKind, NewAnnualReport, Result, Status,
    TagUsage, TrendLink, TrendPublisher, TrendStatusFlag, TrendTagFlag,
};

use super::schema::{
    self, ADMIN_AUDIT, ADMIN_AUDIT_BY_TARGET, ANNUAL_REPORTS, ANNUAL_REPORTS_BY_ACCOUNT, JOBS,
    TREND_LINKS, TREND_LINKS_BY_URL, TREND_PUBLISHERS, TREND_PUBLISHERS_BY_DOMAIN,
    TREND_STATUS_FLAGS, TREND_TAG_FLAGS,
};
use super::{decode, encode, RedbStore};

/// Distinct tags scanned for the tag-usage derivation (bounds the index walk).
const TAG_USAGE_SCAN_CAP: usize = 50_000;
/// Distinct tags returned (bounds the admin/paging join).
const TAG_USAGE_CAP: usize = 5_000;
/// Link/publisher rows returned per read (newest first).
const LINK_LIST_CAP: usize = 2_000;
/// Annual-report statuses scanned per generation (oldest-first walk with an
/// early stop at the year boundary; ids embed the timestamp).
const YEAR_SCAN_CAP: usize = 20_000;
/// Jobs scanned for the `generating` state check.
const JOB_SCAN_CAP: usize = 5_000;

/// Lowercase a host for publisher rows.
fn normalize_host(host: &str) -> String {
    host.to_ascii_lowercase()
}

impl TrendStore for RedbStore {
    fn trend_tag_usages(&self, cap: usize) -> Result<Vec<TagUsage>> {
        let cap = cap.min(TAG_USAGE_CAP);
        let read = self.begin_read()?;
        let table = read.open_table(schema::HASHTAG_INDEX)?;
        // Keys are `tag || 0xFF || status_id_be`: one entry per (tag, status).
        // Count distinct tags; statuses per tag are the usage count.
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut scanned = 0usize;
        for entry in table.iter()? {
            let (key, _value) = entry?;
            let bytes = key.value();
            let Some(sep) = bytes.iter().position(|b| *b == 0xFF) else {
                continue;
            };
            let tag = String::from_utf8_lossy(&bytes[..sep]).into_owned();
            *counts.entry(tag).or_insert(0) += 1;
            scanned += 1;
            if scanned >= TAG_USAGE_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(scanned);
        let mut out: Vec<TagUsage> = counts
            .into_iter()
            .map(|(name, uses)| TagUsage { name, uses })
            .collect();
        out.sort_by(|a, b| b.uses.cmp(&a.uses).then(a.name.cmp(&b.name)));
        out.truncate(cap);
        Ok(out)
    }

    fn trend_tag_flag(&self, name: &str) -> Result<Option<TrendTagFlag>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_TAG_FLAGS)?;
        match table.get(name)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn all_trend_tag_flags(&self) -> Result<Vec<TrendTagFlag>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_TAG_FLAGS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_key, value) = entry?;
            out.push(decode(value.value())?);
        }
        self.note_scanned(out.len());
        Ok(out)
    }

    fn ensure_trend_tag(&self, name: &str, now_ms: u64) -> Result<TrendTagFlag> {
        let write = self.begin_write()?;
        let mut table = write.open_table(TREND_TAG_FLAGS)?;
        let existing: Option<TrendTagFlag> = match table.get(name)? {
            Some(guard) => Some(decode(guard.value())?),
            None => None,
        };
        let row = match existing {
            Some(row) => row,
            None => {
                let row = TrendTagFlag {
                    id: self.next_id(),
                    name: name.to_string(),
                    allowed: None,
                    reviewed_at_ms: None,
                    created_at_ms: now_ms,
                };
                table.insert(name, encode(&row)?.as_slice())?;
                row
            }
        };
        drop(table);
        write.commit()?;
        Ok(row)
    }

    fn review_trend_tag(
        &self,
        id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendTagFlag> {
        let write = self.begin_write()?;
        // Find the row by id (table is name-keyed; tag rows are few).
        let (name, mut row): (String, TrendTagFlag) = {
            let table = write.open_table(TREND_TAG_FLAGS)?;
            let mut found = None;
            for entry in table.iter()? {
                let (key, value) = entry?;
                let row: TrendTagFlag = decode(value.value())?;
                if row.id == id {
                    found = Some((key.value().to_string(), row));
                    break;
                }
            }
            match found {
                Some(found) => found,
                None => return Err(Error::NotFound(format!("trend tag {id}"))),
            }
        };
        row.allowed = Some(allowed);
        row.reviewed_at_ms = Some(now_ms);
        {
            let mut table = write.open_table(TREND_TAG_FLAGS)?;
            table.insert(name.as_str(), encode(&row)?.as_slice())?;
        }
        {
            let mut audit = write.open_table(ADMIN_AUDIT)?;
            let mut by_target = write.open_table(ADMIN_AUDIT_BY_TARGET)?;
            let record = AdminAuditRecord {
                id: self.next_id(),
                actor_account_id,
                action: if allowed { "approve".into() } else { "reject".into() },
                target_type: "trend_tag".into(),
                target_id: id.to_string(),
                created_at_ms: now_ms,
            };
            audit.insert(record.id.value(), encode(&record)?.as_slice())?;
            by_target.insert(
                schema::audit_target_key("trend_tag", &id.to_string(), record.id.value())
                    .as_slice(),
                record.id.value(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn trend_status_flag(&self, status_id: Id) -> Result<Option<TrendStatusFlag>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_STATUS_FLAGS)?;
        match table.get(status_id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn all_trend_status_flags(&self) -> Result<Vec<TrendStatusFlag>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_STATUS_FLAGS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_key, value) = entry?;
            out.push(decode(value.value())?);
        }
        self.note_scanned(out.len());
        Ok(out)
    }

    fn review_trend_status(
        &self,
        status_id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendStatusFlag> {
        let write = self.begin_write()?;
        // The status must exist and be live (approving a deleted status 404s).
        {
            let statuses = write.open_table(schema::STATUSES)?;
            match statuses.get(status_id.value())? {
                Some(guard) => {
                    let status: Status = decode(guard.value())?;
                    if status.is_deleted() {
                        return Err(Error::NotFound(format!("trend status {status_id}")));
                    }
                }
                None => return Err(Error::NotFound(format!("trend status {status_id}"))),
            }
        }
        let row = TrendStatusFlag {
            status_id,
            allowed,
            reviewed_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(TREND_STATUS_FLAGS)?;
            table.insert(status_id.value(), encode(&row)?.as_slice())?;
        }
        {
            let mut audit = write.open_table(ADMIN_AUDIT)?;
            let mut by_target = write.open_table(ADMIN_AUDIT_BY_TARGET)?;
            let record = AdminAuditRecord {
                id: self.next_id(),
                actor_account_id,
                action: if allowed { "approve".into() } else { "reject".into() },
                target_type: "trend_status".into(),
                target_id: status_id.to_string(),
                created_at_ms: now_ms,
            };
            audit.insert(record.id.value(), encode(&record)?.as_slice())?;
            by_target.insert(
                schema::audit_target_key(
                    "trend_status",
                    &status_id.to_string(),
                    record.id.value(),
                )
                .as_slice(),
                record.id.value(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn ensure_trend_links(&self, urls: &[String], now_ms: u64) -> Result<Vec<TrendLink>> {
        if urls.is_empty() {
            return Ok(Vec::new());
        }
        let mut seen = std::collections::HashSet::new();
        let write = self.begin_write()?;
        let mut out = Vec::new();
        {
            let mut links = write.open_table(TREND_LINKS)?;
            let mut by_url = write.open_table(TREND_LINKS_BY_URL)?;
            let mut publishers = write.open_table(TREND_PUBLISHERS)?;
            let mut publishers_by_domain = write.open_table(TREND_PUBLISHERS_BY_DOMAIN)?;
            for url in urls {
                if !seen.insert(url.clone()) {
                    continue;
                }
                if let Some(id) = by_url.get(url.as_str())? {
                    let id = Id::new(id.value());
                    if let Some(guard) = links.get(id.value())? {
                        out.push(decode(guard.value())?);
                    }
                    continue;
                }
                let row = TrendLink {
                    id: self.next_id(),
                    url: url.clone(),
                    title: None,
                    description: None,
                    provider_name: None,
                    provider_url: None,
                    image_url: None,
                    allowed: None,
                    reviewed_at_ms: None,
                    created_at_ms: now_ms,
                };
                links.insert(row.id.value(), encode(&row)?.as_slice())?;
                by_url.insert(url.as_str(), row.id.value())?;
                // A new row gets one bounded preview-card fetch (D40): the
                // crawler fills the card fields after commit; a retry never
                // duplicates the row, and the job is a no-op when metadata
                // already arrived.
                super::jobs::insert_job(
                    &write,
                    &Job::new(
                        self.next_id(),
                        JobKind::PreviewCardFetch,
                        serde_json::json!({"link_id": row.id.value(), "url": row.url}),
                        now_ms,
                        now_ms,
                    ),
                )?;
                // Every link URL's host gets a publisher row (the admin
                // publishers index lists every seen host).
                if let Some(host) = link_host(url)
                    && publishers_by_domain.get(host.as_str())?.is_none()
                {
                    let publisher = TrendPublisher {
                        id: self.next_id(),
                        domain: host.clone(),
                        allowed: None,
                        reviewed_at_ms: None,
                        created_at_ms: now_ms,
                    };
                    publishers.insert(publisher.id.value(), encode(&publisher)?.as_slice())?;
                    publishers_by_domain.insert(host.as_str(), publisher.id.value())?;
                }
                out.push(row);
            }
        }
        write.commit()?;
        Ok(out)
    }

    fn trend_link_by_id(&self, id: Id) -> Result<Option<TrendLink>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_LINKS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn all_trend_links(&self) -> Result<Vec<TrendLink>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_LINKS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_key, value) = entry?;
            out.push(decode(value.value())?);
            if out.len() >= LINK_LIST_CAP {
                break;
            }
        }
        self.note_scanned(out.len());
        // Newest first (row ids are time-ordered).
        out.sort_by(|a: &TrendLink, b: &TrendLink| b.id.cmp(&a.id));
        Ok(out)
    }

    fn review_trend_link(
        &self,
        id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendLink> {
        let write = self.begin_write()?;
        let mut row: TrendLink = {
            let table = write.open_table(TREND_LINKS)?;
            match table.get(id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("trend link {id}"))),
            }
        };
        row.allowed = Some(allowed);
        row.reviewed_at_ms = Some(now_ms);
        {
            let mut table = write.open_table(TREND_LINKS)?;
            table.insert(id.value(), encode(&row)?.as_slice())?;
        }
        {
            let mut audit = write.open_table(ADMIN_AUDIT)?;
            let mut by_target = write.open_table(ADMIN_AUDIT_BY_TARGET)?;
            let record = AdminAuditRecord {
                id: self.next_id(),
                actor_account_id,
                action: if allowed { "approve".into() } else { "reject".into() },
                target_type: "trend_link".into(),
                target_id: id.to_string(),
                created_at_ms: now_ms,
            };
            audit.insert(record.id.value(), encode(&record)?.as_slice())?;
            by_target.insert(
                schema::audit_target_key("trend_link", &id.to_string(), record.id.value())
                    .as_slice(),
                record.id.value(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn all_trend_publishers(&self) -> Result<Vec<TrendPublisher>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_PUBLISHERS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_key, value) = entry?;
            out.push(decode(value.value())?);
            if out.len() >= LINK_LIST_CAP {
                break;
            }
        }
        self.note_scanned(out.len());
        out.sort_by(|a: &TrendPublisher, b: &TrendPublisher| b.id.cmp(&a.id));
        Ok(out)
    }

    fn trend_publisher_by_id(&self, id: Id) -> Result<Option<TrendPublisher>> {
        let read = self.begin_read()?;
        let table = read.open_table(TREND_PUBLISHERS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn trend_audit_trail(
        &self,
        target_type: &str,
        target_id: &str,
    ) -> Result<Vec<AdminAuditRecord>> {
        let read = self.begin_read()?;
        let by_target = read.open_table(ADMIN_AUDIT_BY_TARGET)?;
        let prefix = schema::audit_target_prefix(target_type, target_id);
        let mut ids = Vec::new();
        for entry in by_target.range(prefix.as_slice()..)? {
            let (key, value) = entry?;
            if !key.value().starts_with(prefix.as_slice()) {
                break;
            }
            ids.push(Id::new(value.value()));
            if ids.len() >= 100 {
                break;
            }
        }
        let table = read.open_table(ADMIN_AUDIT)?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(guard) = table.get(id.value())? {
                out.push(decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn review_trend_publisher(
        &self,
        id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendPublisher> {
        let write = self.begin_write()?;
        let mut row: TrendPublisher = {
            let table = write.open_table(TREND_PUBLISHERS)?;
            match table.get(id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("trend publisher {id}"))),
            }
        };
        row.allowed = Some(allowed);
        row.reviewed_at_ms = Some(now_ms);
        {
            let mut table = write.open_table(TREND_PUBLISHERS)?;
            table.insert(id.value(), encode(&row)?.as_slice())?;
        }
        {
            let mut audit = write.open_table(ADMIN_AUDIT)?;
            let mut by_target = write.open_table(ADMIN_AUDIT_BY_TARGET)?;
            let record = AdminAuditRecord {
                id: self.next_id(),
                actor_account_id,
                action: if allowed { "approve".into() } else { "reject".into() },
                target_type: "trend_publisher".into(),
                target_id: id.to_string(),
                created_at_ms: now_ms,
            };
            audit.insert(record.id.value(), encode(&record)?.as_slice())?;
            by_target.insert(
                schema::audit_target_key(
                    "trend_publisher",
                    &id.to_string(),
                    record.id.value(),
                )
                .as_slice(),
                record.id.value(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn tag_newest_status_id(&self, tag: &str) -> Result<Option<Id>> {
        if tag.is_empty() {
            return Ok(None);
        }
        let read = self.begin_read()?;
        let index = read.open_table(schema::HASHTAG_INDEX)?;
        // Keys are `tag || 0xFF || status_id_be` ascending: the last key of
        // the prefix range is the newest indexed status. The walk is bounded
        // like the tag-usage derivation.
        let mut prefix = Vec::with_capacity(tag.len() + 9);
        prefix.extend_from_slice(tag.as_bytes());
        prefix.push(0xFF);
        let mut end = prefix.clone();
        end.extend_from_slice(&u64::MAX.to_be_bytes());
        let mut newest: Option<Id> = None;
        let mut scanned = 0usize;
        for entry in index.range(prefix.as_slice()..=end.as_slice())? {
            let (key, _) = entry?;
            let bytes = key.value();
            let id = Id::new(u64::from_be_bytes(bytes[tag.len() + 1..].try_into().unwrap()));
            newest = Some(id);
            scanned += 1;
            if scanned >= TAG_USAGE_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(newest)
    }

    fn update_trend_link_card(
        &self,
        id: Id,
        title: Option<String>,
        description: Option<String>,
        provider_name: Option<String>,
        provider_url: Option<String>,
        image_url: Option<String>,
        now_ms: u64,
    ) -> Result<Option<TrendLink>> {
        let _ = now_ms;
        let write = self.begin_write()?;
        let mut row: TrendLink = {
            let table = write.open_table(TREND_LINKS)?;
            match table.get(id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Ok(None),
            }
        };
        // `None` arguments leave the stored field unchanged, so a partial
        // fetch (a page with a title but no description) never blanks a
        // field an earlier fetch filled.
        if let Some(title) = title {
            row.title = Some(title);
        }
        if let Some(description) = description {
            row.description = Some(description);
        }
        if let Some(provider_name) = provider_name {
            row.provider_name = Some(provider_name);
        }
        if let Some(provider_url) = provider_url {
            row.provider_url = Some(provider_url);
        }
        if let Some(image_url) = image_url {
            row.image_url = Some(image_url);
        }
        {
            let mut table = write.open_table(TREND_LINKS)?;
            table.insert(id.value(), encode(&row)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(row))
    }
}

/// Host of an `http(s)` URL, lowercased; `None` when the URL is not usable
/// for a publisher row.
fn link_host(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next_back()
        .unwrap_or("");
    let host = host.split(':').next().unwrap_or("").trim();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    Some(normalize_host(host))
}

impl AnnualReportStore for RedbStore {
    fn annual_report(&self, account_id: Id, year: i32) -> Result<Option<AnnualReport>> {
        let read = self.begin_read()?;
        let index = read.open_table(ANNUAL_REPORTS_BY_ACCOUNT)?;
        let key = schema::account_year_key(account_id.value(), year);
        let Some(id) = index.get(key.as_slice())? else {
            return Ok(None);
        };
        let table = read.open_table(ANNUAL_REPORTS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn pending_annual_reports(&self, account_id: Id) -> Result<Vec<AnnualReport>> {
        let read = self.begin_read()?;
        let index = read.open_table(ANNUAL_REPORTS_BY_ACCOUNT)?;
        let lo = schema::account_year_key(account_id.value(), i32::MIN);
        let hi = schema::account_year_key(account_id.value(), i32::MAX);
        let mut ids = Vec::new();
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            ids.push(Id::new(value.value()));
        }
        let table = read.open_table(ANNUAL_REPORTS)?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(guard) = table.get(id.value())? {
                let report: AnnualReport = decode(guard.value())?;
                if report.viewed_at_ms.is_none() {
                    out.push(report);
                }
            }
        }
        // Newest year first.
        out.sort_by(|a, b| b.year.cmp(&a.year));
        Ok(out)
    }

    fn insert_annual_report(&self, input: NewAnnualReport, now_ms: u64) -> Result<AnnualReport> {
        let write = self.begin_write()?;
        let key = schema::account_year_key(input.account_id.value(), input.year);
        let existing: Option<AnnualReport> = {
            let index = write.open_table(ANNUAL_REPORTS_BY_ACCOUNT)?;
            match index.get(key.as_slice())? {
                Some(id) => {
                    let table = write.open_table(ANNUAL_REPORTS)?;
                    match table.get(id.value())? {
                        Some(guard) => Some(decode(guard.value())?),
                        None => None,
                    }
                }
                None => None,
            }
        };
        if let Some(report) = existing {
            return Ok(report);
        }
        let report = AnnualReport {
            id: self.next_id(),
            account_id: input.account_id,
            year: input.year,
            data: input.data,
            schema_version: input.schema_version,
            share_key: None,
            viewed_at_ms: None,
            created_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(ANNUAL_REPORTS)?;
            table.insert(report.id.value(), encode(&report)?.as_slice())?;
        }
        {
            let mut index = write.open_table(ANNUAL_REPORTS_BY_ACCOUNT)?;
            index.insert(key.as_slice(), report.id.value())?;
        }
        // D51 `annual_report` to the report owner, same transaction (G-B
        // producer). The early `Ok(report)` return above stays silent: a
        // duplicate insert is not a new report.
        for job in crate::application::notifications::emit_annual_report(input.account_id, now_ms)
        {
            super::jobs::insert_job(
                &write,
                &Job::new(self.next_id(), job.kind, job.payload, now_ms, now_ms),
            )?;
        }
        write.commit()?;
        Ok(report)
    }

    fn mark_annual_report_viewed(
        &self,
        account_id: Id,
        year: i32,
        now_ms: u64,
    ) -> Result<Option<AnnualReport>> {
        let write = self.begin_write()?;
        let key = schema::account_year_key(account_id.value(), year);
        let id: Option<Id> = {
            let index = write.open_table(ANNUAL_REPORTS_BY_ACCOUNT)?;
            index.get(key.as_slice())?.map(|guard| Id::new(guard.value()))
        };
        let Some(id) = id else {
            return Ok(None);
        };
        let mut report: AnnualReport = {
            let table = write.open_table(ANNUAL_REPORTS)?;
            match table.get(id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Ok(None),
            }
        };
        report.viewed_at_ms = Some(now_ms);
        {
            let mut table = write.open_table(ANNUAL_REPORTS)?;
            table.insert(id.value(), encode(&report)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(report))
    }

    fn has_pending_generate_report(&self, account_id: Id, year: i32) -> Result<bool> {
        let read = self.begin_read()?;
        let table = read.open_table(JOBS)?;
        let mut scanned = 0usize;
        for entry in table.iter()? {
            let (_key, value) = entry?;
            let job: crate::domain::Job = decode(value.value())?;
            scanned += 1;
            if job.kind != JobKind::GenerateAnnualReport {
                continue;
            }
            if job.state.is_terminal() {
                continue;
            }
            let payload_account = value_account(&job.payload);
            let payload_year = value_year(&job.payload);
            if payload_account == Some(account_id.value()) && payload_year == Some(year) {
                return Ok(true);
            }
            if scanned >= JOB_SCAN_CAP {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(false)
    }

    fn account_statuses_in_year(
        &self,
        account_id: Id,
        year: i32,
        cap: usize,
    ) -> Result<Vec<Status>> {
        let cap = cap.min(YEAR_SCAN_CAP);
        // Year boundaries as the smallest/largest timestamp ids of the year.
        let (start_ms, end_ms) = year_bounds_ms(year);
        let floor = crate::domain::Id::from_parts(start_ms, 0);
        let read = self.begin_read()?;
        let index = read.open_table(schema::STATUS_AUTHOR_INDEX)?;
        let statuses = read.open_table(schema::STATUSES)?;
        let lo = schema::pair_key(account_id.value(), 0);
        let hi = schema::pair_key(account_id.value(), u64::MAX);
        let mut out = Vec::new();
        let mut scanned = 0usize;
        // The author index is `(account || desc(status_id))`: forward order is
        // newest first. Stop once ids predate the year.
        for entry in index.range(lo.as_slice()..=hi.as_slice())? {
            let (_key, value) = entry?;
            let id = Id::new(value.value());
            if id < floor {
                break;
            }
            scanned += 1;
            if id.timestamp_ms() > end_ms {
                continue;
            }
            if let Some(guard) = statuses.get(id.value())? {
                let status: Status = decode(guard.value())?;
                if !status.is_deleted() {
                    out.push(status);
                }
            }
            if out.len() >= cap {
                break;
            }
        }
        self.note_scanned(scanned);
        // Oldest first (generation input order).
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

fn value_account(payload: &serde_json::Value) -> Option<u64> {
    payload.get("account_id")?.as_u64()
}

fn value_year(payload: &serde_json::Value) -> Option<i32> {
    payload.get("year")?.as_i64()?.try_into().ok()
}

/// Millisecond `[start, end]` of a UTC calendar year.
fn year_bounds_ms(year: i32) -> (u64, u64) {
    let start = chrono::NaiveDate::from_ymd_opt(year, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp_millis().max(0) as u64)
        .unwrap_or(0);
    let end = chrono::NaiveDate::from_ymd_opt(year, 12, 31)
        .and_then(|d| d.and_hms_opt(23, 59, 59))
        .map(|dt| dt.and_utc().timestamp_millis().max(0) as u64)
        .unwrap_or(u64::MAX);
    // Millisecond precision: the year's last millisecond.
    (start, end.saturating_add(999))
}

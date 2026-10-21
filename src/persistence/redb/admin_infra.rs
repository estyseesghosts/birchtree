//! Admin infrastructure rows (Phase 7.3, subagent C).
//!
//! Global ordered tables keyed by `desc_id(row_id)` (forward scan = newest
//! first) plus per-value unique indexes. One `redb` transaction per method.

use std::collections::BTreeSet;

use redb::ReadableTable;

use crate::application::ports::{
    AdminDomainBlockPatch, AdminInfraStore, AdminIpBlockPatch, AdminTagPatch, StatusMetricRow,
};
use crate::application::admin_infra as use_cases;
use crate::domain::{
    Account, AdminAccountOverview, AdminAuditRecord, AdminCanonicalEmailBlock, AdminDomainAllow,
    AdminDomainBlock, AdminDomainSeverity, AdminEmailDomainBlock, AdminIpBlock, AdminIpSeverity,
    AdminMediaOwner, AdminReportTarget, AdminTag, Error, Follow, FollowMetricRow, Id,
    MediaAttachment, Report, Result, Rule, RuleTranslation, SignInEvent, Status,
    RULE_LOCALE_LIMIT, RULE_TEXT_LIMIT,
};

use super::schema::{
    self, ACCOUNTS, ADMIN_AUDIT, ADMIN_CANONICAL_EMAIL_BLOCKS,
    ADMIN_CANONICAL_EMAIL_BLOCK_BY_HASH, ADMIN_DOMAIN_ALLOWS, ADMIN_DOMAIN_ALLOW_BY_DOMAIN,
    ADMIN_DOMAIN_BLOCKS, ADMIN_DOMAIN_BLOCK_BY_DOMAIN, ADMIN_EMAIL_DOMAIN_BLOCKS,
    ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN, ADMIN_IP_BLOCKS, ADMIN_IP_BLOCK_BY_IP, ADMIN_TAGS,
    ADMIN_TAG_BY_NAME, FOLLOWS, HASHTAG_INDEX, MEDIA, REPORTS, RULES, RULE_TRANSLATIONS, SIGN_INS,
    STATUSES,
};
use super::{decode, encode, RedbStore};

/// Scan cap for admin list pages (upstream allows up to 500 per page).
const PAGE_SCAN_CAP: usize = 5_000;

/// Scan cap for the metric full-table scans.
const METRIC_SCAN_CAP: usize = 200_000;

/// Scan cap for distinct hashtag collection (tag registry materialization).
const TAG_SCAN_CAP: usize = 20_000;

/// Bounded newest-first page over a `desc_id`-keyed global table.
fn page_desc<T>(
    store: &RedbStore,
    table: redb::TableDefinition<u64, &[u8]>,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
    limit: usize,
) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    use super::StoreError;
    if limit == 0 {
        return Ok(Vec::new());
    }
    if max_id.is_some_and(|m| m.value() == 0) {
        return Ok(Vec::new());
    }
    let read = store.begin_read()?;
    let index: redb::ReadOnlyTable<u64, &[u8]> = read.open_table(table).map_err(StoreError::from)?;
    // Keys hold `desc(id)`: ascending key order is newest first.
    // `max_id` is exclusive (older than it); `since_id`/`min_id` are exclusive
    // (newer than them) — the same mapping as `notifications_page`.
    let lower = max_id
        .map(|m| schema::desc_id(m.value()).saturating_add(1))
        .unwrap_or(0);
    let upper_exclusive = since_id.or(min_id).map(|x| schema::desc_id(x.value()));
    let mut out: Vec<T> = Vec::new();
    let mut scanned = 0usize;
    // Two identical loops (rather than one boxed iterator) so the range type
    // never needs an annotation.
    if let Some(upper) = upper_exclusive {
        for entry in index.range(lower..upper)? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > PAGE_SCAN_CAP {
                break;
            }
            out.push(decode(value.value())?);
            if out.len() >= limit {
                break;
            }
        }
    } else {
        for entry in index.range(lower..)? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > PAGE_SCAN_CAP {
                break;
            }
            out.push(decode(value.value())?);
            if out.len() >= limit {
                break;
            }
        }
    }
    store.note_scanned(scanned);
    Ok(out)
}

fn get_by_desc<T>(store: &RedbStore, table: redb::TableDefinition<u64, &[u8]>, id: Id) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    use super::StoreError;
    let read = store.begin_read()?;
    let rows: redb::ReadOnlyTable<u64, &[u8]> = read.open_table(table).map_err(StoreError::from)?;
    match rows.get(schema::desc_id(id.value()))? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

impl AdminInfraStore for RedbStore {
    // -- Domain allows --

    fn admin_create_domain_allow(&self, domain: &str, now_ms: u64) -> Result<AdminDomainAllow> {
        let domain = use_cases::normalize_domain(domain);
        use_cases::validate_domain(&domain)?;
        let write = self.begin_write()?;
        let existing_id: Option<u64> = {
            let by_domain = write.open_table(ADMIN_DOMAIN_ALLOW_BY_DOMAIN)?;
            by_domain.get(domain.as_str())?.map(|guard| guard.value())
        };
        if let Some(existing_id) = existing_id {
            let id = Id::new(existing_id);
            let row: Option<AdminDomainAllow> = {
                let rows = write.open_table(ADMIN_DOMAIN_ALLOWS)?;
                match rows.get(schema::desc_id(id.value()))? {
                    Some(guard) => Some(decode(guard.value())?),
                    None => None,
                }
            };
            if let Some(row) = row {
                return Ok(row);
            }
        }
        let row = AdminDomainAllow { id: self.next_id(), domain: domain.clone(), created_at_ms: now_ms };
        {
            let mut rows = write.open_table(ADMIN_DOMAIN_ALLOWS)?;
            rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
        }
        {
            let mut by_domain = write.open_table(ADMIN_DOMAIN_ALLOW_BY_DOMAIN)?;
            by_domain.insert(domain.as_str(), row.id.value())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn admin_domain_allow_by_id(&self, id: Id) -> Result<Option<AdminDomainAllow>> {
        get_by_desc(self, ADMIN_DOMAIN_ALLOWS, id)
    }

    fn admin_domain_allows_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminDomainAllow>> {
        page_desc(self, ADMIN_DOMAIN_ALLOWS, max_id, since_id, min_id, limit)
    }

    fn admin_delete_domain_allow(&self, id: Id) -> Result<Option<AdminDomainAllow>> {
        let existing: Option<AdminDomainAllow> = get_by_desc(self, ADMIN_DOMAIN_ALLOWS, id)?;
        let Some(row) = existing else { return Ok(None) };
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_DOMAIN_ALLOWS)?;
            rows.remove(schema::desc_id(id.value()))?;
        }
        {
            let mut by_domain = write.open_table(ADMIN_DOMAIN_ALLOW_BY_DOMAIN)?;
            by_domain.remove(row.domain.as_str())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    // -- Domain blocks --

    fn admin_create_domain_block(
        &self,
        domain: &str,
        patch: AdminDomainBlockPatch,
        now_ms: u64,
    ) -> Result<AdminDomainBlock> {
        let domain = use_cases::normalize_domain(domain);
        use_cases::validate_domain(&domain)?;
        let write = self.begin_write()?;
        let duplicate: bool = {
            let by_domain = write.open_table(ADMIN_DOMAIN_BLOCK_BY_DOMAIN)?;
            by_domain.get(domain.as_str())?.is_some()
        };
        if duplicate {
            return Err(Error::InvalidInput(
                "Validation failed: Domain has already been taken".into(),
            ));
        }
        let row = AdminDomainBlock {
            id: self.next_id(),
            domain: domain.clone(),
            severity: patch.severity.unwrap_or(AdminDomainSeverity::Silence),
            reject_media: patch.reject_media.unwrap_or(false),
            reject_reports: patch.reject_reports.unwrap_or(false),
            private_comment: patch.private_comment.unwrap_or(None),
            public_comment: patch.public_comment.unwrap_or(None),
            obfuscate: patch.obfuscate.unwrap_or(false),
            created_at_ms: now_ms,
        };
        {
            let mut rows = write.open_table(ADMIN_DOMAIN_BLOCKS)?;
            rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
        }
        {
            let mut by_domain = write.open_table(ADMIN_DOMAIN_BLOCK_BY_DOMAIN)?;
            by_domain.insert(domain.as_str(), row.id.value())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn admin_domain_block_by_id(&self, id: Id) -> Result<Option<AdminDomainBlock>> {
        get_by_desc(self, ADMIN_DOMAIN_BLOCKS, id)
    }

    fn admin_domain_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminDomainBlock>> {
        page_desc(self, ADMIN_DOMAIN_BLOCKS, max_id, since_id, min_id, limit)
    }

    fn admin_update_domain_block(
        &self,
        id: Id,
        patch: AdminDomainBlockPatch,
        _now_ms: u64,
    ) -> Result<Option<AdminDomainBlock>> {
        let existing: Option<AdminDomainBlock> = get_by_desc(self, ADMIN_DOMAIN_BLOCKS, id)?;
        let Some(mut row) = existing else { return Ok(None) };
        if let Some(severity) = patch.severity {
            row.severity = severity;
        }
        if let Some(reject_media) = patch.reject_media {
            row.reject_media = reject_media;
        }
        if let Some(reject_reports) = patch.reject_reports {
            row.reject_reports = reject_reports;
        }
        if let Some(private_comment) = patch.private_comment {
            row.private_comment = private_comment;
        }
        if let Some(public_comment) = patch.public_comment {
            row.public_comment = public_comment;
        }
        if let Some(obfuscate) = patch.obfuscate {
            row.obfuscate = obfuscate;
        }
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_DOMAIN_BLOCKS)?;
            rows.insert(schema::desc_id(id.value()), encode(&row)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    fn admin_delete_domain_block(&self, id: Id) -> Result<Option<AdminDomainBlock>> {
        let existing: Option<AdminDomainBlock> = get_by_desc(self, ADMIN_DOMAIN_BLOCKS, id)?;
        let Some(row) = existing else { return Ok(None) };
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_DOMAIN_BLOCKS)?;
            rows.remove(schema::desc_id(id.value()))?;
        }
        {
            let mut by_domain = write.open_table(ADMIN_DOMAIN_BLOCK_BY_DOMAIN)?;
            by_domain.remove(row.domain.as_str())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    // -- Email-domain blocks --

    fn admin_create_email_domain_block(
        &self,
        domain: &str,
        allow_with_approval: bool,
        now_ms: u64,
    ) -> Result<AdminEmailDomainBlock> {
        let domain = use_cases::normalize_domain(domain);
        use_cases::validate_domain(&domain)?;
        let write = self.begin_write()?;
        let duplicate: bool = {
            let by_domain = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
            by_domain.get(domain.as_str())?.is_some()
        };
        if duplicate {
            return Err(Error::InvalidInput(
                "Validation failed: Domain has already been taken".into(),
            ));
        }
        let row = AdminEmailDomainBlock {
            id: self.next_id(),
            domain: domain.clone(),
            allow_with_approval,
            created_at_ms: now_ms,
        };
        {
            let mut rows = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCKS)?;
            rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
        }
        {
            let mut by_domain = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
            by_domain.insert(domain.as_str(), row.id.value())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn admin_email_domain_block_by_id(&self, id: Id) -> Result<Option<AdminEmailDomainBlock>> {
        get_by_desc(self, ADMIN_EMAIL_DOMAIN_BLOCKS, id)
    }

    fn email_domain_block_for_domain(&self, domain: &str) -> Result<Option<AdminEmailDomainBlock>> {
        // Domains are unique at creation (`admin_create_email_domain_block`
        // refuses duplicates), so the by-domain index holds at most one row.
        let normalized = use_cases::normalize_domain(domain);
        if normalized.is_empty() {
            return Ok(None);
        }
        let found: Option<u64> = {
            let read = self.begin_read()?;
            let by_domain = read.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
            by_domain.get(normalized.as_str())?.map(|guard| guard.value())
        };
        let Some(id) = found else {
            return Ok(None);
        };
        get_by_desc(self, ADMIN_EMAIL_DOMAIN_BLOCKS, Id::new(id))
    }

    fn admin_email_domain_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminEmailDomainBlock>> {
        page_desc(self, ADMIN_EMAIL_DOMAIN_BLOCKS, max_id, since_id, min_id, limit)
    }

    fn admin_delete_email_domain_block(&self, id: Id) -> Result<Option<AdminEmailDomainBlock>> {
        let existing: Option<AdminEmailDomainBlock> =
            get_by_desc(self, ADMIN_EMAIL_DOMAIN_BLOCKS, id)?;
        let Some(row) = existing else { return Ok(None) };
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCKS)?;
            rows.remove(schema::desc_id(id.value()))?;
        }
        {
            let mut by_domain = write.open_table(ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
            by_domain.remove(row.domain.as_str())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    // -- IP blocks --

    fn admin_create_ip_block(
        &self,
        ip: &str,
        severity: AdminIpSeverity,
        comment: &str,
        expires_at_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<AdminIpBlock> {
        let cidr = use_cases::parse_cidr(ip)?;
        let text = use_cases::cidr_text(cidr);
        let write = self.begin_write()?;
        let duplicate: bool = {
            let by_ip = write.open_table(ADMIN_IP_BLOCK_BY_IP)?;
            by_ip.get(text.as_str())?.is_some()
        };
        if duplicate {
            return Err(Error::InvalidInput(
                "Validation failed: Ip has already been taken".into(),
            ));
        }
        let row = AdminIpBlock {
            id: self.next_id(),
            ip: text.clone(),
            severity,
            comment: comment.to_string(),
            expires_at_ms,
            created_at_ms: now_ms,
        };
        {
            let mut rows = write.open_table(ADMIN_IP_BLOCKS)?;
            rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
        }
        {
            let mut by_ip = write.open_table(ADMIN_IP_BLOCK_BY_IP)?;
            by_ip.insert(text.as_str(), row.id.value())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn admin_ip_block_by_id(&self, id: Id) -> Result<Option<AdminIpBlock>> {
        get_by_desc(self, ADMIN_IP_BLOCKS, id)
    }

    fn admin_ip_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminIpBlock>> {
        page_desc(self, ADMIN_IP_BLOCKS, max_id, since_id, min_id, limit)
    }

    fn admin_ip_blocks_all(&self, cap: usize) -> Result<Vec<AdminIpBlock>> {
        let read = self.begin_read()?;
        let rows = read.open_table(ADMIN_IP_BLOCKS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in rows.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > PAGE_SCAN_CAP {
                break;
            }
            out.push(decode::<AdminIpBlock>(value.value())?);
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn admin_update_ip_block(
        &self,
        id: Id,
        patch: AdminIpBlockPatch,
        _now_ms: u64,
    ) -> Result<Option<AdminIpBlock>> {
        let existing: Option<AdminIpBlock> = get_by_desc(self, ADMIN_IP_BLOCKS, id)?;
        let Some(mut row) = existing else { return Ok(None) };
        if let Some(severity) = patch.severity {
            row.severity = severity;
        }
        if let Some(comment) = patch.comment {
            row.comment = comment.unwrap_or_default();
        }
        if let Some(expires_at_ms) = patch.expires_at_ms {
            row.expires_at_ms = expires_at_ms;
        }
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_IP_BLOCKS)?;
            rows.insert(schema::desc_id(id.value()), encode(&row)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    fn admin_delete_ip_block(&self, id: Id) -> Result<Option<AdminIpBlock>> {
        let existing: Option<AdminIpBlock> = get_by_desc(self, ADMIN_IP_BLOCKS, id)?;
        let Some(row) = existing else { return Ok(None) };
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_IP_BLOCKS)?;
            rows.remove(schema::desc_id(id.value()))?;
        }
        {
            let mut by_ip = write.open_table(ADMIN_IP_BLOCK_BY_IP)?;
            by_ip.remove(row.ip.as_str())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    // -- Canonical email blocks --

    fn admin_create_canonical_email_block(
        &self,
        email_hash: &str,
        now_ms: u64,
    ) -> Result<AdminCanonicalEmailBlock> {
        if email_hash.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Validation failed: Canonical email hash can't be blank".into(),
            ));
        }
        let row = AdminCanonicalEmailBlock {
            id: self.next_id(),
            canonical_email_hash: email_hash.to_string(),
            created_at_ms: now_ms,
        };
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCKS)?;
            rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
        }
        {
            let mut by_hash = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCK_BY_HASH)?;
            by_hash.insert(email_hash, row.id.value())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn admin_canonical_email_block_by_id(
        &self,
        id: Id,
    ) -> Result<Option<AdminCanonicalEmailBlock>> {
        get_by_desc(self, ADMIN_CANONICAL_EMAIL_BLOCKS, id)
    }

    fn admin_canonical_email_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminCanonicalEmailBlock>> {
        page_desc(
            self,
            ADMIN_CANONICAL_EMAIL_BLOCKS,
            max_id,
            since_id,
            min_id,
            limit,
        )
    }

    fn admin_canonical_blocks_for_hash(&self, email_hash: &str) -> Result<Vec<AdminCanonicalEmailBlock>> {
        if email_hash.is_empty() {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let rows = read.open_table(ADMIN_CANONICAL_EMAIL_BLOCKS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in rows.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > PAGE_SCAN_CAP {
                break;
            }
            let row: AdminCanonicalEmailBlock = decode(value.value())?;
            if row.canonical_email_hash == email_hash {
                out.push(row);
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn admin_delete_canonical_email_block(
        &self,
        id: Id,
    ) -> Result<Option<AdminCanonicalEmailBlock>> {
        let existing: Option<AdminCanonicalEmailBlock> =
            get_by_desc(self, ADMIN_CANONICAL_EMAIL_BLOCKS, id)?;
        let Some(row) = existing else { return Ok(None) };
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCKS)?;
            rows.remove(schema::desc_id(id.value()))?;
        }
        {
            let mut by_hash = write.open_table(ADMIN_CANONICAL_EMAIL_BLOCK_BY_HASH)?;
            by_hash.remove(row.canonical_email_hash.as_str())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    // -- Tags --

    fn admin_ensure_tag(&self, name: &str, now_ms: u64) -> Result<AdminTag> {
        let write = self.begin_write()?;
        let existing_id: Option<u64> = {
            let by_name = write.open_table(ADMIN_TAG_BY_NAME)?;
            by_name.get(name)?.map(|guard| guard.value())
        };
        if let Some(existing_id) = existing_id {
            let id = Id::new(existing_id);
            let row: Option<AdminTag> = {
                let rows = write.open_table(ADMIN_TAGS)?;
                match rows.get(schema::desc_id(id.value()))? {
                    Some(guard) => Some(decode(guard.value())?),
                    None => None,
                }
            };
            if let Some(row) = row {
                return Ok(row);
            }
        }
        let row = AdminTag {
            id: self.next_id(),
            name: name.to_string(),
            display_name: name.to_string(),
            trendable: None,
            usable: None,
            listable: None,
            reviewed_at_ms: None,
            created_at_ms: now_ms,
        };
        {
            let mut rows = write.open_table(ADMIN_TAGS)?;
            rows.insert(schema::desc_id(row.id.value()), encode(&row)?.as_slice())?;
        }
        {
            let mut by_name = write.open_table(ADMIN_TAG_BY_NAME)?;
            by_name.insert(name, row.id.value())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn admin_tags_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminTag>> {
        page_desc(self, ADMIN_TAGS, max_id, since_id, min_id, limit)
    }

    fn admin_tag_by_id(&self, id: Id) -> Result<Option<AdminTag>> {
        get_by_desc(self, ADMIN_TAGS, id)
    }

    fn admin_update_tag(
        &self,
        id: Id,
        patch: AdminTagPatch,
        now_ms: u64,
    ) -> Result<Option<AdminTag>> {
        let existing: Option<AdminTag> = get_by_desc(self, ADMIN_TAGS, id)?;
        let Some(mut row) = existing else { return Ok(None) };
        if let Some(display_name) = patch.display_name {
            if display_name.trim().is_empty() {
                return Err(Error::InvalidInput(
                    "Validation failed: Display name can't be blank".into(),
                ));
            }
            row.display_name = display_name;
        }
        if let Some(trendable) = patch.trendable {
            row.trendable = Some(trendable);
        }
        if let Some(usable) = patch.usable {
            row.usable = Some(usable);
        }
        if let Some(listable) = patch.listable {
            row.listable = Some(listable);
        }
        // Upstream stamps `reviewed_at` on every admin tag update.
        row.reviewed_at_ms = Some(now_ms);
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ADMIN_TAGS)?;
            rows.insert(schema::desc_id(id.value()), encode(&row)?.as_slice())?;
        }
        write.commit()?;
        Ok(Some(row))
    }

    // -- Audit trail --

    fn admin_audit(
        &self,
        actor_account_id: Id,
        action: &str,
        target_type: &str,
        target_id: &str,
        now_ms: u64,
    ) -> Result<AdminAuditRecord> {
        let record = AdminAuditRecord {
            id: self.next_id(),
            actor_account_id,
            action: action.to_string(),
            target_type: target_type.to_string(),
            target_id: target_id.to_string(),
            created_at_ms: now_ms,
        };
        let write = self.begin_write()?;
        {
            let mut log = write.open_table(ADMIN_AUDIT)?;
            log.insert(record.id.value(), encode(&record)?.as_slice())?;
        }
        write.commit()?;
        Ok(record)
    }

    fn admin_audit_log(&self, limit: usize) -> Result<Vec<AdminAuditRecord>> {
        let read = self.begin_read()?;
        let log = read.open_table(ADMIN_AUDIT)?;
        let mut out = Vec::new();
        for entry in log.range(0u64..=u64::MAX)?.rev() {
            let (_key, value) = entry?;
            out.push(decode(value.value())?);
            if out.len() >= limit.max(1) {
                break;
            }
        }
        Ok(out)
    }

    // -- Bounded metric scans --

    fn admin_account_created_times(&self, cap: usize) -> Result<Vec<u64>> {
        let read = self.begin_read()?;
        let accounts = read.open_table(ACCOUNTS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in accounts.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let account: Account = decode(value.value())?;
            out.push(account.created_at_ms);
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        out.sort_unstable();
        Ok(out)
    }

    fn admin_status_metrics(&self, cap: usize) -> Result<Vec<StatusMetricRow>> {
        let read = self.begin_read()?;
        let statuses = read.open_table(STATUSES)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in statuses.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let status: Status = decode(value.value())?;
            if status.deleted_at_ms.is_some() {
                continue;
            }
            out.push(StatusMetricRow {
                account_id: status.account_id,
                created_at_ms: status.created_at_ms,
                language: status.language.clone(),
                application_name: status.application.as_ref().map(|app| app.name.clone()),
                is_reblog: status.is_reblog(),
            });
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        out.sort_by_key(|row| row.created_at_ms);
        Ok(out)
    }

    fn admin_report_created_times(&self, cap: usize) -> Result<Vec<u64>> {
        let read = self.begin_read()?;
        let reports = read.open_table(REPORTS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in reports.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let report: Report = decode(value.value())?;
            out.push(report.created_at_ms);
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        out.sort_unstable();
        Ok(out)
    }

    fn admin_report_resolved_times(&self, cap: usize) -> Result<Vec<u64>> {
        let read = self.begin_read()?;
        let reports = read.open_table(REPORTS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in reports.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let report: Report = decode(value.value())?;
            if let Some(at) = report.action_taken_at_ms {
                out.push(at);
            }
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        out.sort_unstable();
        Ok(out)
    }

    fn admin_media_metrics(&self, cap: usize) -> Result<Vec<(u64, u64)>> {
        let read = self.begin_read()?;
        let media = read.open_table(MEDIA)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in media.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let attachment: MediaAttachment = decode(value.value())?;
            out.push((attachment.created_at_ms, attachment.file_size));
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        out.sort_by_key(|&(created, _)| created);
        Ok(out)
    }

    fn admin_remote_domains(&self, cap: usize) -> Result<Vec<(String, u64)>> {
        let read = self.begin_read()?;
        let accounts = read.open_table(ACCOUNTS)?;
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut scanned = 0usize;
        for entry in accounts.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let account: Account = decode(value.value())?;
            if !account.local {
                *counts.entry(account.domain.clone()).or_insert(0) += 1;
            }
        }
        self.note_scanned(scanned);
        let mut out: Vec<(String, u64)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.truncate(cap.max(1));
        Ok(out)
    }

    fn admin_local_domain(&self) -> String {
        // The store does not carry `ServerInfo`; the endpoint supplies the
        // domain for instance-scoped metric params. The trait method exists so
        // callers behind `&dyn Store` share one spelling.
        String::new()
    }

    /// Round 3 (D39 `space_usage`): the live `redb` file size, measured from
    /// the filesystem — never estimated. `None` when the file cannot be
    /// statted (the caller omits the entry). postgres/redis sizes are never
    /// reported: this server has neither.
    fn admin_db_size_bytes(&self) -> Result<Option<u64>> {
        match std::fs::metadata(self.path()) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(_) => Ok(None),
        }
    }

    fn admin_accounts_overview(&self, cap: usize) -> Result<Vec<AdminAccountOverview>> {
        let read = self.begin_read()?;
        let accounts = read.open_table(ACCOUNTS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in accounts.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let account: Account = decode(value.value())?;
            out.push(AdminAccountOverview {
                id: account.id,
                created_at_ms: account.created_at_ms,
                domain: account.domain,
                local: account.local,
                username: account.username,
            });
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn admin_report_targets(&self, cap: usize) -> Result<Vec<AdminReportTarget>> {
        let read = self.begin_read()?;
        let reports = read.open_table(REPORTS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in reports.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let report: Report = decode(value.value())?;
            out.push(AdminReportTarget {
                created_at_ms: report.created_at_ms,
                target_account_id: report.target_account_id,
            });
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn admin_media_owners(&self, cap: usize) -> Result<Vec<AdminMediaOwner>> {
        let read = self.begin_read()?;
        let media = read.open_table(MEDIA)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in media.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let attachment: MediaAttachment = decode(value.value())?;
            out.push(AdminMediaOwner {
                created_at_ms: attachment.created_at_ms,
                file_size: attachment.file_size,
                owner_id: attachment.account_id,
            });
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn admin_follow_metrics(&self, cap: usize) -> Result<Vec<FollowMetricRow>> {
        let read = self.begin_read()?;
        let follows = read.open_table(FOLLOWS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in follows.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let follow: Follow = decode(value.value())?;
            out.push(FollowMetricRow {
                follower_id: follow.follower_id,
                target_id: follow.followed_id,
                created_at_ms: follow.created_at_ms,
            });
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn admin_sign_ins(&self, cap: usize) -> Result<Vec<(Id, u64)>> {
        let read = self.begin_read()?;
        let table = read.open_table(SIGN_INS)?;
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in table.iter()? {
            let (_key, value) = entry?;
            scanned += 1;
            if scanned > METRIC_SCAN_CAP {
                break;
            }
            let event: SignInEvent = decode(value.value())?;
            out.push((event.account_id, event.created_at_ms));
            if out.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(out)
    }

    fn admin_create_rule(&self, text: &str, hint: &str, now_ms: u64) -> Result<Rule> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(Error::InvalidInput(
                "Validation failed: Text can't be blank".into(),
            ));
        }
        if text.chars().count() > crate::domain::RULE_TEXT_LIMIT {
            return Err(Error::InvalidInput(
                "Validation failed: Text is too long".into(),
            ));
        }
        let write = self.begin_write()?;
        let rule = Rule {
            id: self.next_id(),
            text,
            hint: hint.to_string(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(RULES)?;
            table.insert(rule.id.value(), encode(&rule)?.as_slice())?;
        }
        write.commit()?;
        Ok(rule)
    }

    fn admin_delete_rule(&self, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(RULES)?;
            table.remove(id.value())?.is_some()
        };
        write.commit()?;
        Ok(removed)
    }

    fn admin_rules(&self) -> Result<Vec<Rule>> {
        let read = self.begin_read()?;
        let table = read.open_table(RULES)?;
        let mut out = Vec::new();
        for entry in table.range(0u64..)? {
            let (_, value) = entry?;
            out.push(decode(value.value())?);
        }
        Ok(out)
    }

    fn admin_rules_by_ids(&self, ids: &[Id]) -> Result<Vec<Rule>> {
        let read = self.begin_read()?;
        let table = read.open_table(RULES)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(guard) = table.get(id.value())? {
                out.push(decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn admin_rule_translations(&self) -> Result<Vec<RuleTranslation>> {
        let read = self.begin_read()?;
        let table = read.open_table(RULE_TRANSLATIONS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            out.push(decode(value.value())?);
        }
        out.sort_by(|a: &RuleTranslation, b: &RuleTranslation| {
            a.rule_id
                .value()
                .cmp(&b.rule_id.value())
                .then_with(|| a.locale.cmp(&b.locale))
        });
        Ok(out)
    }

    fn admin_set_rule_translation(
        &self,
        rule_id: Id,
        locale: String,
        text: String,
        hint: String,
        now_ms: u64,
    ) -> Result<RuleTranslation> {
        let locale = locale.trim().to_string();
        if locale.is_empty()
            || locale.len() > RULE_LOCALE_LIMIT
            || !locale.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::InvalidInput(format!(
                "rule locale {locale:?} is not a BCP-47 tag"
            )));
        }
        if text.chars().count() > RULE_TEXT_LIMIT {
            return Err(Error::InvalidInput("Validation failed: Text is too long".into()));
        }
        let write = self.begin_write()?;
        {
            let rules = write.open_table(RULES)?;
            if rules.get(rule_id.value())?.is_none() {
                return Err(Error::NotFound(format!("rule {rule_id}")));
            }
        }
        let row = RuleTranslation {
            rule_id,
            locale: locale.clone(),
            text,
            hint,
            updated_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(RULE_TRANSLATIONS)?;
            table.insert(
                super::schema::rule_translation_key(rule_id.value(), &locale).as_str(),
                encode(&row)?.as_slice(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn admin_delete_rule_translation(&self, rule_id: Id, locale: &str) -> Result<bool> {
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(RULE_TRANSLATIONS)?;
            table
                .remove(super::schema::rule_translation_key(rule_id.value(), locale).as_str())?
                .is_some()
        };
        write.commit()?;
        Ok(removed)
    }

    fn admin_hashtag_names(&self, cap: usize) -> Result<Vec<String>> {
        let read = self.begin_read()?;
        let index = read.open_table(HASHTAG_INDEX)?;
        let mut tags = BTreeSet::new();
        let mut scanned = 0usize;
        for entry in index.iter()? {
            let (key, _value) = entry?;
            scanned += 1;
            if scanned > TAG_SCAN_CAP {
                break;
            }
            let bytes = key.value();
            let end = bytes.iter().position(|&b| b == 0xFF).unwrap_or(bytes.len());
            let tag = String::from_utf8_lossy(&bytes[..end]).to_string();
            tags.insert(tag);
            if tags.len() >= cap.max(1) {
                break;
            }
        }
        self.note_scanned(scanned);
        Ok(tags.into_iter().collect())
    }
}

/// Distinct hashtag names from the hashtag index (bounded), for tag registry
/// materialization. Exported for the endpoint module: the GET index ensures a
/// row per real tag before paging, so the registry is never fabricated.
pub fn collect_hashtag_names(store: &RedbStore, cap: usize) -> Result<Vec<String>> {
    let read = store.begin_read()?;
    let index = read.open_table(HASHTAG_INDEX)?;
    let mut tags = BTreeSet::new();
    let mut scanned = 0usize;
    for entry in index.iter()? {
        let (key, _value) = entry?;
        scanned += 1;
        if scanned > TAG_SCAN_CAP {
            break;
        }
        let bytes = key.value();
        let end = bytes.iter().position(|&b| b == 0xFF).unwrap_or(bytes.len());
        let tag = String::from_utf8_lossy(&bytes[..end]).to_string();
        tags.insert(tag);
        if tags.len() >= cap.max(1) {
            break;
        }
    }
    store.note_scanned(scanned);
    Ok(tags.into_iter().collect())
}


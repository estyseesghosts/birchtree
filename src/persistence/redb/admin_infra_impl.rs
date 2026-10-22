//! Admin infrastructure store: the single `AdminInfraStore` implementation.
//!
//! S6 split the resource surface into per-area modules with plain
//! `pub(super)` functions (`admin_infra_{domain,email,ip,tags,metrics}`,
//! `admin_infra_audit`); this file owns the ONE `impl AdminInfraStore for
//! `RedbStore` block and delegates every method, so trait coherence holds
//! while each resource body lives next to its tables. No behavior change:
//! delegation is one call deep with identical arguments.

use crate::application::ports::{
    AdminDomainBlockPatch, AdminInfraStore, AdminIpBlockPatch, AdminTagPatch, StatusMetricRow,
};
use crate::domain::{
    AdminAccountOverview, AdminAuditRecord, AdminCanonicalEmailBlock, AdminDomainAllow,
    AdminDomainBlock, AdminEmailDomainBlock, AdminIpBlock, AdminIpSeverity, AdminMediaOwner,
    AdminReportTarget, AdminTag, FollowMetricRow, Id, Result, Rule, RuleTranslation,
};

use super::RedbStore;

impl AdminInfraStore for RedbStore {
    // -- Delegations to the per-resource modules (S6 split) --

    fn admin_create_domain_allow(&self, domain: &str, now_ms: u64) -> Result<AdminDomainAllow> {
        super::admin_infra_domain::admin_create_domain_allow(self, domain, now_ms)
    }
    fn admin_domain_allow_by_id(&self, id: Id) -> Result<Option<AdminDomainAllow>> {
        super::admin_infra_domain::admin_domain_allow_by_id(self, id)
    }
    fn admin_domain_allows_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminDomainAllow>> {
        super::admin_infra_domain::admin_domain_allows_page(self, max_id, since_id, min_id, limit)
    }
    fn admin_delete_domain_allow(&self, id: Id) -> Result<Option<AdminDomainAllow>> {
        super::admin_infra_domain::admin_delete_domain_allow(self, id)
    }
    fn admin_create_domain_block(
        &self,
        domain: &str,
        patch: AdminDomainBlockPatch,
        now_ms: u64,
    ) -> Result<AdminDomainBlock> {
        super::admin_infra_domain::admin_create_domain_block(self, domain, patch, now_ms)
    }
    fn admin_domain_block_by_id(&self, id: Id) -> Result<Option<AdminDomainBlock>> {
        super::admin_infra_domain::admin_domain_block_by_id(self, id)
    }
    fn admin_domain_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminDomainBlock>> {
        super::admin_infra_domain::admin_domain_blocks_page(self, max_id, since_id, min_id, limit)
    }
    fn admin_update_domain_block(
        &self,
        id: Id,
        patch: AdminDomainBlockPatch,
        _now_ms: u64,
    ) -> Result<Option<AdminDomainBlock>> {
        super::admin_infra_domain::admin_update_domain_block(self, id, patch, _now_ms)
    }
    fn admin_delete_domain_block(&self, id: Id) -> Result<Option<AdminDomainBlock>> {
        super::admin_infra_domain::admin_delete_domain_block(self, id)
    }
    fn admin_create_email_domain_block(
        &self,
        domain: &str,
        allow_with_approval: bool,
        now_ms: u64,
    ) -> Result<AdminEmailDomainBlock> {
        super::admin_infra_email::admin_create_email_domain_block(
            self,
            domain,
            allow_with_approval,
            now_ms,
        )
    }
    fn admin_email_domain_block_by_id(&self, id: Id) -> Result<Option<AdminEmailDomainBlock>> {
        super::admin_infra_email::admin_email_domain_block_by_id(self, id)
    }
    fn email_domain_block_for_domain(&self, domain: &str) -> Result<Option<AdminEmailDomainBlock>> {
        super::admin_infra_email::email_domain_block_for_domain(self, domain)
    }
    fn admin_email_domain_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminEmailDomainBlock>> {
        super::admin_infra_email::admin_email_domain_blocks_page(
            self, max_id, since_id, min_id, limit,
        )
    }
    fn admin_delete_email_domain_block(&self, id: Id) -> Result<Option<AdminEmailDomainBlock>> {
        super::admin_infra_email::admin_delete_email_domain_block(self, id)
    }
    fn admin_create_canonical_email_block(
        &self,
        email_hash: &str,
        now_ms: u64,
    ) -> Result<AdminCanonicalEmailBlock> {
        super::admin_infra_email::admin_create_canonical_email_block(self, email_hash, now_ms)
    }
    fn admin_canonical_email_block_by_id(
        &self,
        id: Id,
    ) -> Result<Option<AdminCanonicalEmailBlock>> {
        super::admin_infra_email::admin_canonical_email_block_by_id(self, id)
    }
    fn admin_canonical_email_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminCanonicalEmailBlock>> {
        super::admin_infra_email::admin_canonical_email_blocks_page(
            self, max_id, since_id, min_id, limit,
        )
    }
    fn admin_canonical_blocks_for_hash(
        &self,
        email_hash: &str,
    ) -> Result<Vec<AdminCanonicalEmailBlock>> {
        super::admin_infra_email::admin_canonical_blocks_for_hash(self, email_hash)
    }
    fn admin_delete_canonical_email_block(
        &self,
        id: Id,
    ) -> Result<Option<AdminCanonicalEmailBlock>> {
        super::admin_infra_email::admin_delete_canonical_email_block(self, id)
    }
    fn admin_create_ip_block(
        &self,
        ip: &str,
        severity: AdminIpSeverity,
        comment: &str,
        expires_at_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<AdminIpBlock> {
        super::admin_infra_ip::admin_create_ip_block(
            self,
            ip,
            severity,
            comment,
            expires_at_ms,
            now_ms,
        )
    }
    fn admin_ip_block_by_id(&self, id: Id) -> Result<Option<AdminIpBlock>> {
        super::admin_infra_ip::admin_ip_block_by_id(self, id)
    }
    fn admin_ip_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminIpBlock>> {
        super::admin_infra_ip::admin_ip_blocks_page(self, max_id, since_id, min_id, limit)
    }
    fn admin_ip_blocks_all(&self, cap: usize) -> Result<Vec<AdminIpBlock>> {
        super::admin_infra_ip::admin_ip_blocks_all(self, cap)
    }
    fn admin_update_ip_block(
        &self,
        id: Id,
        patch: AdminIpBlockPatch,
        _now_ms: u64,
    ) -> Result<Option<AdminIpBlock>> {
        super::admin_infra_ip::admin_update_ip_block(self, id, patch, _now_ms)
    }
    fn admin_delete_ip_block(&self, id: Id) -> Result<Option<AdminIpBlock>> {
        super::admin_infra_ip::admin_delete_ip_block(self, id)
    }
    fn admin_ensure_tag(&self, name: &str, now_ms: u64) -> Result<AdminTag> {
        super::admin_infra_tags::admin_ensure_tag(self, name, now_ms)
    }
    fn admin_tags_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminTag>> {
        super::admin_infra_tags::admin_tags_page(self, max_id, since_id, min_id, limit)
    }
    fn admin_tag_by_id(&self, id: Id) -> Result<Option<AdminTag>> {
        super::admin_infra_tags::admin_tag_by_id(self, id)
    }
    fn admin_update_tag(
        &self,
        id: Id,
        patch: AdminTagPatch,
        now_ms: u64,
    ) -> Result<Option<AdminTag>> {
        super::admin_infra_tags::admin_update_tag(self, id, patch, now_ms)
    }
    fn admin_hashtag_names(&self, cap: usize) -> Result<Vec<String>> {
        super::admin_infra_tags::admin_hashtag_names(self, cap)
    }
    fn admin_account_created_times(&self, cap: usize) -> Result<Vec<u64>> {
        super::admin_infra_metrics::admin_account_created_times(self, cap)
    }
    fn admin_status_metrics(&self, cap: usize) -> Result<Vec<StatusMetricRow>> {
        super::admin_infra_metrics::admin_status_metrics(self, cap)
    }
    fn admin_report_created_times(&self, cap: usize) -> Result<Vec<u64>> {
        super::admin_infra_metrics::admin_report_created_times(self, cap)
    }
    fn admin_report_resolved_times(&self, cap: usize) -> Result<Vec<u64>> {
        super::admin_infra_metrics::admin_report_resolved_times(self, cap)
    }
    fn admin_media_metrics(&self, cap: usize) -> Result<Vec<(u64, u64)>> {
        super::admin_infra_metrics::admin_media_metrics(self, cap)
    }
    fn admin_remote_domains(&self, cap: usize) -> Result<Vec<(String, u64)>> {
        super::admin_infra_metrics::admin_remote_domains(self, cap)
    }
    fn admin_local_domain(&self) -> String {
        super::admin_infra_metrics::admin_local_domain(self)
    }
    fn admin_db_size_bytes(&self) -> Result<Option<u64>> {
        super::admin_infra_metrics::admin_db_size_bytes(self)
    }
    fn admin_accounts_overview(&self, cap: usize) -> Result<Vec<AdminAccountOverview>> {
        super::admin_infra_metrics::admin_accounts_overview(self, cap)
    }
    fn admin_report_targets(&self, cap: usize) -> Result<Vec<AdminReportTarget>> {
        super::admin_infra_metrics::admin_report_targets(self, cap)
    }
    fn admin_media_owners(&self, cap: usize) -> Result<Vec<AdminMediaOwner>> {
        super::admin_infra_metrics::admin_media_owners(self, cap)
    }
    fn admin_follow_metrics(&self, cap: usize) -> Result<Vec<FollowMetricRow>> {
        super::admin_infra_metrics::admin_follow_metrics(self, cap)
    }
    fn admin_sign_ins(&self, cap: usize) -> Result<Vec<(Id, u64)>> {
        super::admin_infra_metrics::admin_sign_ins(self, cap)
    }
    fn admin_audit(
        &self,
        actor_account_id: Id,
        action: &str,
        target_type: &str,
        target_id: &str,
        now_ms: u64,
    ) -> Result<AdminAuditRecord> {
        {
            super::admin_infra_audit::admin_audit(
                self,
                actor_account_id,
                action,
                target_type,
                target_id,
                now_ms,
            )
        }
    }
    fn admin_audit_log(&self, limit: usize) -> Result<Vec<AdminAuditRecord>> {
        { super::admin_infra_audit::admin_audit_log(self, limit) }
    }
    fn admin_create_rule(&self, text: &str, hint: &str, now_ms: u64) -> Result<Rule> {
        { super::admin_infra_audit::admin_create_rule(self, text, hint, now_ms) }
    }
    fn admin_delete_rule(&self, id: Id) -> Result<bool> {
        { super::admin_infra_audit::admin_delete_rule(self, id) }
    }
    fn admin_rules(&self) -> Result<Vec<Rule>> {
        { super::admin_infra_audit::admin_rules(self) }
    }
    fn admin_rules_by_ids(&self, ids: &[Id]) -> Result<Vec<Rule>> {
        { super::admin_infra_audit::admin_rules_by_ids(self, ids) }
    }
    fn admin_rule_translations(&self) -> Result<Vec<RuleTranslation>> {
        { super::admin_infra_audit::admin_rule_translations(self) }
    }
    fn admin_set_rule_translation(
        &self,
        rule_id: Id,
        locale: String,
        text: String,
        hint: String,
        now_ms: u64,
    ) -> Result<RuleTranslation> {
        {
            super::admin_infra_audit::admin_set_rule_translation(
                self, rule_id, locale, text, hint, now_ms,
            )
        }
    }
    fn admin_delete_rule_translation(&self, rule_id: Id, locale: &str) -> Result<bool> {
        { super::admin_infra_audit::admin_delete_rule_translation(self, rule_id, locale) }
    }
}

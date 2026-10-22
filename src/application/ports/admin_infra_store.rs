use crate::domain::AdminAccountOverview;
use crate::domain::AdminAuditRecord;
use crate::domain::AdminCanonicalEmailBlock;
use crate::domain::AdminDomainAllow;
use crate::domain::AdminDomainBlock;
use crate::domain::AdminDomainSeverity;
use crate::domain::AdminEmailDomainBlock;
use crate::domain::AdminIpBlock;
use crate::domain::AdminIpSeverity;
use crate::domain::AdminMediaOwner;
use crate::domain::AdminReportTarget;
use crate::domain::AdminTag;
use crate::domain::Error;
use crate::domain::FollowMetricRow;
use crate::domain::Id;
use crate::domain::Result;
use crate::domain::Rule;
use crate::domain::RuleTranslation;

/// A domain-block write (create or update).
#[derive(Debug, Clone, Default)]
pub struct AdminDomainBlockPatch {
    pub severity: Option<AdminDomainSeverity>,
    pub reject_media: Option<bool>,
    pub reject_reports: Option<bool>,
    /// `Some(None)` clears the comment; `None` leaves it unchanged.
    pub private_comment: Option<Option<String>>,
    pub public_comment: Option<Option<String>>,
    pub obfuscate: Option<bool>,
}

/// An IP-block write (create or update).
#[derive(Debug, Clone, Default)]
pub struct AdminIpBlockPatch {
    pub severity: Option<AdminIpSeverity>,
    /// `Some(None)` clears the comment; `None` leaves it unchanged.
    pub comment: Option<Option<String>>,
    /// `Some(None)` clears the expiry; `None` leaves it unchanged.
    pub expires_at_ms: Option<Option<u64>>,
}

/// A tag-moderation write.
#[derive(Debug, Clone, Default)]
pub struct AdminTagPatch {
    pub display_name: Option<String>,
    pub trendable: Option<bool>,
    pub usable: Option<bool>,
    pub listable: Option<bool>,
}

/// One status row projected for the metrics endpoints.
#[derive(Debug, Clone)]
pub struct StatusMetricRow {
    pub account_id: Id,
    pub created_at_ms: u64,
    pub language: Option<String>,
    pub application_name: Option<String>,
    /// True for reblog wrappers (excluded from language breakdowns, like
    /// upstream's `statuses.reblog_of_id IS NULL`).
    pub is_reblog: bool,
}

/// Admin infrastructure rows: domain allows/blocks, email-domain blocks, IP
/// blocks, canonical email blocks, admin tag flags, the audit trail, and
/// bounded metric scans (Phase 7.3, subagent C).
///
/// One `redb` transaction per method: the record mutation, its indexes and the
/// audit row commit together.
pub trait AdminInfraStore: Send + Sync {
    // -- Domain allows --
    /// Insert a domain allow (`NotFound`-free; duplicate domains return the
    /// existing row, like upstream's `find_by || create!`). Invalid domains
    /// are rejected with the pinned `Validation failed` messages.
    fn admin_create_domain_allow(&self, domain: &str, now_ms: u64) -> Result<AdminDomainAllow>;
    fn admin_domain_allow_by_id(&self, id: Id) -> Result<Option<AdminDomainAllow>>;
    /// Global allow list, newest first; cursors exclusive over row ids.
    fn admin_domain_allows_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminDomainAllow>>;
    fn admin_delete_domain_allow(&self, id: Id) -> Result<Option<AdminDomainAllow>>;

    // -- Domain blocks --
    fn admin_create_domain_block(
        &self,
        domain: &str,
        patch: AdminDomainBlockPatch,
        now_ms: u64,
    ) -> Result<AdminDomainBlock>;
    fn admin_domain_block_by_id(&self, id: Id) -> Result<Option<AdminDomainBlock>>;
    fn admin_domain_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminDomainBlock>>;
    fn admin_update_domain_block(
        &self,
        id: Id,
        patch: AdminDomainBlockPatch,
        now_ms: u64,
    ) -> Result<Option<AdminDomainBlock>>;
    fn admin_delete_domain_block(&self, id: Id) -> Result<Option<AdminDomainBlock>>;

    // -- Email-domain blocks --
    fn admin_create_email_domain_block(
        &self,
        domain: &str,
        allow_with_approval: bool,
        now_ms: u64,
    ) -> Result<AdminEmailDomainBlock>;
    fn admin_email_domain_block_by_id(&self, id: Id) -> Result<Option<AdminEmailDomainBlock>>;
    fn admin_email_domain_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminEmailDomainBlock>>;
    /// The email-domain block for one normalized domain, if any (targeted
    /// signup lookup; domains are unique at creation, so at most one row).
    /// `None` for blank/unknown domains — never an error.
    fn email_domain_block_for_domain(&self, domain: &str) -> Result<Option<AdminEmailDomainBlock>>;
    fn admin_delete_email_domain_block(&self, id: Id) -> Result<Option<AdminEmailDomainBlock>>;

    // -- IP blocks --
    fn admin_create_ip_block(
        &self,
        ip: &str,
        severity: AdminIpSeverity,
        comment: &str,
        expires_at_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<AdminIpBlock>;
    fn admin_ip_block_by_id(&self, id: Id) -> Result<Option<AdminIpBlock>>;
    fn admin_ip_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminIpBlock>>;
    /// All IP blocks (bounded by `cap`), for request-path enforcement.
    fn admin_ip_blocks_all(&self, cap: usize) -> Result<Vec<AdminIpBlock>>;
    fn admin_update_ip_block(
        &self,
        id: Id,
        patch: AdminIpBlockPatch,
        now_ms: u64,
    ) -> Result<Option<AdminIpBlock>>;
    fn admin_delete_ip_block(&self, id: Id) -> Result<Option<AdminIpBlock>>;

    // -- Canonical email blocks --
    fn admin_create_canonical_email_block(
        &self,
        email_hash: &str,
        now_ms: u64,
    ) -> Result<AdminCanonicalEmailBlock>;
    fn admin_canonical_email_block_by_id(&self, id: Id)
    -> Result<Option<AdminCanonicalEmailBlock>>;
    fn admin_canonical_email_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminCanonicalEmailBlock>>;
    /// Rows whose hash matches (the `test` endpoint), newest first.
    fn admin_canonical_blocks_for_hash(
        &self,
        email_hash: &str,
    ) -> Result<Vec<AdminCanonicalEmailBlock>>;
    fn admin_delete_canonical_email_block(
        &self,
        id: Id,
    ) -> Result<Option<AdminCanonicalEmailBlock>>;

    // -- Tags --
    /// Ensure a tag row for `name` (idempotent; materializes real hashtag
    /// usage — never fabricated).
    fn admin_ensure_tag(&self, name: &str, now_ms: u64) -> Result<AdminTag>;
    fn admin_tags_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminTag>>;
    fn admin_tag_by_id(&self, id: Id) -> Result<Option<AdminTag>>;
    fn admin_update_tag(
        &self,
        id: Id,
        patch: AdminTagPatch,
        now_ms: u64,
    ) -> Result<Option<AdminTag>>;

    // -- Audit trail --
    /// Append one audit record per admin write (proposed default: on).
    fn admin_audit(
        &self,
        actor_account_id: Id,
        action: &str,
        target_type: &str,
        target_id: &str,
        now_ms: u64,
    ) -> Result<AdminAuditRecord>;
    /// Recent audit records, newest first, bounded by `limit`.
    fn admin_audit_log(&self, limit: usize) -> Result<Vec<AdminAuditRecord>>;

    // -- Bounded metric scans (measures/dimensions/retention) --
    /// Account creation times, oldest first, bounded by `cap`.
    fn admin_account_created_times(&self, cap: usize) -> Result<Vec<u64>>;
    /// `(id, created_at, domain, local)` per account, bounded by `cap`.
    fn admin_accounts_overview(&self, cap: usize) -> Result<Vec<AdminAccountOverview>>;
    /// `(created_at_ms, language, application_name)` per status, oldest
    /// first, bounded by `cap`.
    fn admin_status_metrics(&self, cap: usize) -> Result<Vec<StatusMetricRow>>;
    /// Report creation times, oldest first, bounded by `cap`.
    fn admin_report_created_times(&self, cap: usize) -> Result<Vec<u64>>;
    /// Resolution times (`action_taken_at`) of resolved reports, bounded by
    /// `cap` (D39 `resolved_reports` measure).
    fn admin_report_resolved_times(&self, cap: usize) -> Result<Vec<u64>>;
    /// `(created_at, target_account_id)` per report, bounded by `cap`.
    fn admin_report_targets(&self, cap: usize) -> Result<Vec<AdminReportTarget>>;
    /// `(created_at_ms, file_size)` per media attachment, oldest first,
    /// bounded by `cap`.
    fn admin_media_metrics(&self, cap: usize) -> Result<Vec<(u64, u64)>>;
    /// `(created_at, file_size, owner_id)` per media attachment, bounded.
    fn admin_media_owners(&self, cap: usize) -> Result<Vec<AdminMediaOwner>>;
    /// Distinct hashtag names from the hashtag index, bounded by `cap`
    /// (tag registry materialization — real usage, never fabricated).
    fn admin_hashtag_names(&self, cap: usize) -> Result<Vec<String>>;
    /// `(domain, account_count)` for remote domains, bounded by `cap`.
    fn admin_remote_domains(&self, cap: usize) -> Result<Vec<(String, u64)>>;
    /// `(follower_id, target_id, created_at)` per follow, bounded by `cap`
    /// (D39 `instance_follows` / `instance_followers` measures).
    fn admin_follow_metrics(&self, cap: usize) -> Result<Vec<FollowMetricRow>>;
    /// `(account_id, created_at)` per recorded password sign-in, bounded by
    /// `cap` (D39 `active_users` measure).
    fn admin_sign_ins(&self, cap: usize) -> Result<Vec<(Id, u64)>>;
    /// Local domain (bare host of the public URL) for instance scoping.
    fn admin_local_domain(&self) -> String;
    /// Size of the underlying database file in bytes, when the backend can
    /// report it (`None` = unknown; the caller omits the entry rather than
    /// fabricating a size). Round 3 (D39 `space_usage`): the default is
    /// unknown so backends without a measurable file stay refused-by-omission.
    fn admin_db_size_bytes(&self) -> Result<Option<u64>> {
        Ok(None)
    }

    // -- Server rules (D37) --
    /// Create one server rule (operator-managed, like custom emoji).
    fn admin_create_rule(&self, text: &str, hint: &str, now_ms: u64) -> Result<Rule>;
    /// Delete one server rule. Reports that referenced it keep the stale id;
    /// serialization skips ids with no rule row (documented).
    fn admin_delete_rule(&self, id: Id) -> Result<bool>;
    /// Every server rule, id-ascending (operator reads).
    fn admin_rules(&self) -> Result<Vec<Rule>>;
    /// The rules for `ids`, in `ids` order, skipping unknown ids.
    fn admin_rules_by_ids(&self, ids: &[Id]) -> Result<Vec<Rule>>;
    /// Every rule translation row, rule-then-locale ordered (gaps batch G-A;
    /// backs the instance rules `translations` map). Empty when none are
    /// stored (the serializer renders `{}` per rule then).
    fn admin_rule_translations(&self) -> Result<Vec<RuleTranslation>> {
        Ok(Vec::new())
    }
    /// Create or replace the `(rule, locale)` translation in ONE
    /// transaction. `NotFound` when the rule is gone; `InvalidInput` on a
    /// malformed locale or over-long text.
    fn admin_set_rule_translation(
        &self,
        _rule_id: Id,
        _locale: String,
        _text: String,
        _hint: String,
        _now_ms: u64,
    ) -> Result<RuleTranslation> {
        Err(Error::Internal(
            "admin_set_rule_translation is not implemented".into(),
        ))
    }
    /// Delete one `(rule, locale)` translation; `true` when a row existed.
    fn admin_delete_rule_translation(&self, _rule_id: Id, _locale: &str) -> Result<bool> {
        Err(Error::Internal(
            "admin_delete_rule_translation is not implemented".into(),
        ))
    }
}

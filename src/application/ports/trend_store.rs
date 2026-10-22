use crate::domain::AdminAuditRecord;
use crate::domain::Id;
use crate::domain::Result;
use crate::domain::TagUsage;
use crate::domain::TrendLink;
use crate::domain::TrendPublisher;
use crate::domain::TrendStatusFlag;
use crate::domain::TrendTagFlag;

/// Trend candidate and approval-flag storage.
pub trait TrendStore: Send + Sync {
    /// Distinct tag usage from the hashtag index, most-used first, bounded by
    /// `cap` distinct tags (candidate input for tag trends and the admin tag
    /// review queue).
    fn trend_tag_usages(&self, cap: usize) -> Result<Vec<TagUsage>>;

    /// Approval flag for one tag name, if reviewed.
    fn trend_tag_flag(&self, name: &str) -> Result<Option<TrendTagFlag>>;
    /// Every reviewed tag flag (the admin review queue joins these with
    /// [`TrendStore::trend_tag_usages`]).
    fn all_trend_tag_flags(&self) -> Result<Vec<TrendTagFlag>>;
    /// Materialize a tag flag row for `name` (idempotent; pending when new),
    /// minting its numeric admin `:id`.
    fn ensure_trend_tag(&self, name: &str, now_ms: u64) -> Result<TrendTagFlag>;
    /// Approve or reject a tag trend in ONE transaction (flag + audit row).
    fn review_trend_tag(
        &self,
        id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendTagFlag>;

    /// Approval flag for one status, if reviewed (absence = pending).
    fn trend_status_flag(&self, status_id: Id) -> Result<Option<TrendStatusFlag>>;
    /// Every reviewed status flag.
    fn all_trend_status_flags(&self) -> Result<Vec<TrendStatusFlag>>;
    /// Approve or reject a status trend in ONE transaction (flag + audit
    /// row). `NotFound` when the status is missing or deleted.
    fn review_trend_status(
        &self,
        status_id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendStatusFlag>;

    /// Materialize link rows for `urls` (idempotent; pending when new),
    /// minting numeric admin `:id`s and publisher rows for their hosts.
    fn ensure_trend_links(&self, urls: &[String], now_ms: u64) -> Result<Vec<TrendLink>>;
    /// One link row by its numeric id.
    fn trend_link_by_id(&self, id: Id) -> Result<Option<TrendLink>>;
    /// Every link row, newest first (both read paths page this in memory;
    /// link candidates are bounded by the status scan that produced them).
    fn all_trend_links(&self) -> Result<Vec<TrendLink>>;
    /// Approve or reject a link trend in ONE transaction (flag + audit row).
    fn review_trend_link(
        &self,
        id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendLink>;

    /// Every publisher row, newest first.
    fn all_trend_publishers(&self) -> Result<Vec<TrendPublisher>>;
    /// One publisher row by its numeric id.
    fn trend_publisher_by_id(&self, id: Id) -> Result<Option<TrendPublisher>>;
    /// Approve or reject a publisher in ONE transaction (flag + audit row).
    fn review_trend_publisher(
        &self,
        id: Id,
        allowed: bool,
        actor_account_id: Id,
        now_ms: u64,
    ) -> Result<TrendPublisher>;

    /// Audit records for one trend target, newest first (review history).
    fn trend_audit_trail(
        &self,
        target_type: &str,
        target_id: &str,
    ) -> Result<Vec<AdminAuditRecord>>;

    /// Newest live status id carrying `tag` (already normalized), if any —
    /// the recency input of the trend-score pipeline. `None` when the tag
    /// has no indexed status.
    fn tag_newest_status_id(&self, tag: &str) -> Result<Option<Id>>;

    /// Store preview-card metadata fetched by the link crawler (D40) in ONE
    /// transaction. Fields are real fetched content or `None` (never
    /// fabricated); `None` arguments leave the stored field unchanged.
    /// `None` when the row is gone.
    fn update_trend_link_card(
        &self,
        id: Id,
        title: Option<String>,
        description: Option<String>,
        provider_name: Option<String>,
        provider_url: Option<String>,
        image_url: Option<String>,
        now_ms: u64,
    ) -> Result<Option<TrendLink>>;
}

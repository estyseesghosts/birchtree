use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::application::streaming::StreamEvent;
use crate::domain::bookmark::Bookmark;
use crate::domain::favourite::Favourite;
use crate::domain::{
    AccessToken, Account, AccountNote, AccountStats, AdminAccountOverview, AdminAuditRecord,
    AdminCanonicalEmailBlock,
    AdminDomainAllow, AdminDomainBlock, AdminDomainSeverity, AdminEmailDomainBlock, AdminIpBlock,
    AdminIpSeverity, AdminMediaOwner, AdminReportTarget, AdminTag, AnnualReport,
    AuthorizationCode, Block, CustomEmoji, CustomFilter,
    DomainBlock,
    EmailToken,
    Endorsement, Error, FilterKeyword, FilterStatus, Follow, FollowMetricRow, Id, Job, JobKind,
    InteractionEvent, InteractionEventKind,
    List,
    LocalUser,
    LoginSession, Marker, MediaAttachment, Mute, NewAnnualReport, Notification, NotificationPolicy,
    NotificationRequest, NotificationType,
    OAuthApplication,
    Poll, QuotePolicy, QuoteRecord, RepliesPolicy, Report, Result, Rule, RuleTranslation,
    ScheduledStatus, Status,
    StatusApplication, StatusVersion, TagUsage, TrendLink, TrendPublisher, TrendStatusFlag,
    TrendTagFlag, UserRole, Visibility,
};

// Re-export the two kernel ports under their canonical names.
pub use crate::domain::{Clock, IdGenerator};

/// Repository inputs and results shared by the ports below.

/// Input for creating a local account and its credential together.
pub struct NewLocalAccount {
    pub username: String,
    pub normalized_username: String,
    pub domain: String,
    pub display_name: String,
    pub password_hash: String,
    pub email: Option<String>,
    pub actor_uri: String,
    pub now_ms: u64,
    /// When true, a `SendConfirmationEmail` job is committed in the same
    /// transaction as the account (public registration). The CLI bootstrap
    /// leaves this false.
    pub send_confirmation_email: bool,
    /// Whether the account starts approved. Public registration sets this
    /// `false` when the operator requires approval (the pending queue);
    /// operator-created accounts are always approved.
    pub approved: bool,
    /// OAuth application that performed the signup (`None` for CLI-created
    /// and pre-attribution records). Recorded on the credential row for the
    /// email-confirmation ownership check (D12 remainder).
    pub created_by_application: Option<Id>,
    /// Account locale captured at registration (upstream `user.locale`).
    pub locale: Option<String>,
    /// Invite-request text captured at registration (`reason` param, upstream
    /// `user.invite_request.text`).
    pub invite_request: Option<String>,
    /// Invite code consumed at registration (`None` when no usable invite was
    /// supplied). Recorded in `INVITE_USES` in the same transaction as the
    /// account (backs `invited_by_account_id` + the `invited_by` filter).
    pub invite_code: Option<String>,
    /// IANA time-zone name captured at registration (upstream `user.time_zone`).
    pub time_zone: Option<String>,
    /// Date of birth (`YYYY-MM-DD`, upstream `user.date_of_birth`).
    pub date_of_birth: Option<String>,
}

/// Input for registering an OAuth application.
pub struct NewApp {
    pub name: String,
    pub client_id: String,
    pub client_secret_hash: Option<String>,
    pub redirect_uris: Vec<String>,
    pub scopes: Vec<String>,
    pub website: Option<String>,
    pub now_ms: u64,
}

/// Input for issuing an access token (hash only; plaintext stays with caller).
pub struct NewToken {
    pub app_id: Id,
    pub user_id: Option<Id>,
    pub token_hash: String,
    pub scopes: Vec<String>,
    pub now_ms: u64,
    pub expires_at_ms: Option<u64>,
}

/// Input for creating an OAuth authorization code (hash only).
pub struct NewAuthCode {
    pub app_id: Id,
    pub user_id: Id,
    pub code_hash: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    pub now_ms: u64,
    pub ttl_ms: u64,
}

/// Input for creating a login session.
pub struct NewSession {
    pub user_id: Id,
    pub token_hash: String,
    pub now_ms: u64,
    pub ttl_ms: u64,
}

/// Mutable subset of a user's preferences (`PATCH update_credentials`).
///
/// `None` fields are left unchanged.
#[derive(Debug, Default, Clone)]
pub struct UserSettingsUpdate {
    pub default_privacy: Option<String>,
    pub default_sensitive: Option<bool>,
    pub default_language: Option<String>,
    pub default_quote_policy: Option<String>,
    /// D22 home mention policy (`show`/`collapse`/`filter`; `None` = unchanged).
    pub home_mention_policy: Option<String>,
    /// D21 chosen content languages (`None` = unchanged; `Some`, possibly
    /// empty, = replace; an empty list clears the preference like upstream's
    /// `compact_blank.presence` normalization).
    pub chosen_languages: Option<Vec<String>>,
    /// Reading preferences (`None` = unchanged). Birchtree extension:
    /// upstream sets these only through the web settings UI; here
    /// `update_credentials[source][display_media|expand_spoilers|
    /// autoplay_gifs]` stores them. `display_media` is one of
    /// `default`/`show_all`/`hide_all` (upstream `user_settings.rb:38`).
    pub web_display_media: Option<String>,
    pub web_expand_spoilers: Option<bool>,
    pub web_auto_play_gifs: Option<bool>,
}

/// A staged avatar/header image waiting to be referenced by an account row.
///
/// The bytes were already staged under `key` (blob store, outside the
/// transaction); the profile commit only records the reference.
#[derive(Debug, Clone)]
pub struct ProfileImageData {
    pub key: String,
    pub content_type: String,
}

/// Mutable subset of an account profile (`PATCH update_credentials`).
///
/// `None` fields are left unchanged.
#[derive(Debug, Default, Clone)]
pub struct AccountProfileUpdate {
    pub display_name: Option<String>,
    pub note: Option<String>,
    pub locked: Option<bool>,
    pub discoverable: Option<bool>,
    pub indexable: Option<bool>,
    pub hide_collections: Option<bool>,
    /// Replace the whole profile-field list (`fields_attributes` semantics).
    pub fields: Option<Vec<crate::domain::AccountField>>,
    /// Replace the attribution-domain list.
    pub attribution_domains: Option<Vec<String>>,
    /// Automated account flag (`update_credentials[bot]`).
    pub bot: Option<bool>,
    /// Core extension CORE-L: author's default license (`None` leaves it
    /// unchanged; `Some(None)` clears; `Some(Some(id))` sets; validated
    /// before the store call).
    pub default_license: Option<Option<u8>>,
    /// Avatar slot: `None` = unchanged, `Some(None)` = clear back to the
    /// default, `Some(Some(data))` = reference the staged blob.
    pub avatar: Option<Option<ProfileImageData>>,
    /// Avatar alt text (upstream `avatar_description`, max 150 chars;
    /// length is enforced at the endpoint).
    pub avatar_description: Option<String>,
    /// Header slot, same presence semantics as `avatar`.
    pub header: Option<Option<ProfileImageData>>,
    /// Header alt text (upstream `header_description`, max 150 chars).
    pub header_description: Option<String>,
}

/// Accounts, local users, and the unique handle index.
pub trait AccountStore: Send + Sync {
    /// Create an account + credential + handle index entry + follow-up job(s) in
    /// one atomic transaction.
    fn create_local_account(&self, input: NewLocalAccount) -> Result<Account>;

    fn account_by_id(&self, id: Id) -> Result<Option<Account>>;

    fn account_by_handle(&self, normalized_username: &str, domain: &str) -> Result<Option<Account>>;

    fn user_by_account_id(&self, account_id: Id) -> Result<Option<LocalUser>>;

    fn local_user_by_handle(
        &self,
        normalized_username: &str,
        domain: &str,
    ) -> Result<Option<LocalUser>>;

    fn account_count(&self) -> Result<u64>;

    /// Accounts by id in ONE transaction (page loader; missing ids are
    /// omitted). Duplicates in `ids` are read once.
    fn accounts_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Account>>;

    /// Apply a profile patch, returning the updated account.
    ///
    /// The commit also enqueues one `FederationEvent` (`profile_update`) so
    /// followers refetch the actor; the event is a no-op for remote rows
    /// (which never reach this path — they sync through `upsert_remote`).
    fn update_account_profile(
        &self,
        account_id: Id,
        update: AccountProfileUpdate,
        now_ms: u64,
    ) -> Result<Account>;

    /// Apply a preferences patch, returning the updated credential record.
    fn update_user_settings(&self, account_id: Id, update: UserSettingsUpdate) -> Result<LocalUser>;

    /// Grant or clear a staff role (`admin`, `moderator`, `none`) in ONE
    /// transaction, returning the updated credential record. `NotFound` when
    /// no local user exists for `account_id` (wrong id or remote account).
    fn set_user_role(&self, account_id: Id, role: UserRole) -> Result<LocalUser>;

    /// Record the account this one moved to (federation `Move`, G-D). Remote
    /// rows included. ONE transaction; `None` clears.
    fn set_moved_to(&self, account_id: Id, moved_to: Option<Id>, now_ms: u64) -> Result<Account>;

    /// Overwrite a REMOTE account's counters from validated collection
    /// totals (federation tail refresh). `None` fields keep their row.
    /// Callers gate on remote rows (local counters are authoritative); the
    /// port itself is a plain upsert.
    fn set_remote_stats(
        &self,
        account_id: Id,
        followers: Option<u64>,
        following: Option<u64>,
        statuses: Option<u64>,
    ) -> Result<AccountStats>;

    /// Record one password sign-in event for the admin `active_users`
    /// telemetry (D39). Called once when a local user completes the browser
    /// password login; API calls on an existing token are not sign-ins.
    /// `ip` is the client IP when the adapter supplied one (backs the admin
    /// `ip`/`ips` fields; upstream `user.ips`).
    fn record_sign_in(
        &self,
        account_id: Id,
        now_ms: u64,
        ip: Option<std::net::IpAddr>,
    ) -> Result<()>;

    /// Invite code consumed at registration, if any (fix round P1/P3; backs
    /// `invited_by_account_id` + the v2 `invited_by` filter). `None` when the
    /// account was not invite-created.
    fn invite_code_for_account(&self, account_id: Id) -> Result<Option<String>>;

    /// Accounts created with an invite issued by `inviter_id`, oldest first,
    /// bounded by `cap` (fix round P1/P3; backs the v2 `invited_by` filter).
    fn accounts_for_inviter(&self, inviter_id: Id, cap: usize) -> Result<Vec<Id>>;

    /// Login IPs for `account_id`, newest first, bounded by `cap`
    /// (fix round P1/P3; backs the admin `ips` field as `(ip, used_at)`
    /// pairs; `ip` is the latest entry).
    fn login_ips_for(
        &self,
        account_id: Id,
        cap: usize,
    ) -> Result<Vec<(std::net::IpAddr, u64)>>;

    /// Resolve a local account by email address (normalized lowercase; D12
    /// public forgot-password). `None` for unknown addresses — the caller,
    /// not this method, decides what to reveal (no enumeration oracle here).
    /// Scaffold default refuses loudly; RedbStore overrides.
    fn account_by_email(&self, _normalized_email: &str) -> Result<Option<Account>> {
        Err(Error::Internal("account_by_email is not implemented".into()))
    }

    /// Flip the email-verified flag (D12 confirmation redemption).
    /// Scaffold default refuses loudly; RedbStore overrides.
    fn set_email_verified(&self, _account_id: Id, _verified: bool) -> Result<LocalUser> {
        Err(Error::Internal("set_email_verified is not implemented".into()))
    }

    /// Store a pending email token (D12 redemption; single-use bearer).
    /// Scaffold default refuses loudly; RedbStore overrides.
    fn store_email_token(&self, _token: &str, _record: EmailToken) -> Result<()> {
        Err(Error::Internal("store_email_token is not implemented".into()))
    }

    /// Consume a pending email token: returns the record and deletes the row
    /// (`None` for unknown/already-used tokens). Scaffold default refuses
    /// loudly; RedbStore overrides.
    fn redeem_email_token(&self, _token: &str) -> Result<Option<EmailToken>> {
        Err(Error::Internal("redeem_email_token is not implemented".into()))
    }

    /// Set a local user's password hash in ONE transaction (D12
    /// password-reset redemption). `NotFound` when no local user exists for
    /// `account_id`. Scaffold default refuses loudly; RedbStore overrides.
    fn set_user_password(&self, _account_id: Id, _password_hash: String) -> Result<LocalUser> {
        Err(Error::Internal("set_user_password is not implemented".into()))
    }
}

/// OAuth applications, authorization codes and access tokens.
pub trait OAuthStore: Send + Sync {
    fn register_app(&self, input: NewApp) -> Result<OAuthApplication>;
    fn app_by_client_id(&self, client_id: &str) -> Result<Option<OAuthApplication>>;
    /// Resolve an application by its numeric id (used to attach the owning app
    /// to an access token).
    fn app_by_id(&self, id: Id) -> Result<Option<OAuthApplication>>;
    fn issue_access_token(&self, input: NewToken) -> Result<AccessToken>;
    fn token_by_plaintext(&self, plaintext: &str) -> Result<Option<AccessToken>>;
    fn revoke_token(&self, plaintext: &str) -> Result<bool>;
    /// D24: revoke every live token whose user is `account_id` (suspension /
    /// disablement kill), returning the revoked token ids so the adapter can
    /// close their open streams (`StreamEvent::TokenRevoked` per id, after
    /// commit). The scan is bounded (see the `redb` implementation's cap);
    /// ids are returned newest-first.
    fn revoke_account_tokens(&self, account_id: Id) -> Result<Vec<Id>>;

    fn create_authorization_code(&self, input: NewAuthCode) -> Result<AuthorizationCode>;
    fn authorization_code_by_plaintext(&self, plaintext: &str) -> Result<Option<AuthorizationCode>>;
    /// Atomically mark the code used and issue its token. Returns an error if
    /// the code is missing, already used, or expired — so a replay can never
    /// mint a second token.
    fn redeem_authorization_code(
        &self,
        plaintext: &str,
        token: NewToken,
        now_ms: u64,
    ) -> Result<AccessToken>;
}

/// Server-side login sessions for the OAuth approval flow.
pub trait SessionStore: Send + Sync {
    fn create_session(&self, input: NewSession) -> Result<LoginSession>;
    fn session_by_plaintext(&self, plaintext: &str) -> Result<Option<LoginSession>>;
    fn revoke_session(&self, plaintext: &str) -> Result<bool>;
}

/// Outbound mail (verification messages). An adapter/job boundary: mail is
/// never sent inside a request transaction.
pub trait MailSender: Send + Sync {
    /// Send an email-confirmation message for a newly registered account.
    fn send_confirmation(&self, to: &str, account_id: Id, token: &str) -> Result<()>;
    /// Send a password-reset message (resend endpoint job).
    fn send_password_reset(&self, to: &str, account_id: Id, token: &str) -> Result<()>;
}

/// Blob/object storage for media bytes and custom emoji images.
///
/// File storage cannot participate in a `redb` transaction: bytes are staged
/// *before* the metadata row commits, and a failed commit leaves only a
/// bounded orphan for cleanup (`birchtree.md` §5).
pub trait BlobStore: Send + Sync {
    /// Stage `bytes` under `key` (creates parents; overwrites).
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()>;
    /// Read a blob (`None` when missing).
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    /// Delete a blob (idempotent).
    fn delete(&self, key: &str) -> Result<()>;
    /// Whether a blob exists.
    fn exists(&self, key: &str) -> Result<bool>;

    /// Filesystem root this handle writes under, when the handle is backed
    /// by the filesystem (storage overhaul, `docs/database.md` Agent C).
    /// The fetch path uses it to stage response bodies to `tmp/{rand}.part`
    /// under the byte cap and atomically rename onto the key. `None` keeps
    /// the legacy buffered path (in-memory test doubles).
    fn root_path(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Publish an already-staged tmp file onto `key` (atomic rename; storage
    /// overhaul, Agent C). The default refuses; `FsBlobStore` resolves the
    /// key under its root and renames.
    fn publish_tmp(&self, _tmp: &std::path::Path, _key: &str) -> Result<()> {
        Err(Error::Internal("publish_tmp is not implemented".into()))
    }

    /// Stream `bytes` under `key` without buffering the whole body: the
    /// implementation stages to `tmp/{rand}.part` under `max_bytes`
    /// (`media::fetch_stream::stage_slices_capped` shape) and atomically
    /// renames on success. Default impl buffers (back-compat for test doubles);
    /// `FsBlobStore` overrides with tmp+rename.
    fn put_streamed(&self, key: &str, bytes: &[u8], max_bytes: u64) -> Result<()> {
        if bytes.len() as u64 > max_bytes {
            return Err(Error::InvalidInput(format!("stream body exceeds {max_bytes} bytes")));
        }
        self.put(key, bytes)
    }
}

/// Which byte pool a blob handle addresses (`docs/database.md` Agent A).
/// Durable = local uploads, never evicted; cache = remote fetches, evictable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobPool {
    Durable,
    Cache,
}

/// Access to both blob pools. The durable handle keeps the pre-overhaul
/// `<data_dir>/media` root (lazy-move readable); the cache handle owns the
/// fresh `<data_dir>/media_cache` root. Agent B's fetch path takes `Cache`.
pub trait BlobStores: Send + Sync {
    fn pool(&self, pool: BlobPool) -> std::sync::Arc<dyn BlobStore>;
}

/// Remote-cache metadata + accounting (`docs/database.md` Step 0).
/// Payloads < 64 KiB; evict batches bounded (<= 500 rows).
pub trait CacheStore: Send + Sync {
    fn cache_get(&self, source_url: &str, variant: &str)
        -> Result<Option<crate::media::remote_cache::RemoteCacheEntry>>;
    fn cache_put_metadata(&self, entry: crate::media::remote_cache::RemoteCacheEntry) -> Result<()>;
    fn cache_mark_stored(
        &self, source_url: &str, variant: &str, blob_key: &str,
        size: u64, etag: Option<String>, now_ms: u64,
    ) -> Result<()>;
    /// Drop the byte mapping, keep the metadata shell; returns the blob key
    /// the caller must delete. `None` when already shell-only / missing.
    fn cache_mark_evicted(&self, source_url: &str, variant: &str) -> Result<Option<String>>;
    fn cache_touch(&self, source_url: &str, variant: &str, now_ms: u64) -> Result<()>;
    fn cache_accounting(&self) -> Result<crate::media::remote_cache::CacheUsage>;
    /// Bounded victim batch (<= 500): expired-then-LRU within `need_class`,
    /// then other classes, never avatar unless need is avatar. One txn for
    /// mapping+accounting; caller deletes the returned keys (own keys only).
    fn cache_evict_batch(
        &self, need: crate::media::remote_cache::MediaClass,
        bytes_to_free: u64, now_ms: u64, cap: usize,
    ) -> Result<Vec<String>>;
}

/// Durable job queue.
pub trait JobQueue: Send + Sync {
    fn enqueue(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
        now_ms: u64,
    ) -> Result<Job>;

    /// Enqueue a job that becomes claimable at `due_at_ms` (scheduled-status
    /// publication, delayed retries).
    fn enqueue_at(
        &self,
        kind: JobKind,
        payload: serde_json::Value,
        due_at_ms: u64,
        now_ms: u64,
    ) -> Result<Job>;
    /// Claim up to `limit` due jobs under a lease.
    fn claim_due(&self, now_ms: u64, limit: usize) -> Result<Vec<Job>>;
    /// Delete up to `limit` `Succeeded` jobs whose `due_at_ms` is older than
    /// `older_than_ms`, returning how many rows were removed. Only
    /// `Succeeded` rows are eligible: `Pending`/`Leased` rows are live work
    /// and `Quarantined` rows are the operator inspection surface, so both
    /// are never touched. `complete()` keeps the row `Succeeded` (callers
    /// prune separately through this method).
    fn prune_succeeded(&self, older_than_ms: u64, limit: usize) -> Result<u64>;
    fn complete(&self, id: Id) -> Result<()>;
    /// Record a failed attempt; schedules a retry or quarantines.
    fn retry(&self, id: Id, error: &str, now_ms: u64) -> Result<Job>;
    fn quarantine(&self, id: Id, error: &str) -> Result<()>;
    fn get(&self, id: Id) -> Result<Option<Job>>;
}

/// Readiness probe used by the runtime.
pub trait ReadyCheck: Send + Sync {
    /// True when the store is open and can serve a read.
    fn is_ready(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Phase 3 ports: statuses/interactions, relationships, polls, quotes,
// notifications, media metadata.
//
// Invariants every mutating method upholds (see `birchtree.md` §5):
// one `redb` transaction commits the record mutation, affected indexes,
// counters, and any bounded durable jobs atomically; network-facing work
// (notification fan-out, federation) runs after commit as jobs.
// ---------------------------------------------------------------------------

/// A poll to embed in a created or edited status.
#[derive(Debug, Clone)]
pub struct NewPollSpec {
    pub multiple: bool,
    pub hide_totals: bool,
    /// Absolute expiry; local polls always have one.
    pub expires_at_ms: u64,
    /// 2–4 option titles, validated by the use case.
    pub options: Vec<String>,
}

/// A quote to attach at status creation (local quoted statuses in Phase 3).
#[derive(Debug, Clone)]
pub struct NewQuoteSpec {
    pub quoted_status_id: Id,
    /// Author of the quoted status (for the silent mention + counter update).
    pub quoted_account_id: Id,
    /// The quoting status's `quote_approval_policy`.
    pub approval_policy: QuotePolicy,
}

/// Input for creating a status in one atomic transaction.
/// The URI and URL of a remote status, as its server published them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatusIdentity {
    pub uri: String,
    pub url: String,
}

#[derive(Debug, Clone, Default)]
pub struct NewStatus {
    /// Set for a status received from a remote server: its own URI and URL are
    /// stored as given, and no local URI is minted. `None` for a local status.
    pub remote: Option<RemoteStatusIdentity>,
    pub account_id: Id,
    /// Canonical ActivityPub object URI base; the store appends `/{id}`
    /// (computed by the use case from the public URL + username).
    pub uri_base: String,
    /// Local web URL base; the store appends `/{id}`.
    pub url_base: String,
    pub source_text: String,
    pub content_html: String,
    pub spoiler_text: Option<String>,
    pub sensitive: bool,
    pub visibility: Visibility,
    pub language: Option<String>,
    pub in_reply_to_id: Option<Id>,
    pub in_reply_to_account_id: Option<Id>,
    /// Attached media ids, in the author's order.
    pub media_ids: Vec<Id>,
    /// Core extension CORE-P: attached place id (`None` = no place).
    pub place_id: Option<Id>,
    /// Resolved mention account ids, in mention order (serialized).
    pub mentions: Vec<Id>,
    /// Resolved silent mentions (quote access grants): visibility only.
    pub silent_mentions: Vec<Id>,
    /// Normalized hashtags without the `#` prefix (D25).
    pub tags: Vec<String>,
    pub poll: Option<NewPollSpec>,
    pub quote: Option<NewQuoteSpec>,
    pub application: Option<StatusApplication>,
    /// Mastodon `Idempotency-Key` header value, when present.
    pub idempotency_key: Option<String>,
}

/// Result of a status creation; a replayed idempotent retry returns the
/// originally created status with `replayed = true`.
#[derive(Debug, Clone)]
pub struct CreatedStatus {
    pub status: Status,
    pub replayed: bool,
}

/// A deletion's pre-deletion snapshots (see [`StatusStore::delete_status`]).
#[derive(Debug, Clone)]
pub struct DeletedStatus {
    pub status: Status,
    /// Reblog wrappers tombstoned by the same transaction.
    pub reblogs: Vec<Status>,
}

/// Field-level edit patch (`PUT /api/v1/statuses/:id`). `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct StatusUpdate {
    pub source_text: Option<String>,
    pub content_html: Option<String>,
    /// `Some(None)` clears a spoiler; `None` leaves it unchanged.
    pub spoiler_text: Option<Option<String>>,
    pub sensitive: Option<bool>,
    pub language: Option<String>,
    pub media_ids: Option<Vec<Id>>,
    /// Core extension CORE-P: replace the attached place (`None` leaves it
    /// unchanged; `Some(None)` clears; `Some(Some(id))` sets).
    pub place_id: Option<Option<Id>>,
    /// Replace poll options/multiple (resets all votes, per Mastodon).
    pub poll: Option<NewPollSpec>,
    /// Extend poll expiry without resetting votes.
    pub poll_expires_in_ms: Option<u64>,
    pub quote_approval_policy: Option<QuotePolicy>,
    /// Replace the stored hashtags (already normalized by the caller) and
    /// move the hashtag-index keys in the same transaction. `None` leaves
    /// tags and index untouched.
    pub tags: Option<Vec<String>>,
}

/// Per-viewer status flags for DTO packing.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusViewFlags {
    pub favourited: bool,
    pub bookmarked: bool,
    /// The viewer has reblogged the (proper) status.
    pub reblogged: bool,
    /// The viewer has pinned the status.
    pub pinned: bool,
    /// The viewer mutes the status's conversation (thread root).
    pub muted: bool,
}

/// Cursor-bounded query for an account's statuses.
#[derive(Debug, Clone, Copy)]
pub struct AccountStatusesQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
    /// Return the account's pinned statuses in pin order instead.
    pub pinned_only: bool,
    pub exclude_replies: bool,
    pub exclude_reblogs: bool,
}

/// Statuses, replies, reblogs, edits, pins and status interactions.
pub trait StatusStore: Send + Sync {
    /// Create a status with its poll/quote, indexes, counters, idempotency
    /// record and follow-up jobs in ONE transaction.
    fn create_status(&self, input: NewStatus, now_ms: u64) -> Result<CreatedStatus>;

    fn status_by_id(&self, id: Id) -> Result<Option<Status>>;

    /// Non-deleted statuses among `ids`, ordered by id descending.
    fn statuses_by_ids(&self, ids: &[Id]) -> Result<Vec<Status>>;

    /// Apply an edit: snapshot the pre-edit version (once), patch fields, bump
    /// `edited_at_ms`, keep counters — ONE transaction. Returns `Ok(None)`
    /// when the patch changed nothing (no snapshot, no `edited_at` bump).
    fn update_status(
        &self,
        id: Id,
        account_id: Id,
        update: StatusUpdate,
        now_ms: u64,
    ) -> Result<Option<Status>>;

    /// Tombstone a status in ONE transaction (tombstone + dependent reblogs +
    /// pin removal + parent/author counter fixes + follow-up job). Returns the
    /// pre-deletion snapshots: the status and every reblog wrapper the cascade
    /// tombstoned (streaming needs them to send `delete` events). `NotFound`
    /// when missing, deleted, or not owned.
    fn delete_status(&self, id: Id, account_id: Id, now_ms: u64) -> Result<DeletedStatus>;

    /// The live reblog wrapper `account_id` created of `status_id`, if any.
    fn own_reblog(&self, account_id: Id, status_id: Id) -> Result<Option<Status>>;

    /// Direct reply children of `parent_id`, newest first, cursor-bounded.
    fn status_children(
        &self,
        parent_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Status>>;

    /// Edit-history snapshots, oldest first (empty when never edited).
    fn status_versions(&self, status_id: Id) -> Result<Vec<StatusVersion>>;

    /// Statuses authored by `account_id` (non-deleted), id-descending with
    /// cursors; `pinned_only` returns the account's pins in pin order.
    fn statuses_by_account(
        &self,
        account_id: Id,
        query: AccountStatusesQuery,
    ) -> Result<Vec<Status>>;

    /// Viewer flags for a batch of statuses (no N+1).
    fn viewer_status_flags(
        &self,
        viewer_id: Id,
        status_ids: &[Id],
    ) -> Result<HashMap<Id, StatusViewFlags>>;

    // -- Interactions (each ONE transaction: row + counters + follow-up job) --

    /// Add a favourite (idempotent: already-favourited returns the status
    /// unchanged). NotFound when the status is missing or not visible.
    fn favourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    /// Remove a favourite (no-op when absent), returning the status.
    fn unfavourite_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    fn bookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    fn unbookmark_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Create (or return the existing) reblog by `account_id` of `status_id`.
    fn reblog_status(
        &self,
        account_id: Id,
        status_id: Id,
        visibility: Visibility,
        now_ms: u64,
    ) -> Result<Status>;
    /// Delete the viewer's reblog of `status_id`, returning the original
    /// (counters decremented; no-op when the viewer never reblogged).
    fn unreblog_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Pin own status (limit enforced by the use case), returning the status.
    fn pin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;
    /// Unpin own status (no-op when not pinned), returning the status.
    fn unpin_status(&self, account_id: Id, status_id: Id, now_ms: u64) -> Result<Status>;

    /// Update only the quote approval policy — no edit snapshot and no
    /// `edited_at` bump (mirrors Mastodon's interaction-policy endpoint).
    /// NotFound when missing/deleted; the caller enforces ownership.
    fn set_quote_policy(&self, status_id: Id, policy: QuotePolicy) -> Result<Status>;

    /// Accounts that favourited `status_id`, newest first, as
    /// `(account_id, favourite_id)` pairs; the interaction id is the cursor.
    fn favourited_by(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>>;
    /// Accounts that reblogged `status_id`, newest first, as
    /// `(account_id, reblog_status_id)` pairs; the reblog id is the cursor.
    fn reblogged_by(&self, status_id: Id, max_id: Option<Id>, limit: usize)
    -> Result<Vec<(Id, Id)>>;

    /// The viewer's favourites, newest first (cursor = favourite row id).
    fn favourited_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Favourite>>;
    /// The viewer's bookmarks, newest first (cursor = bookmark row id).
    fn bookmarked_statuses(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Bookmark>>;

    /// One bounded page of the tag-normalization backfill (D25): newest
    /// first, `(status id, stored tags, source text)`. `after` is the
    /// exclusive resume cursor (`None` starts at the newest row).
    fn backfill_tag_batch(
        &self,
        after: Option<Id>,
        cap: usize,
    ) -> Result<Vec<(Id, Vec<String>, String)>>;
    /// Rewrite a status's stored tag list and move its hashtag-index keys in
    /// ONE transaction (D25 backfill writer).
    fn rewrite_status_tags(&self, status_id: Id, new_tags: &[String]) -> Result<()>;
}

/// Follows, follow requests, blocks, domain blocks, mutes, notes, endorsements
/// and the relationship aggregate.
#[derive(Debug, Clone)]
pub struct FollowOptions {
    pub show_reblogs: bool,
    pub notify: bool,
    /// D23 per-follow language filter (normalized BCP-47 codes).
    pub languages: Option<Vec<String>>,
}

/// Outcome of a follow attempt (target locked => request).
#[derive(Debug, Clone)]
pub enum FollowOutcome {
    Following(Follow),
    Requested(Follow),
}

/// Everything `REST::RelationshipSerializer` needs for one (viewer, target) pair.
#[derive(Debug, Clone)]
pub struct Relationship {
    pub target_id: Id,
    pub following: bool,
    pub show_reblogs: bool,
    pub notifying: bool,
    /// D23 per-follow language filter (`None` = no preference).
    pub languages: Option<Vec<String>>,
    pub followed_by: bool,
    pub blocking: bool,
    pub blocked_by: bool,
    pub muting: bool,
    pub muting_notifications: bool,
    pub muting_expires_at_ms: Option<u64>,
    pub requested: bool,
    pub requested_by: bool,
    pub domain_blocking: bool,
    pub endorsed: bool,
    /// Private note the viewer keeps on the target ("" when none).
    pub note: String,
}

/// Pairwise read-authorization facts used by the visibility policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct PairState {
    /// `a` blocks `b`.
    pub blocking: bool,
    /// `b` blocks `a`.
    pub blocked_by: bool,
    /// `a` follows `b` (active).
    pub following: bool,
    /// `b` follows `a` (active).
    pub followed_by: bool,
    /// `a` mutes `b` (unexpired).
    pub muting: bool,
}

pub trait RelationshipStore: Send + Sync {
    /// Create (or update) a follow/follow-request row atomically with both
    /// sides' counters and the follow(-request) notification job.
    fn follow(
        &self,
        follower_id: Id,
        followed_id: Id,
        opts: FollowOptions,
        request: bool,
        now_ms: u64,
    ) -> Result<FollowOutcome>;

    /// Remove an outgoing follow or cancel a pending request.
    fn unfollow(&self, follower_id: Id, followed_id: Id, now_ms: u64) -> Result<Option<Follow>>;

    /// Remove an inbound follow (the caller's own follower).
    fn remove_follower(&self, account_id: Id, follower_id: Id, now_ms: u64) -> Result<Option<Follow>>;

    /// Pending inbound follow requests, newest first (cursor = follower id;
    /// upstream paginates `FollowRequest` row ids, which are client-opaque
    /// through `Link` — registered as a divergence, not re-keyed).
    fn follow_requests(
        &self,
        target_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Follow>>;

    /// Convert a pending request to an active follow (atomic + notification
    /// job); NotFound when no request exists.
    fn authorize_follow_request(&self, target_id: Id, follower_id: Id, now_ms: u64)
    -> Result<Follow>;

    /// Drop a pending request (atomic).
    fn reject_follow_request(
        &self,
        target_id: Id,
        follower_id: Id,
        now_ms: u64,
    ) -> Result<Option<Follow>>;

    /// Block: mutual unfollow, reject pending inbound request, drop
    /// endorsements between the pair, then insert the block — ONE transaction.
    /// Returns `None` when the block already existed.
    fn block(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Block>>;
    fn unblock(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Block>>;

    /// Mute with a duration (0 = permanent) and notification-hiding flag.
    fn mute(
        &self,
        account_id: Id,
        target_id: Id,
        duration_seconds: u64,
        hide_notifications: bool,
        now_ms: u64,
    ) -> Result<Mute>;
    fn unmute(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Option<Mute>>;

    /// Set (or with `comment` empty, delete) the viewer's private note.
    fn set_account_note(
        &self,
        viewer_id: Id,
        target_id: Id,
        comment: &str,
        now_ms: u64,
    ) -> Result<Option<AccountNote>>;

    fn domain_block(&self, account_id: Id, domain: &str, now_ms: u64) -> Result<DomainBlock>;
    fn unblock_domain(&self, account_id: Id, domain: &str) -> Result<Option<DomainBlock>>;
    /// Blocked domains, newest first. `max_id` and `since_id` are exclusive
    /// numeric row-id cursors (Mastodon's `paginate_by_max_id`).
    fn domain_blocks(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<DomainBlock>>;

    /// Endorse (pin) an account the caller follows; NotFound otherwise.
    fn endorse(&self, account_id: Id, target_id: Id, now_ms: u64) -> Result<Endorsement>;
    fn unendorse(&self, account_id: Id, target_id: Id) -> Result<Option<Endorsement>>;
    /// Endorsed account ids, newest first (cursor = endorsed account id).
    fn endorsements(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;

    /// Relationship aggregates for `(viewer, (target_id, target_domain))` pairs.
    fn relationships(
        &self,
        viewer_id: Id,
        targets: &[(Id, String)],
        now_ms: u64,
    ) -> Result<Vec<Relationship>>;

    /// Pairwise authorization facts (one lookup per pair member).
    fn pair_state(&self, a: Id, b: Id, now_ms: u64) -> Result<PairState>;

    /// The follow row of `follower_id` -> `followed_id`, if any.
    fn follow_of(&self, follower_id: Id, followed_id: Id) -> Result<Option<Follow>>;

    /// Followers of / following by `account_id`, newest first (cursor = account id).
    fn followers(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;
    fn following(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;

    /// Account counters (followers/following/statuses/last status/requests).
    fn account_stats(&self, account_id: Id) -> Result<AccountStats>;

    /// Account counters for many accounts in ONE transaction (page loader).
    fn account_stats_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, AccountStats>>;

    /// Batched pairwise authorization facts in ONE transaction.
    ///
    /// For each `other` the entry is `(incoming, outgoing)` with
    /// `incoming = pair_state(other, viewer)` (does `other` block/follow/mute
    /// the viewer?) and `outgoing = pair_state(viewer, other)` (does the
    /// viewer block/follow/mute `other`?). An entry exists for every `other`
    /// (no relationship rows yield the default pair state), so callers can
    /// index without existence checks.
    fn batch_pair_states(
        &self,
        viewer: Id,
        others: &[Id],
        now_ms: u64,
    ) -> Result<HashMap<Id, (PairState, PairState)>>;

    /// Blocked account ids, newest first (cursor = blocked account id).
    fn blocks_of(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;
    /// Mute rows, newest first (cursor = muted account id).
    fn mutes_of(&self, account_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Mute>>;
}

/// Dismissed follow suggestions (fix round P1/S1; upstream
/// `follow_recommendation_mutes`). One row per (viewer, target) dismissal;
/// the ranking engine reads the set back into its exclusion list.
pub trait SuggestionStore: Send + Sync {
    /// Record a dismissal; returns `true` when the row is new.
    fn dismiss_suggestion(&self, viewer_id: Id, target_id: Id, now_ms: u64) -> Result<bool>;
    /// Dismissed target ids for `viewer_id`, bounded by `cap`.
    fn dismissed_suggestions(&self, viewer_id: Id, cap: usize) -> Result<Vec<Id>>;
}

/// A tag follow (`TagFollow`): unique per (account, normalized tag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagFollow {
    pub id: Id,
    pub account_id: Id,
    pub tag: String,
    pub created_at_ms: u64,
}

/// A featured tag on an account's profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturedTag {
    pub id: Id,
    pub account_id: Id,
    pub tag: String,
    pub created_at_ms: u64,
}

/// A session-bound first-party web push subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebPushSubscription {
    pub id: Id,
    pub user_id: Id,
    pub endpoint: String,
    pub key_auth: String,
    pub key_p256dh: String,
    pub standard: bool,
    pub data: String,
    pub unsubscribe_token: String,
    pub created_at_ms: u64,
}

/// Phase 7 web/tag rows (Subagent B): settings sync, web push
/// subscriptions, tag follows and featured tags. One `redb` transaction
/// per mutation; reads are bounded scans.
pub trait WebStore: Send + Sync {
    fn web_settings(&self, user_id: Id) -> Result<Option<String>>;
    fn set_web_settings(&self, user_id: Id, data: &str) -> Result<String>;
    fn web_push_subscriptions(&self, user_id: Id) -> Result<Vec<WebPushSubscription>>;
    fn create_web_push_subscription(
        &self,
        user_id: Id,
        endpoint: &str,
        key_p256dh: &str,
        key_auth: &str,
        standard: bool,
        data: &str,
        now_ms: u64,
    ) -> Result<WebPushSubscription>;
    fn update_web_push_subscription(
        &self,
        user_id: Id,
        id: Id,
        data: &str,
        now_ms: u64,
    ) -> Result<Option<WebPushSubscription>>;
    fn destroy_web_push_subscription(
        &self,
        user_id: Id,
        id: Id,
    ) -> Result<Option<WebPushSubscription>>;
    fn destroy_web_push_subscription_by_token(&self, token: &str)
        -> Result<Option<WebPushSubscription>>;
    fn follow_tag(&self, account_id: Id, tag: &str, now_ms: u64) -> Result<TagFollow>;
    fn unfollow_tag(&self, account_id: Id, tag: &str) -> Result<Option<TagFollow>>;
    fn followed_tags(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<TagFollow>>;
    fn tag_follow(&self, account_id: Id, tag: &str) -> Result<Option<TagFollow>>;
    fn feature_tag(&self, account_id: Id, tag: &str, now_ms: u64) -> Result<FeaturedTag>;
    fn unfeature_tag(&self, account_id: Id, id: Id) -> Result<Option<FeaturedTag>>;
    fn featured_tags_of(&self, account_id: Id) -> Result<Vec<FeaturedTag>>;
    fn featured_tag(&self, account_id: Id, tag: &str) -> Result<Option<FeaturedTag>>;
}

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
    fn email_domain_block_for_domain(
        &self,
        domain: &str,
    ) -> Result<Option<AdminEmailDomainBlock>>;    fn admin_delete_email_domain_block(&self, id: Id) -> Result<Option<AdminEmailDomainBlock>>;

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
    fn admin_create_canonical_email_block(&self, email_hash: &str, now_ms: u64)
    -> Result<AdminCanonicalEmailBlock>;
    fn admin_canonical_email_block_by_id(&self, id: Id) -> Result<Option<AdminCanonicalEmailBlock>>;
    fn admin_canonical_email_blocks_page(
        &self,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<AdminCanonicalEmailBlock>>;
    /// Rows whose hash matches (the `test` endpoint), newest first.
    fn admin_canonical_blocks_for_hash(&self, email_hash: &str) -> Result<Vec<AdminCanonicalEmailBlock>>;
    fn admin_delete_canonical_email_block(&self, id: Id) -> Result<Option<AdminCanonicalEmailBlock>>;

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
        Err(Error::Internal("admin_set_rule_translation is not implemented".into()))
    }
    /// Delete one `(rule, locale)` translation; `true` when a row existed.
    fn admin_delete_rule_translation(&self, _rule_id: Id, _locale: &str) -> Result<bool> {
        Err(Error::Internal(
            "admin_delete_rule_translation is not implemented".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Phase 7.3 ports (Subagent D): trends, annual reports.
//
// Trend approvals share the audit record shape with `AdminInfraStore`
// (`AdminAuditRecord`, one row per admin write), but live in their own
// tables: trend candidates are read-time derivations from real usage
// (hashtag index, recent public statuses, URLs in status text) joined to
// small persisted approval-flag rows. `AdminTag.trendable` (subagent C's
// admin-tags surface) and `TrendTagFlag.allowed` (here) model the same
// upstream `Tag#trendable` column; synthesis unifies them into one store.
// Until then both serialize the same `trendable` wire key.
// ---------------------------------------------------------------------------

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

/// Annual-report storage (generation runs as a durable job).
pub trait AnnualReportStore: Send + Sync {
    /// This account's report for `year`, if generated.
    fn annual_report(&self, account_id: Id, year: i32) -> Result<Option<AnnualReport>>;
    /// This account's unread reports, newest year first (the index).
    fn pending_annual_reports(&self, account_id: Id) -> Result<Vec<AnnualReport>>;
    /// Insert a generated report; idempotent on (account, year) — returns the
    /// existing row when one is already stored, so a retried generation job
    /// never duplicates it.
    fn insert_annual_report(&self, input: NewAnnualReport, now_ms: u64) -> Result<AnnualReport>;
    /// Mark the account's report for `year` read; `None` when missing.
    fn mark_annual_report_viewed(
        &self,
        account_id: Id,
        year: i32,
        now_ms: u64,
    ) -> Result<Option<AnnualReport>>;
    /// True when a `GenerateAnnualReport` job for (account, year) is still
    /// pending or leased (the `generating` state).
    fn has_pending_generate_report(&self, account_id: Id, year: i32) -> Result<bool>;
    /// The account's non-deleted statuses created in `year`, oldest first,
    /// bounded by `cap` (generation input; ids are time-ordered so the scan
    /// stops at the year boundary).
    fn account_statuses_in_year(
        &self,
        account_id: Id,
        year: i32,
        cap: usize,
    ) -> Result<Vec<Status>>;
}

/// Why a poll vote was rejected (mapped to Mastodon 422 messages by the use
/// case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteRejection {
    Expired,
    InvalidChoice,
    SelfVote,
    AlreadyVoted,
}

/// Polls and ballots.
pub trait PollStore: Send + Sync {
    fn poll_by_id(&self, id: Id) -> Result<Option<Poll>>;
    fn poll_by_status(&self, status_id: Id) -> Result<Option<Poll>>;

    /// The viewer's ballot on a poll, if any (for `voted`/`own_votes`).
    fn ballot(&self, poll_id: Id, account_id: Id) -> Result<Option<Vec<u64>>>;

    /// Polls by id in ONE transaction (page loader; missing ids omitted).
    fn polls_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Poll>>;

    /// The viewer's ballots for several polls in ONE transaction (page
    /// loader); polls without a ballot are omitted.
    fn ballots(&self, poll_ids: &[Id], account_id: Id) -> Result<HashMap<Id, Vec<u64>>>;

    /// Cast a ballot in ONE transaction: tallies, ballot row and the
    /// vote-notification job commit together. Mastodon semantics: any second
    /// vote on a single-choice poll is rejected; on multiple-choice polls only
    /// already-cast choices are rejected (new choices are added). Validation
    /// happens inside the transaction (expiry/choice/self/dup).
    fn vote_poll(
        &self,
        poll_id: Id,
        account_id: Id,
        choices: &[u64],
        now_ms: u64,
    ) -> std::result::Result<Poll, VoteRejection>;

    /// Sticky-mark a poll expired (PollExpired job handler; idempotent).
    fn mark_poll_expired(&self, poll_id: Id, now_ms: u64) -> Result<Option<Poll>>;

    /// Overwrite a poll's tallies from its origin in ONE transaction: option
    /// counts matched by title (unknown names ignored), the voters count, the
    /// expiry when given, and the sticky expired flag. `None` when the poll
    /// row is gone. Local ballots are never rewritten by this path.
    fn sync_remote_poll(
        &self,
        poll_id: Id,
        counts: &[(String, u64)],
        voters_count: u64,
        expires_at_ms: Option<u64>,
        expired: bool,
        now_ms: u64,
    ) -> Result<Option<Poll>>;
}

/// Quotes (local quote state machine; federated quote requests are Phase 6).
pub trait QuoteStore: Send + Sync {
    /// The quote authored by a quoting status, if any.
    fn quote_by_quoting_status(&self, quoting_status_id: Id) -> Result<Option<QuoteRecord>>;

    /// Quote records keyed by quoting status id in ONE transaction (page
    /// loader; statuses without a quote are omitted).
    fn quotes_by_quoting_ids(&self, ids: &[Id]) -> Result<HashMap<Id, QuoteRecord>>;

    /// Accepted quotes of `quoted_status_id` with their quoting statuses,
    /// newest first (cursor = quote id, upstream `paginate_by_max_id`).
    fn quotes_of_status(
        &self,
        quoted_status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(QuoteRecord, Status)>>;

    /// Revoke an accepted quote (quoted author only): state -> `revoked`,
    /// quoted counter decrement — ONE transaction. NotFound when no such
    /// quote; Forbidden handled by the use case before the call.
    fn revoke_quote(
        &self,
        quoted_status_id: Id,
        quoting_status_id: Id,
        now_ms: u64,
    ) -> Result<QuoteRecord>;
}

/// Notification records (fan-out happens after commit, via jobs).
#[derive(Debug, Clone)]
pub struct NewNotification {
    pub recipient_id: Id,
    pub r#type: NotificationType,
    pub actor_id: Option<Id>,
    pub status_id: Option<Id>,
    pub created_at_ms: u64,
    /// Upstream `notifications.filtered`: hidden from the index until the
    /// recipient accepts the matching notification request. Defaults to
    /// `false` at construction sites that predate the request pipeline.
    pub filtered: bool,
}

pub trait NotificationStore: Send + Sync {
    /// Insert a notification unless its dedup key `(recipient, type, actor,
    /// status)` already exists; returns `None` on a dedup hit. Used by the
    /// `FanOutNotification` job handler, so job retries are harmless.
    fn create_notification(&self, input: NewNotification) -> Result<Option<Notification>>;

    /// One page for the recipient, newest first; cursors exclusive
    /// (`id < max_id`, `id > since_id`/`id > min_id`), bounded scan.
    /// Filtered rows are excluded (upstream's default notification scope):
    /// they surface only through notification requests until accepted.
    fn notifications_page(
        &self,
        recipient_id: Id,
        limit: usize,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Result<Vec<Notification>>;

    /// Filtered rows for `include_filtered=true`, newest first with the same
    /// exclusive cursors (upstream `browserable` serves them when the flag is
    /// set). Bounded scan like `notifications_page`.
    fn filtered_notifications_page(
        &self,
        recipient_id: Id,
        limit: usize,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
    ) -> Result<Vec<Notification>>;

    fn notification_by_id(&self, id: Id) -> Result<Option<Notification>>;

    /// Delete one of the recipient's notifications; `false` when missing or
    /// belonging to someone else.
    fn delete_notification(&self, recipient_id: Id, id: Id) -> Result<bool>;

    /// Delete every notification of the recipient.
    fn clear_notifications(&self, recipient_id: Id) -> Result<()>;

    /// Count notifications with `id > floor`, capped by `limit` (the
    /// marker-based `unread_count`). Filtered rows are excluded, like the
    /// index page.
    fn count_notifications_after(
        &self,
        recipient_id: Id,
        floor: Option<Id>,
        limit: u64,
    ) -> Result<u64>;

    // -- Notification-request pipeline (round 3, area C) -------------------
    // Upstream: `NotifyService` filters into `notification_requests`;
    // `AcceptNotificationRequestService` unhides + emits
    // `notifications_merged`; dismiss deletes the filtered rows.

    /// The recipient's policy, or the upstream-column defaults when no row
    /// was ever stored (`find_or_initialize_by`).
    fn notification_policy(&self, account_id: Id) -> Result<NotificationPolicy>;

    /// Replace the recipient's policy row in ONE transaction.
    fn set_notification_policy(&self, policy: &NotificationPolicy) -> Result<NotificationPolicy>;

    /// Create or refresh the `(recipient, actor)` request row (upstream
    /// `find_or_initialize_by` + `last_status_id` bump): the newest filtered
    /// mention/quote status wins, `updated_at` moves, the row id is stable.
    /// `None` status ids never create a row (upstream only files requests
    /// for mention/quote notifications, which always carry a status).
    fn upsert_notification_request(
        &self,
        recipient_id: Id,
        from_account_id: Id,
        last_status_id: Id,
        now_ms: u64,
    ) -> Result<NotificationRequest>;

    /// Every request row of the recipient, newest first, bounded scan.
    fn notification_requests(&self, recipient_id: Id) -> Result<Vec<NotificationRequest>>;

    /// One request row of the recipient (`None` when missing or foreign).
    fn notification_request(
        &self,
        recipient_id: Id,
        id: Id,
    ) -> Result<Option<NotificationRequest>>;

    /// Delete one request row of the recipient; `false` when missing or
    /// foreign (accept/dismiss destroy the row after doing their work).
    fn delete_notification_request(&self, recipient_id: Id, id: Id) -> Result<bool>;

    /// How many *filtered* notification rows the recipient still holds from
    /// this actor (the request DTO `notifications_count`, computed live so
    /// it can never drift from the rows).
    fn count_filtered_notifications(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<u64>;

    /// Unhide every filtered notification from this actor (accept path);
    /// returns how many rows flipped. One transaction.
    fn unfilter_notifications(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<u64>;

    /// Delete every *filtered* notification from this actor (dismiss path);
    /// returns how many rows were removed. Unfiltered rows are kept.
    fn delete_filtered_notifications(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<u64>;

    /// Record that future notifications from this actor bypass the filter
    /// (upstream `NotificationPermission`, created on accept). Idempotent.
    fn grant_notification_permission(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<()>;

    /// Whether such a permission row exists (upstream
    /// `FilterCondition#override_for_sender?`).
    fn has_notification_permission(
        &self,
        recipient_id: Id,
        from_account_id: Id,
    ) -> Result<bool>;
}

/// Media attachment metadata (bytes/lifecycle are Phase 4).
#[derive(Debug, Clone)]
pub struct NewMedia {
    pub account_id: Id,
    /// One of `unknown`/`image`/`gifv`/`video`/`audio`.
    pub r#type: String,
    pub description: String,
    /// Core extension CORE-L: Pixelfed license id (validated before the
    /// store call; `None` = all rights reserved). Already policy-gated by
    /// the caller (`off` stores `None`).
    pub license: Option<u8>,
}

/// An uploaded attachment whose bytes are already staged in the blob store.
///
/// The store commits metadata + the `ProcessMedia` job in ONE transaction; the
/// job finishes processing after commit (`birchtree.md` §5).
#[derive(Debug, Clone)]
pub struct NewMediaUpload {
    pub account_id: Id,
    /// Wire `type`: `image`/`video`/`audio`/`gifv`.
    pub r#type: String,
    pub mime_type: String,
    pub description: String,
    pub file_size: u64,
    /// Blob key of the staged original.
    pub file_key: String,
    /// Cover image blob key (video/audio).
    pub thumbnail_key: Option<String>,
    /// Focus point (`focus` param).
    pub focus: Option<crate::domain::FocusPoint>,
    /// Delayed processing (v2 create for larger formats): the attachment stays
    /// `Pending` until the `ProcessMedia` job completes.
    pub delay: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub bitrate: Option<u64>,
    /// Core extension CORE-L: Pixelfed license id (validated before the
    /// store call; `None` = all rights reserved). Already policy-gated by
    /// the caller (`off` stores `None`).
    pub license: Option<u8>,
}

/// Processing result committed by the `ProcessMedia` job (idempotent).
#[derive(Debug, Clone)]
pub struct MediaProcessingResult {
    /// `Ready` (or `Failed` with `error`).
    pub state: crate::domain::MediaState,
    pub r#type: String,
    pub blurhash: Option<String>,
    /// Blob key of the generated `small` preview.
    pub preview_key: Option<String>,
    pub preview_width: Option<u32>,
    pub preview_height: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub frame_rate: Option<String>,
    pub bitrate: Option<u64>,
    pub error: Option<String>,
}

/// Owner-scoped media patch (`PUT /api/v1/media/:id`).
#[derive(Debug, Clone, Default)]
pub struct MediaPatch {
    pub description: Option<String>,
    /// `Some(None)` clears the focus point; `None` leaves it unchanged.
    pub focus: Option<Option<crate::domain::FocusPoint>>,
    /// Replace the cover image blob key (`None` leaves it unchanged).
    pub thumbnail_key: Option<Option<String>>,
    /// Core extension CORE-L: replace the license (`None` leaves it
    /// unchanged; `Some(None)` clears to all-rights).
    pub license: Option<Option<u8>>,
}

/// Core extension CORE-P: the place registry (`places` / `places_by_name`,
/// schema v22 — renumbered from v18). Places are seeded by tests/operators
/// and remote ingest; there is no public proximity/discover/geo-search
/// surface (stub).
pub trait PlaceStore: Send + Sync {
    /// Validate and upsert a place row + its name-index entry in ONE
    /// transaction, returning the stored row.
    fn upsert_place(&self, place: crate::domain::Place) -> Result<crate::domain::Place>;
    /// Read a place row by id.
    fn place_by_id(&self, id: Id) -> Result<Option<crate::domain::Place>>;
    /// Places by id in ONE transaction (page loader; missing ids omitted).
    fn places_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, crate::domain::Place>>;
    /// Link a validated remote place: a same-name local row wins (returned
    /// as-is, so remote input can neither hijack nor duplicate local
    /// names); otherwise mint a local row in ONE transaction.
    fn link_remote_place(&self, place: &crate::domain::Place) -> Result<crate::domain::Place>;
}

pub trait MediaStore: Send + Sync {
    fn create_media(&self, input: NewMedia) -> Result<MediaAttachment>;
    /// The subset of `ids` owned by `owner_id` and unattached (or already
    /// attached to `status_id`), returned in the requested order.
    fn attachable_media(&self, owner_id: Id, ids: &[Id], status_id: Option<Id>)
    -> Result<Vec<MediaAttachment>>;
    fn media_by_id(&self, id: Id) -> Result<Option<MediaAttachment>>;

    /// Attachments by id in ONE transaction (page loader; missing ids omitted,
    /// duplicates read once).
    fn media_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, MediaAttachment>>;

    /// Insert an uploaded attachment's metadata + `ProcessMedia` job in ONE
    /// transaction.
    fn create_media_upload(&self, input: NewMediaUpload, now_ms: u64) -> Result<MediaAttachment>;

    /// The owner's media row (Mastodon `set_media_attachment`: only the
    /// owner's own, not-yet-attached rows are addressable; others are `None`
    /// → 404).
    fn owner_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>>;

    /// Apply an owner-scoped patch (description/focus/thumbnail).
    fn update_media(
        &self,
        account_id: Id,
        id: Id,
        patch: MediaPatch,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>>;

    /// Delete an owner's unattached attachment (bytes are removed by the
    /// caller after commit). `None` when missing/foreign; the use case
    /// rejects attached rows with the Mastodon in-usage error.
    fn delete_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>>;

    /// Commit a processing result (idempotent; `None` when the row is gone).
    fn mark_media_processed(
        &self,
        id: Id,
        result: MediaProcessingResult,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>>;
}

// ---------------------------------------------------------------------------
// Phase 4 ports: lists, markers, filters, scheduled statuses, custom emoji,
// timeline scans. Same invariants as the Phase 3 ports: one `redb`
// transaction per method, mutating methods commit record + indexes +
// counters + bounded durable jobs atomically.
// ---------------------------------------------------------------------------

pub trait ListStore: Send + Sync {
    /// Create a list for `account_id` (the use case enforces the per-account
    /// limit and title rules).
    fn create_list(
        &self,
        account_id: Id,
        title: &str,
        replies_policy: RepliesPolicy,
        exclusive: bool,
        now_ms: u64,
    ) -> Result<List>;
    fn list_by_id(&self, id: Id) -> Result<Option<List>>;
    /// The owner's lists (index returns all; no pagination in v4.7.3).
    fn lists_by_account(&self, account_id: Id) -> Result<Vec<List>>;
    /// Patch a list the viewer owns; `None` when missing/foreign (404).
    fn update_list(
        &self,
        account_id: Id,
        id: Id,
        title: Option<String>,
        replies_policy: Option<RepliesPolicy>,
        exclusive: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<List>>;
    /// Delete a list the viewer owns (and its memberships); `None` when
    /// missing/foreign.
    fn delete_list(&self, account_id: Id, id: Id) -> Result<bool>;

    /// Add members (must follow, or have a pending request — enforced by the
    /// use case). Idempotent: existing members are skipped; the returned ids
    /// are the newly added ones.
    fn add_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>>;
    /// Remove members (idempotent); returns the removed ids.
    fn remove_list_members(&self, list_id: Id, account_ids: &[Id]) -> Result<Vec<Id>>;
    /// Members of a list, newest first (cursor = member account id).
    fn list_members(&self, list_id: Id, max_id: Option<Id>, limit: usize) -> Result<Vec<Id>>;
    /// All members of a list (bounded), for the list timeline merge.
    fn all_list_members(&self, list_id: Id, cap: usize) -> Result<Vec<Id>>;
    /// The `candidates` that belong to one of `owner`'s lists with
    /// `exclusive = true` (upstream's `exclusive_list_users`, which hides them
    /// from home). One read transaction; no scan cap, because the cost is the
    /// owner's exclusive lists times the candidates.
    fn exclusive_list_members(&self, owner: Id, candidates: &[Id]) -> Result<HashSet<Id>>;
}

/// Why a marker update was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerUpdateError {
    /// Unknown or invalid timeline name.
    InvalidTimeline,
    /// `lock_version` did not match (Mastodon answers 409).
    Stale,
}

pub trait MarkerStore: Send + Sync {
    /// The account's markers for `timelines` (missing ones are omitted).
    fn markers(&self, account_id: Id, timelines: &[&str]) -> Result<Vec<Marker>>;
    /// Upsert a marker with optimistic locking: when `lock_version` is given
    /// it must match the stored version, else `Stale`.
    fn set_marker(
        &self,
        account_id: Id,
        timeline: &str,
        last_read_id: Id,
        lock_version: Option<u64>,
        now_ms: u64,
    ) -> std::result::Result<Marker, MarkerUpdateError>;
}

/// A filter to create (v2 record; v1 create wraps exactly one keyword).
#[derive(Debug, Clone)]
pub struct NewFilter {
    pub account_id: Id,
    pub title: String,
    pub context: Vec<String>,
    pub expires_at_ms: Option<u64>,
    pub action: crate::domain::FilterAction,
    /// Initial keywords (`v2 keywords_attributes` or the v1 `phrase`).
    pub keywords: Vec<(String, bool)>,
}

/// Field-level filter patch; `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct FilterPatch {
    pub title: Option<String>,
    pub context: Option<Vec<String>>,
    /// `Some(None)` clears the expiry.
    pub expires_at_ms: Option<Option<u64>>,
    pub action: Option<crate::domain::FilterAction>,
    /// Replace keywords wholesale (`v2 keywords_attributes`): `(id, keyword,
    /// whole_word, destroy)`.
    pub keywords: Option<Vec<(Option<Id>, String, bool, bool)>>,
}

/// A filter with its rules, as every filter read returns it.
#[derive(Debug, Clone)]
pub struct FilterBundle {
    pub filter: CustomFilter,
    pub keywords: Vec<FilterKeyword>,
    pub statuses: Vec<FilterStatus>,
}

pub trait FilterStore: Send + Sync {
    fn create_filter(&self, input: NewFilter, now_ms: u64) -> Result<FilterBundle>;
    fn filter_by_id(&self, id: Id) -> Result<Option<FilterBundle>>;
    /// The account's filters (optionally narrowed to a context), bounded.
    fn filters_by_account(&self, account_id: Id, context: Option<&str>)
    -> Result<Vec<FilterBundle>>;
    /// Patch an owned filter; `None` when missing/foreign.
    fn update_filter(
        &self,
        account_id: Id,
        id: Id,
        patch: FilterPatch,
        now_ms: u64,
    ) -> Result<Option<FilterBundle>>;
    fn delete_filter(&self, account_id: Id, id: Id) -> Result<bool>;

    fn keyword_by_id(&self, id: Id) -> Result<Option<FilterKeyword>>;
    /// Add a keyword to an owned filter (id = the v1 "filter" id).
    fn create_keyword(
        &self,
        account_id: Id,
        filter_id: Id,
        keyword: &str,
        whole_word: bool,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>>;
    fn update_keyword(
        &self,
        account_id: Id,
        id: Id,
        keyword: Option<&str>,
        whole_word: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<FilterKeyword>>;
    fn delete_keyword(&self, account_id: Id, id: Id) -> Result<Option<FilterKeyword>>;

    fn filter_statuses(&self, filter_id: Id) -> Result<Vec<FilterStatus>>;
    fn create_filter_status(
        &self,
        account_id: Id,
        filter_id: Id,
        status_id: Id,
        now_ms: u64,
    ) -> Result<Option<FilterStatus>>;
    fn delete_filter_status(&self, account_id: Id, id: Id) -> Result<Option<FilterStatus>>;
}

/// A scheduled status to create in ONE transaction (record + indexes + the
/// `PublishScheduledStatus` job).
#[derive(Debug, Clone)]
pub struct NewScheduledStatus {
    pub account_id: Id,
    pub scheduled_at_ms: u64,
    pub media_ids: Vec<Id>,
    pub params: serde_json::Value,
    /// Public URL base, carried into the job payload so the publish handler
    /// can replay the params through the normal creation use case.
    pub base_url: String,
}

pub trait ScheduledStore: Send + Sync {
    fn create_scheduled(&self, input: NewScheduledStatus, now_ms: u64) -> Result<ScheduledStatus>;
    fn scheduled_by_id(&self, id: Id) -> Result<Option<ScheduledStatus>>;
    /// The account's scheduled statuses, id-descending with cursors.
    fn scheduled_by_account(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<ScheduledStatus>>;
    /// Reschedule (only `scheduled_at` is editable); `None` when
    /// missing/foreign.
    fn reschedule(
        &self,
        account_id: Id,
        id: Id,
        scheduled_at_ms: u64,
        now_ms: u64,
    ) -> Result<Option<ScheduledStatus>>;
    /// Delete a scheduled status (publish job payload becomes a no-op).
    fn delete_scheduled(&self, account_id: Id, id: Id) -> Result<bool>;
    /// Rows due for publication at `now_ms` (bounded).
    fn due_scheduled(&self, now_ms: u64, cap: usize) -> Result<Vec<ScheduledStatus>>;
    /// `(total, today)` counts against Mastodon's 300/25 limits.
    fn scheduled_counts(&self, account_id: Id, day_start_ms: u64, day_end_ms: u64)
    -> Result<(usize, usize)>;
}

pub trait EmojiStore: Send + Sync {
    /// Create or replace an emoji by shortcode (CLI-managed).
    fn upsert_emoji(&self, emoji: CustomEmoji) -> Result<CustomEmoji>;
    fn emoji_by_shortcode(&self, shortcode: &str) -> Result<Option<CustomEmoji>>;
    fn delete_emoji(&self, shortcode: &str) -> Result<bool>;
    /// Listed emoji (index endpoint; local + enabled + picker-visible),
    /// shortcode order, bounded.
    fn listed_emojis(&self, cap: usize) -> Result<Vec<CustomEmoji>>;
    /// Every emoji row (operator listing; shortcode order, bounded).
    fn all_emojis(&self, cap: usize) -> Result<Vec<CustomEmoji>>;
    /// Emoji whose shortcode is in `shortcodes` (status/account payloads).
    fn emojis_by_shortcodes(&self, shortcodes: &[String]) -> Result<Vec<CustomEmoji>>;
    /// Upsert one remote emoji row (federation tail, G-D ingest).
    fn upsert_remote_emoji(&self, row: RemoteEmojiRow) -> Result<RemoteEmojiRow>;
    /// Remote emoji rows by shortcode (federation tail, G-D): backs the
    /// `emojis` arrays for shortcodes with no local row. Empty when none.
    fn remote_emojis_by_shortcodes(&self, shortcodes: &[String]) -> Result<Vec<RemoteEmojiRow>>;
}

/// One remote custom emoji row (federation tail, G-D): the origin domain +
/// shortcode key the render path, with the source URL served in `emojis`
/// arrays (never picker-visible, never listed).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteEmojiRow {
    pub shortcode: String,
    pub domain: String,
    pub url: String,
}

/// Bounded local search scans backing `GET /api/v2/search`.
pub trait SearchStore: Send + Sync {
    /// Accounts whose lowercase handle or display name contains `needle`
    /// (bounded).
    fn search_accounts(&self, needle: &str, cap: usize) -> Result<Vec<Account>>;

    /// Distinct hashtags whose normalized name starts with `prefix`
    /// (bounded; derived from the hashtag index).
    fn search_tags(&self, prefix: &str, cap: usize) -> Result<Vec<String>>;

    /// Recent statuses (optionally one author's), id-descending with
    /// cursors, capped (bounded recent scan for text search).
    fn recent_statuses(
        &self,
        account_id: Option<Id>,
        max_id: Option<Id>,
        min_id: Option<Id>,
        cap: usize,
    ) -> Result<Vec<Status>>;
}

// ---------------------------------------------------------------------------
// Phase 7.3 ports: admin accounts moderation and the moderator report queue.
//
// Same invariants as the other ports: one `redb` transaction per method;
// mutating methods commit the record change, its index/counter updates and
// the audit-trail row atomically.
// ---------------------------------------------------------------------------

/// Moderation status filter for the admin account index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAccountStatus {
    /// No moderation flag and not disabled.
    Active,
    /// Awaiting approval: local accounts whose credential row has
    /// `approved == false` (upstream `User.pending`). Remote accounts, which
    /// have no credential row, never match.
    Pending,
    Disabled,
    Sensitized,
    Silenced,
    Suspended,
}

/// Filters for the admin account index. The v1 and v2 endpoints map their
/// (different) upstream parameter names onto this one struct.
#[derive(Debug, Clone, Default)]
pub struct AdminAccountFilter {
    /// `Some(true)` = local only, `Some(false)` = remote only, `None` = either.
    pub local_only: Option<bool>,
    /// Substring match on the account domain (`by_domain`).
    pub by_domain: Option<String>,
    /// Moderation status (`active`/`pending`/`disabled`/`sensitized`/
    /// `silenced`/`suspended`, v2 `status`).
    pub status: Option<AdminAccountStatus>,
    /// Substring match on the username (`username`).
    pub username_sub: Option<String>,
    /// Substring match on the display name (`display_name`).
    pub display_name_sub: Option<String>,
    /// Substring match on the credential email (`email`; local accounts only).
    pub email_sub: Option<String>,
    /// Substring match on a login IP (`ip`). Matches when any recorded login
    /// IP contains the substring (fix round P1/P3; IPs recorded since schema
    /// v15, older rows have none).
    pub ip_sub: Option<String>,
    /// Only staff accounts (`staff`, v2 `permissions=staff`).
    pub staff_only: bool,
    /// Only accounts created with an invite issued by this account (v2
    /// `invited_by`; upstream `AccountFilter#invited_by`). `None` = no
    /// constraint. A garbage `invited_by` value parses to `None` at the
    /// endpoint and matches nothing (see `index_v2`).
    pub invited_by: Option<Id>,
    /// v2 `role_ids`: matched against the role name (`admin`/`moderator`).
    /// Birchtree has no numeric role ids (documented gap).
    pub role_names: Vec<String>,
}

/// Field-level admin account patch. `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct AdminAccountPatch {
    pub suspended: Option<bool>,
    pub silenced: Option<bool>,
    pub sensitized: Option<bool>,
    pub disabled: Option<bool>,
    /// Approval state (`approve` sets `Some(true)`; upstream `User#approve!`
    /// also enables the account, which the endpoint expresses as
    /// `disabled: Some(false)` alongside).
    pub approved: Option<bool>,
    /// Audit action name recorded with the write (`enable`, `suspend`, ...).
    pub audit_action: String,
}

/// Filters for the moderator report queue.
#[derive(Debug, Clone, Default)]
pub struct AdminReportFilter {
    /// `Some(true)` = resolved only, `Some(false)` = unresolved only.
    pub resolved: Option<bool>,
    /// Only reports filed by this account (`account_id`).
    pub reporter_id: Option<Id>,
    /// Only reports about this account (`target_account_id`).
    pub target_account_id: Option<Id>,
}

/// Field-level moderator report patch. `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct AdminReportPatch {
    /// New category (`spam`, `legal`, `other`, ...).
    pub category: Option<String>,
    /// Attached rule ids. A non-empty list is validated against the rule
    /// store (unknown ids are refused `422`, like upstream
    /// `validate_rule_ids`); empty clears nothing (stored as given when
    /// empty).
    pub rule_ids: Option<Vec<String>>,
    /// `Some(None)` unassigns; `None` leaves the assignee unchanged.
    pub assigned_account_id: Option<Option<Id>>,
    /// Resolve (`Some(true)`) or reopen (`Some(false)`).
    pub action_taken: Option<bool>,
    /// The moderator resolving; set when resolving.
    pub action_taken_by_account_id: Option<Id>,
    /// Audit action name recorded with the write (`update`, `resolve`, ...).
    pub audit_action: String,
}

/// Admin account moderation and the moderator report queue.
pub trait AdminStore: Send + Sync {
    /// Admin account index, id-descending with exclusive cursors, bounded:
    /// at most `limit` rows after scanning at most `limit * 20` (capped at
    /// 5000) index entries. Each row carries its credential (`None` for
    /// remote accounts, which have no local user).
    fn admin_accounts_page(
        &self,
        filter: &AdminAccountFilter,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        ascending: bool,
        limit: usize,
    ) -> Result<Vec<(Account, Option<LocalUser>)>>;

    /// Apply an account moderation patch in ONE transaction (account row,
    /// credential row when `disabled` changes, and the audit row). `NotFound`
    /// when the account is missing.
    fn admin_update_account(
        &self,
        actor_account_id: Id,
        account_id: Id,
        patch: AdminAccountPatch,
        now_ms: u64,
    ) -> Result<(Account, Option<LocalUser>)>;

    /// Suspend and disable an account in ONE transaction (the audit row
    /// records `audit_action`: `destroy` for DELETE, `reject` for reject),
    /// and enqueue the async deletion worker's first batch plus the
    /// federated `Delete` of the actor in the same transaction (D38).
    /// Returns `false` when the account is missing. The account's statuses
    /// are removed by the bounded, restartable batch chain, not here; the
    /// account row itself is retained (suspended) so its URI resolves.
    fn admin_delete_account(
        &self,
        actor_account_id: Id,
        account_id: Id,
        audit_action: &str,
        now_ms: u64,
    ) -> Result<bool>;

    /// One account-action write in ONE transaction: the moderation flags, the
    /// warning row it records (every action type records a strike, like
    /// upstream `process_strike!`; `text` is the moderator's warning text),
    /// the report resolutions it implies, and one audit row for the action
    /// plus one per resolved report. `NotFound` when the account (or the
    /// named report) is missing.
    fn admin_account_action(
        &self,
        actor_account_id: Id,
        account_id: Id,
        action: &str,
        report_id: Option<Id>,
        text: Option<String>,
        now_ms: u64,
    ) -> Result<()>;

    /// Moderator report queue, id-descending with exclusive cursors, bounded
    /// like [`AdminStore::admin_accounts_page`].
    fn admin_reports_page(
        &self,
        filter: &AdminReportFilter,
        max_id: Option<Id>,
        since_id: Option<Id>,
        min_id: Option<Id>,
        ascending: bool,
        limit: usize,
    ) -> Result<Vec<Report>>;

    /// Apply a moderator report patch in ONE transaction (report row + audit
    /// row). `Ok(None)` when the report is missing.
    fn admin_update_report(
        &self,
        actor_account_id: Id,
        id: Id,
        patch: AdminReportPatch,
        now_ms: u64,
    ) -> Result<Option<Report>>;

    /// Audit records, newest first, bounded by `cap` (test and operator reads).
    fn admin_audit_entries(&self, cap: usize) -> Result<Vec<AdminAuditRecord>>;

    /// Warning rows (strikes) against `target_account_id`, newest first
    /// (test and operator reads; upstream serves warnings through the
    /// dashboard only, with no admin API read).
    fn admin_warnings_for_account(
        &self,
        target_account_id: Id,
    ) -> Result<Vec<crate::domain::AccountWarning>>;
}

/// Filters a candidate status through the *viewer's* perspective while a
/// bounded timeline scan fills a page. Implemented in
/// `application::timelines` (the policy stays in the application layer); the
/// store only applies it inside its scan budget.
pub trait TimelineFilter: Send + Sync {
    fn include(&self, status: &Status) -> bool;
}

/// Cursor-bounded timeline query (limit is already endpoint-bounded).
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineQuery {
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
    pub limit: usize,
    /// `only_media` (public/tag timelines).
    pub only_media: bool,
    /// `local=true` (public/tag timelines).
    pub local_only: bool,
    /// `remote=true` (public/tag timelines).
    pub remote_only: bool,
}

/// One page of a timeline plus the number of index entries the bounded scan
/// walked (executed evidence for the no-unbounded-scan tests).
#[derive(Debug, Clone, Default)]
pub struct TimelinePage {
    pub statuses: Vec<Status>,
    pub scanned: usize,
}

pub trait TimelineStore: Send + Sync {
    /// Public timeline: `public`-visibility statuses, newest first. Replies
    /// (other-than-self) and reblogs are excluded unless the flags say
    /// otherwise (tag timelines include both).
    fn public_timeline(
        &self,
        query: TimelineQuery,
        include_replies: bool,
        include_reblogs: bool,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;

    /// Hashtag timeline for an already-normalized tag.
    fn hashtag_timeline(
        &self,
        tag: &str,
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;

    /// Fan-out-on-read merge over `authors` (own + followed accounts for
    /// home; list members for a list), newest first, with per-author and
    /// total scan budgets.
    fn authors_timeline(
        &self,
        authors: &[Id],
        query: TimelineQuery,
        filter: &dyn TimelineFilter,
    ) -> Result<TimelinePage>;
}

/// Unified interaction-event journal backing the admin `interactions`
/// measure (D39). Writers are the application use cases, one row per local
/// interaction; readers are the admin measures. Scaffold defaults refuse
/// loudly; RedbStore overrides (see `persistence::redb::interactions`).
pub trait InteractionLog: Send + Sync {
    /// Append one interaction event (idempotent only by construction: callers
    /// journal each user action once; a retried *job* must not re-journal —
    /// journal at the use-case commit, never in a job handler).
    fn record_interaction(&self, event: InteractionEvent) -> Result<()> {
        let _ = event;
        Err(Error::Internal("record_interaction is not implemented".into()))
    }

    /// Count journaled events in `[since_ms, until_ms)`, optionally filtered
    /// to one kind. `None` kind counts everything (the `interactions` key).
    fn count_interactions(
        &self,
        _kind: Option<InteractionEventKind>,
        _since_ms: u64,
        _until_ms: u64,
    ) -> Result<u64> {
        Err(Error::Internal("count_interactions is not implemented".into()))
    }
}

// ---------------------------------------------------------------------------
// Collections (gaps batch G-C; port moved here at the parent merge so the
// REST adapters can join it to `Store`).
// ---------------------------------------------------------------------------

/// A curated collection of statuses owned by one account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CollectionRecord {
    pub id: Id,
    pub account_id: Id,
    pub title: String,
    pub description: String,
    pub public: bool,
    #[serde(default)]
    pub revoked: bool,
    /// Denormalized membership count. Stays rendered even when the owner's
    /// `hide_collections` hides the members (D30/D52: members hidden, counts
    /// stay).
    pub items_count: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// What changed in a collection (fan-out job payload + handler dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionChange {
    Add,
    Remove,
    Revoke,
    Delete,
}

impl CollectionChange {
    pub const fn as_str(self) -> &'static str {
        match self {
            CollectionChange::Add => "add",
            CollectionChange::Remove => "remove",
            CollectionChange::Revoke => "revoke",
            CollectionChange::Delete => "delete",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "add" => CollectionChange::Add,
            "remove" => CollectionChange::Remove,
            "revoke" => CollectionChange::Revoke,
            "delete" => CollectionChange::Delete,
            _ => return None,
        })
    }
}

/// Narrow port over the `COLLECTIONS` / `COLLECTION_ITEMS` tables.
///
/// `account_id` arguments are the claimed owner: trait methods return
/// `NotFound("Record not found")` for missing *or* foreign collections (never
/// an ownership oracle). Use-case wrappers add validation and policy.
pub trait CollectionStore: Send + Sync {
    fn create_collection(
        &self,
        account_id: Id,
        title: &str,
        description: &str,
        public: bool,
        now_ms: u64,
    ) -> Result<CollectionRecord>;

    fn collection_by_id(&self, id: Id) -> Result<Option<CollectionRecord>>;

    /// The owner's collections, newest first.
    fn collections_by_account(&self, account_id: Id) -> Result<Vec<CollectionRecord>>;

    /// Patch an owned collection; `None` when missing/foreign.
    fn update_collection(
        &self,
        account_id: Id,
        id: Id,
        title: Option<String>,
        description: Option<String>,
        public: Option<bool>,
        now_ms: u64,
    ) -> Result<Option<CollectionRecord>>;

    /// Delete an owned collection and all its memberships; `false` when
    /// missing/foreign. Enqueues one `CollectionFanout` (`delete`) in the
    /// same transaction.
    fn delete_collection(&self, account_id: Id, id: Id, now_ms: u64) -> Result<bool>;

    /// Member status ids, newest first; cursors exclusive (`id < max_id`),
    /// bounded scan.
    fn collection_items(
        &self,
        collection_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<Id>>;

    /// The owner's collections containing `status_id`, newest first.
    fn collections_containing_status(
        &self,
        account_id: Id,
        status_id: Id,
    ) -> Result<Vec<CollectionRecord>>;

    /// Add members (idempotent; existing members skipped). Returns the newly
    /// added ids. Enqueues one `CollectionFanout` (`add`, batch ≤ 500) in
    /// the same transaction. `NotFound` when missing/foreign; `InvalidInput`
    /// when revoked, over the batch bound, or over the item cap.
    fn add_collection_items(
        &self,
        account_id: Id,
        collection_id: Id,
        status_ids: &[Id],
        now_ms: u64,
    ) -> Result<Vec<Id>>;

    /// Remove members (idempotent). Returns the removed ids. Enqueues one
    /// `CollectionFanout` (`remove`) in the same transaction when anything
    /// was removed.
    fn remove_collection_items(
        &self,
        account_id: Id,
        collection_id: Id,
        status_ids: &[Id],
        now_ms: u64,
    ) -> Result<Vec<Id>>;

    /// Revoke an owned collection: every membership is dropped,
    /// `revoked = true`, and one `CollectionFanout` (`revoke`) is enqueued
    /// in the same transaction. `None` when missing/foreign.
    fn revoke_collection(
        &self,
        account_id: Id,
        id: Id,
        now_ms: u64,
    ) -> Result<Option<CollectionRecord>>;

    /// `(account_id, reblog_id)` pairs for `status_id`, newest first, scoped
    /// to distributable reblogs: the reblog wrapper must exist, must not be
    /// a tombstone, and its visibility must be `distributable` (public or
    /// unlisted — `Visibility::is_distributable`). This is the D52 scope the
    /// plain `reblogged_by` scan lacks.
    fn reblogged_by_distributable(
        &self,
        status_id: Id,
        max_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<(Id, Id)>>;
}

/// Bundled persistence surface assembled into a `Context`.
///
/// Implemented once by the `redb` adapter; application code depends on the
/// individual narrow traits above, never on `redb` types.
pub trait Store:
    AccountStore
    + OAuthStore
    + SessionStore
    + JobQueue
    + ReadyCheck
    + InteractionLog
    + StatusStore
    + RelationshipStore
    + PollStore
    + QuoteStore
    + NotificationStore
    + MediaStore
    + PlaceStore
    + ListStore
    + MarkerStore
    + SuggestionStore
    + FilterStore
    + ScheduledStore
    + EmojiStore
    + TimelineStore
    + SearchStore
    + TrendStore
    + AnnualReportStore
    + crate::application::federation_ports::FederationStore
    + crate::application::federation_ports::FederationContentStore
    + AdminInfraStore
    + AdminStore
    + crate::application::announcements::AnnouncementStore
    + crate::application::conversations::ConversationStore
    + crate::application::push::PushSubscriptionStore
    + WebStore
    + CacheStore
    + crate::application::accounts::SecondFactorStore
    + CollectionStore
{
}
impl<T> Store for T where
    T: AccountStore
        + OAuthStore
        + SessionStore
        + JobQueue
        + ReadyCheck
        + InteractionLog
        + StatusStore
        + RelationshipStore
        + PollStore
        + QuoteStore
        + NotificationStore
        + MediaStore
        + PlaceStore
        + ListStore
        + MarkerStore
        + SuggestionStore
        + FilterStore
        + ScheduledStore
        + EmojiStore
        + TimelineStore
        + SearchStore
        + TrendStore
        + AnnualReportStore
        + crate::application::federation_ports::FederationStore
    + crate::application::federation_ports::FederationContentStore
    + AdminInfraStore
    + AdminStore
    + crate::application::announcements::AnnouncementStore
    + crate::application::conversations::ConversationStore
    + crate::application::push::PushSubscriptionStore
    + WebStore
    + CacheStore
    + crate::application::accounts::SecondFactorStore
    + CollectionStore
{
}

/// Publishes committed changes to open streams (Phase 5).
///
/// Non-blocking. The hub's delivery runs on its own thread, so a slow client or
/// a slow policy read never stalls the request that committed the change. If
/// the hub's event queue overflows, the hub closes every open stream instead of
/// dropping the event silently: clients reconnect and reread state.
pub trait StreamingSink: Send + Sync {
    fn publish(&self, event: StreamEvent);
}

/// A sink that discards events. Used where no streaming hub is assembled.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStreaming;

impl StreamingSink for NoStreaming {
    fn publish(&self, _event: StreamEvent) {}
}

/// Snapshot of kernel dependencies assembled in `application::context`.
#[derive(Clone)]
pub struct ContextDeps {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
}

/// Dependency-assembly surface for adapters.
pub trait Ports: Send + Sync {
    fn clock(&self) -> Arc<dyn Clock>;
    fn ids(&self) -> Arc<dyn IdGenerator>;
}

impl Ports for ContextDeps {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
    fn ids(&self) -> Arc<dyn IdGenerator> {
        self.ids.clone()
    }
}

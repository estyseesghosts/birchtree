//! `redb` table definitions.
//!
//! Tables are addressed by these `const` definitions; no application code opens
//! them directly. Values are JSON-encoded bytes so a record can evolve without
//! a key-layout migration; keys are the native ordered primitive so range
//! scans stay bounded and index-ordered.

use redb::TableDefinition;

/// Schema/migration metadata (key -> value bytes).
pub const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// Accounts by numeric ID.
pub const ACCOUNTS: TableDefinition<u64, &[u8]> = TableDefinition::new("accounts");

/// Unique handle index: normalized `user@domain` -> account ID.
pub const HANDLE_INDEX: TableDefinition<&str, u64> = TableDefinition::new("handle_index");

/// Local credentials by account ID.
pub const LOCAL_USERS: TableDefinition<u64, &[u8]> = TableDefinition::new("local_users");

/// OAuth applications by client ID.
pub const OAUTH_APPS: TableDefinition<&str, &[u8]> = TableDefinition::new("oauth_apps");

/// Access tokens by hex-encoded SHA-256 token hash.
pub const ACCESS_TOKENS: TableDefinition<&str, &[u8]> = TableDefinition::new("access_tokens");

/// OAuth authorization codes by hex-encoded SHA-256 code hash (schema v2).
pub const AUTHORIZATION_CODES: TableDefinition<&str, &[u8]> =
    TableDefinition::new("authorization_codes");

/// Login sessions by hex-encoded SHA-256 session-token hash.
pub const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");

/// Durable jobs by numeric job ID.
pub const JOBS: TableDefinition<u64, &[u8]> = TableDefinition::new("jobs");

/// Job scheduling index: fixed 16-byte `(due_ms_be || job_id_be)` -> job ID.
///
/// For a pending job the leading timestamp is its due time; for a leased job it
/// is the lease expiry, so an abandoned lease becomes claimable again without a
/// recovery scan.
pub const JOBS_INDEX: TableDefinition<&[u8], u64> = TableDefinition::new("jobs_index");

// ---------------------------------------------------------------------------
// Phase 3 (schema v3): statuses, relationships, interactions, polls, quotes.
// Composite keys are fixed-width big-endian pairs so range scans stay bounded
// and index-ordered. `desc_id` flips an ID so descending order is a forward
// scan (never lexicographic on decimal strings).
// ---------------------------------------------------------------------------

/// Statuses by numeric status ID.
pub const STATUSES: TableDefinition<u64, &[u8]> = TableDefinition::new("statuses");

/// Statuses by author: 16-byte `(account_id_be || desc(status_id)_be)` -> status ID.
pub const STATUS_AUTHOR_INDEX: TableDefinition<&[u8], u64> =
    TableDefinition::new("status_author_index");

/// Reply children: 16-byte `(parent_id_be || child_id_be)` -> child status ID.
pub const STATUS_PARENT_INDEX: TableDefinition<&[u8], u64> =
    TableDefinition::new("status_parent_index");

/// Reblogs of a status: 16-byte `(target_id_be || reblog_id_be)` -> reblog status ID.
pub const STATUS_REBLOG_INDEX: TableDefinition<&[u8], u64> =
    TableDefinition::new("status_reblog_index");

/// The viewer's reblog of a target: `(account_id_be || target_id_be)` -> reblog status ID.
pub const REBLOGS_BY_ACCOUNT: TableDefinition<&[u8], u64> =
    TableDefinition::new("reblogs_by_account");

/// Edit-history snapshots: 16-byte `(status_id_be || version_be)` -> JSON snapshot.
pub const STATUS_VERSIONS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("status_versions");

/// Pinned statuses per account: 16-byte `(account_id_be || status_id_be)` -> JSON pin.
pub const STATUS_PINS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("status_pins");

/// Favourites: 16-byte `(account_id_be || status_id_be)` -> JSON favourite.
pub const FAVOURITES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("favourites");

/// Reverse favourites index: `(status_id_be || favourite_id_be)` -> account ID.
pub const FAVOURITES_BY_STATUS: TableDefinition<&[u8], u64> =
    TableDefinition::new("favourites_by_status");

/// Bookmarks: 16-byte `(account_id_be || status_id_be)` -> JSON bookmark.
pub const BOOKMARKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("bookmarks");

/// Reverse bookmarks index: `(status_id_be || bookmark_id_be)` -> account ID.
pub const BOOKMARKS_BY_STATUS: TableDefinition<&[u8], u64> =
    TableDefinition::new("bookmarks_by_status");

/// Follows (active or pending request): `(follower_id_be || followed_id_be)` -> JSON follow.
pub const FOLLOWS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("follows");

/// Reverse follows index: `(followed_id_be || follower_id_be)` -> ().
pub const FOLLOWS_BY_FOLLOWED: TableDefinition<&[u8], ()> =
    TableDefinition::new("follows_by_followed");

/// Blocks: `(blocker_id_be || blocked_id_be)` -> JSON block.
pub const BLOCKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks");

/// Reverse blocks index: `(blocked_id_be || blocker_id_be)` -> ().
pub const BLOCKS_BY_BLOCKED: TableDefinition<&[u8], ()> = TableDefinition::new("blocks_by_blocked");

/// Domain blocks: `(account_id_be || 0xFF || domain)` -> JSON domain block.
pub const DOMAIN_BLOCKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("domain_blocks");

/// Domain blocks by row id: `(account_id_be || desc(id)_be)` -> JSON domain
/// block. Ascending key order is newest first, which gives Mastodon's
/// `max_id`/`since_id` paging over the account's block list.
pub const DOMAIN_BLOCKS_BY_ID: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("domain_blocks_by_id");

/// Mutes: `(muter_id_be || muted_id_be)` -> JSON mute (expiry + notify flag).
pub const MUTES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("mutes");

/// Reverse mutes index: `(muted_id_be || muter_id_be)` -> ().
pub const MUTES_BY_MUTED: TableDefinition<&[u8], ()> = TableDefinition::new("mutes_by_muted");

/// Private per-viewer account notes: `(viewer_id_be || target_id_be)` -> JSON note.
pub const ACCOUNT_NOTES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("account_notes");

/// Endorsements (account pins): `(account_id_be || endorsed_id_be)` -> JSON endorsement.
pub const ENDORSEMENTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("endorsements");

/// Reverse endorsements index: `(endorsed_id_be || account_id_be)` -> ().
pub const ENDORSEMENTS_BY_ENDORSED: TableDefinition<&[u8], ()> =
    TableDefinition::new("endorsements_by_endorsed");

/// Polls by numeric poll ID (options, per-option vote counts and voters embedded).
pub const POLLS: TableDefinition<u64, &[u8]> = TableDefinition::new("polls");

/// Poll ID by its status ID.
pub const POLL_BY_STATUS: TableDefinition<u64, u64> = TableDefinition::new("poll_by_status");

/// One voter's ballot per poll: `(poll_id_be || account_id_be)` -> JSON vote.
pub const POLL_VOTES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("poll_votes");

/// Quotes by numeric quote ID.
pub const QUOTES: TableDefinition<u64, &[u8]> = TableDefinition::new("quotes");

/// Quotes of a status: `(quoted_status_id_be || quoting_status_id_be)` -> quote ID.
pub const QUOTES_BY_QUOTED: TableDefinition<&[u8], u64> = TableDefinition::new("quotes_by_quoted");

/// The quote authored by a status: `quoting_status_id_be` -> quote ID.
pub const QUOTES_BY_QUOTING: TableDefinition<u64, u64> = TableDefinition::new("quotes_by_quoting");

/// Notifications by numeric notification ID.
pub const NOTIFICATIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("notifications");

/// Notifications per recipient: `(recipient_id_be || desc(notification_id)_be)` -> ID.
pub const NOTIFICATIONS_BY_RECIPIENT: TableDefinition<&[u8], u64> =
    TableDefinition::new("notifications_by_recipient");

/// Notification dedup: `(recipient_be || type_be || actor_be || status_be ||
/// created_at_be)` (40 bytes) -> notification ID.
pub const NOTIFICATION_DEDUP: TableDefinition<&[u8], u64> =
    TableDefinition::new("notification_dedup");

/// Idempotency records: `(account_id_be || sha256(key)[..16])` -> JSON record.
pub const IDEMPOTENCY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("idempotency");

/// Per-account counters (followers/following/statuses/last status/follow requests).
pub const ACCOUNT_STATS: TableDefinition<u64, &[u8]> = TableDefinition::new("account_stats");

/// Media attachment metadata (bytes/lifecycle are Phase 4).
pub const MEDIA: TableDefinition<u64, &[u8]> = TableDefinition::new("media");

/// Media by owner: `(owner_id_be || media_id_be)` -> ().
pub const MEDIA_BY_OWNER: TableDefinition<&[u8], ()> = TableDefinition::new("media_by_owner");

/// Mentions linking a status to a mentioned account: `(account_id_be || status_id_be)` -> ().
pub const MENTIONS_BY_ACCOUNT: TableDefinition<&[u8], ()> =
    TableDefinition::new("mentions_by_account");

/// Hashtag index: `(tag_be || 0xFF || status_id_be)` -> status ID.
pub const HASHTAG_INDEX: TableDefinition<&[u8], u64> = TableDefinition::new("hashtag_index");

// ---------------------------------------------------------------------------
// Phase 4 (schema v4): timelines, lists, markers, filters, scheduled
// statuses, custom emoji.
// ---------------------------------------------------------------------------

/// Public timeline range: `desc(status_id)_be` -> () for `public`-visibility
/// statuses (local and remote), so the public timeline is a forward scan in
/// descending id order.
pub const TIMELINE_PUBLIC: TableDefinition<u64, ()> = TableDefinition::new("timeline_public");

/// Local timeline range: `desc(status_id)_be` -> () for public statuses
/// authored by local accounts.
pub const TIMELINE_LOCAL: TableDefinition<u64, ()> = TableDefinition::new("timeline_local");

/// Lists by numeric list ID.
pub const LISTS: TableDefinition<u64, &[u8]> = TableDefinition::new("lists");

/// Lists owned by an account: `(account_id_be || desc(list_id)_be)` -> ().
pub const LISTS_BY_ACCOUNT: TableDefinition<&[u8], ()> = TableDefinition::new("lists_by_account");

/// List membership: `(list_id_be || member_id_be)` -> ().
pub const LIST_MEMBERS: TableDefinition<&[u8], ()> = TableDefinition::new("list_members");

/// Reverse membership index (unfollow/block cleanup): `(member_id_be ||
/// list_id_be)` -> ().
pub const MEMBERSHIPS_BY_ACCOUNT: TableDefinition<&[u8], ()> =
    TableDefinition::new("memberships_by_account");

/// Per-user timeline markers (home / notifications last-read ids):
/// `(account_id_be || 0xFF || timeline)` -> JSON marker.
pub const MARKERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("markers");

/// Custom filters (filter v2 model) by numeric ID.
pub const FILTERS: TableDefinition<u64, &[u8]> = TableDefinition::new("filters");

/// Filters owned by an account: `(account_id_be || filter_id_be)` -> ().
pub const FILTERS_BY_ACCOUNT: TableDefinition<&[u8], ()> = TableDefinition::new("filters_by_account");

/// Filter keywords by numeric keyword ID.
pub const FILTER_KEYWORDS: TableDefinition<u64, &[u8]> = TableDefinition::new("filter_keywords");

/// Keywords of a filter: `(filter_id_be || keyword_id_be)` -> ().
pub const FILTER_KEYWORDS_BY_FILTER: TableDefinition<&[u8], ()> =
    TableDefinition::new("filter_keywords_by_filter");

/// Filter status rules by numeric rule ID.
pub const FILTER_STATUSES: TableDefinition<u64, &[u8]> = TableDefinition::new("filter_statuses");

/// Status rules of a filter: `(filter_id_be || rule_id_be)` -> ().
pub const FILTER_STATUSES_BY_FILTER: TableDefinition<&[u8], ()> =
    TableDefinition::new("filter_statuses_by_filter");

/// Scheduled statuses by numeric ID (params + attached media ids embedded).
pub const SCHEDULED_STATUSES: TableDefinition<u64, &[u8]> =
    TableDefinition::new("scheduled_statuses");

/// Scheduled statuses of an account: `(account_id_be || desc(id)_be)` -> ().
pub const SCHEDULED_BY_ACCOUNT: TableDefinition<&[u8], ()> =
    TableDefinition::new("scheduled_by_account");

/// Custom emoji by shortcode (ordered, so the index endpoint lists them
/// alphabetically).
pub const CUSTOM_EMOJI: TableDefinition<&str, &[u8]> = TableDefinition::new("custom_emoji");

// ---------------------------------------------------------------------------
// Phase 6 (schema v6): federation. Keys by account, the actor-URI index, the
// inbound activity dedup record and the outbound follow-by-URI index.
// ---------------------------------------------------------------------------

/// Signing keys by account ID (JSON `StoredKeys`; local accounts carry the private key).
pub const ACTOR_KEYS: TableDefinition<u64, &[u8]> = TableDefinition::new("actor_keys");

/// Accounts by canonical actor URI -> account ID (local and remote).
pub const ACCOUNT_BY_ACTOR_URI: TableDefinition<&str, u64> =
    TableDefinition::new("account_by_actor_uri");

/// Applied inbound activities by canonical activity URI -> recorded-at ms.
pub const ACTIVITY_DEDUP: TableDefinition<&str, u64> = TableDefinition::new("activity_dedup");

/// Outbound follow rows by follow activity URI -> 16-byte `(follower_id_be || followed_id_be)`.
pub const FOLLOW_BY_URI: TableDefinition<&str, &[u8]> = TableDefinition::new("follow_by_uri");

// ---------------------------------------------------------------------------
// Phase 6 (schema v7): status identity by URI, remote-thread refresh cooldown,
// and reports (Flag).
// ---------------------------------------------------------------------------

/// Status ID by canonical status URI (local and remote statuses).
pub const STATUS_BY_URI: TableDefinition<&str, u64> = TableDefinition::new("status_by_uri");

/// Last remote-thread refresh of a status, by status ID (JSON `RefreshRecord`).
pub const REMOTE_REFRESH: TableDefinition<u64, &[u8]> = TableDefinition::new("remote_refresh");

/// Quote by its authorization URI (`approval_uri`) -> quote ID.
pub const QUOTE_BY_APPROVAL: TableDefinition<&str, u64> = TableDefinition::new("quote_by_approval");

/// The parent URI of a reply whose parent is not known here yet, by reply status ID.
pub const UNRESOLVED_PARENT: TableDefinition<u64, &str> = TableDefinition::new("unresolved_parent");

/// Reports (Flag, local and remote), by report ID (JSON).
pub const REPORTS: TableDefinition<u64, &[u8]> = TableDefinition::new("reports");

/// The remote URL of a media attachment fetched from another server, by media ID.
pub const REMOTE_MEDIA_SOURCE: TableDefinition<u64, &str> = TableDefinition::new("remote_media_source");

/// Key under `META` holding the applied schema version (big-endian `u64`).
pub const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Current schema version. Bump with a new migration step.
pub const SCHEMA_VERSION: u64 = 7;

/// All table names, for diagnostics and migration verification.
pub const TABLE_NAMES: &[&str] = &[
    "meta",
    "accounts",
    "handle_index",
    "local_users",
    "oauth_apps",
    "access_tokens",
    "authorization_codes",
    "sessions",
    "jobs",
    "jobs_index",
    "statuses",
    "status_by_uri",
    "remote_refresh",
    "reports",
    "remote_media_source",
    "quote_by_approval",
    "unresolved_parent",
    "status_author_index",
    "status_parent_index",
    "status_reblog_index",
    "reblogs_by_account",
    "status_versions",
    "status_pins",
    "favourites",
    "favourites_by_status",
    "bookmarks",
    "bookmarks_by_status",
    "follows",
    "follows_by_followed",
    "blocks",
    "blocks_by_blocked",
    "domain_blocks",
    "domain_blocks_by_id",
    "mutes",
    "mutes_by_muted",
    "account_notes",
    "endorsements",
    "endorsements_by_endorsed",
    "polls",
    "poll_by_status",
    "poll_votes",
    "quotes",
    "quotes_by_quoted",
    "quotes_by_quoting",
    "notifications",
    "notifications_by_recipient",
    "notification_dedup",
    "idempotency",
    "account_stats",
    "media",
    "media_by_owner",
    "mentions_by_account",
    "hashtag_index",
];

/// Build the fixed 16-byte scheduling key used by [`JOBS_INDEX`].
pub fn job_index_key(timestamp_ms: u64, job_id: u64) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&timestamp_ms.to_be_bytes());
    key[8..].copy_from_slice(&job_id.to_be_bytes());
    key
}

/// Flip an ID so a forward scan over `(a_be || desc(id)_be)` yields descending
/// numeric order for the second component.
pub const fn desc_id(id: u64) -> u64 {
    u64::MAX - id
}

/// Fixed 16-byte composite key `(a_be || b_be)`.
pub fn pair_key(a: u64, b: u64) -> [u8; 16] {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&a.to_be_bytes());
    key[8..].copy_from_slice(&b.to_be_bytes());
    key
}

/// Key for [`DOMAIN_BLOCKS`]: `account_be || 0xFF || domain` (`0xFF` cannot
/// appear in a domain name, so the account prefix scan stays exact).
pub fn domain_block_key(account_id: u64, domain: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(9 + domain.len());
    key.extend_from_slice(&account_id.to_be_bytes());
    key.push(0xFF);
    key.extend_from_slice(domain.as_bytes());
    key
}

/// Fixed 40-byte notification dedup key. The interaction timestamp scopes the
/// key to one interaction *instance*: a retried job for the same interaction
/// deduplicates, while a fresh interaction (e.g. re-favouriting after
/// unfavouriting) produces a new notification.
pub fn notification_dedup_key(
    recipient_id: u64,
    kind_ord: u64,
    actor_id: u64,
    status_id: u64,
    created_at_ms: u64,
) -> [u8; 40] {
    let mut key = [0u8; 40];
    key[..8].copy_from_slice(&recipient_id.to_be_bytes());
    key[8..16].copy_from_slice(&kind_ord.to_be_bytes());
    key[16..24].copy_from_slice(&actor_id.to_be_bytes());
    key[24..32].copy_from_slice(&status_id.to_be_bytes());
    key[32..].copy_from_slice(&created_at_ms.to_be_bytes());
    key
}

/// Key for [`HASHTAG_INDEX`]: `tag_be || 0xFF || status_id_be`.
pub fn hashtag_key(tag: &str, status_id: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(tag.len() + 9);
    key.extend_from_slice(tag.as_bytes());
    key.push(0xFF);
    key.extend_from_slice(&status_id.to_be_bytes());
    key
}

//! Relationship use cases: follows and follow requests, blocks, mutes,
//! domain blocks, private account notes, endorsements, and the relationship
//! aggregate read.
//!
//! Validation happens here, before the store call (same pattern as
//! `accounts.rs`); each store method commits row + counters + affected
//! indexes + follow-up jobs in ONE transaction.

use crate::application::ports::{
    FollowOptions, FollowOutcome, Relationship, Store,
};
use crate::domain::{
    Account, AccountNote, Block, DomainBlock, Endorsement, Error, Follow, Id, Mute, Result,
};

/// Maximum private-note length (Mastodon `AccountNote::COMMENT_SIZE_LIMIT`).
pub const ACCOUNT_NOTE_MAX_LEN: usize = 2000;

/// Load an account or fail Mastodon-shaped: 404 `Record not found`.
fn require_account(store: &dyn Store, id: Id) -> Result<Account> {
    store
        .account_by_id(id)?
        .ok_or_else(|| Error::NotFound("account".into()))
}

/// Normalize a domain for storage and lookup: trim, drop one leading `@`
/// (pasted `@host` forms), and lowercase.
///
/// `None` when the result is blank or still carries a scheme (`:` covers
/// `https://` and ports), path, query/fragment, whitespace, or a leftover
/// `@` — i.e. anything `domain_block` must reject.
fn normalize_domain(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let domain = stripped.trim().to_lowercase();
    if domain.is_empty() {
        return None;
    }
    if domain
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '/' | ':' | '?' | '#' | '@'))
    {
        return None;
    }
    Some(domain)
}

/// Input for `POST /api/v1/accounts/:id/follow`.
#[derive(Debug, Clone)]
pub struct FollowCommand {
    pub follower_id: Id,
    pub target_id: Id,
    /// `reblogs` query flag (default true).
    pub show_reblogs: Option<bool>,
    /// `notify` query flag (default false).
    pub notify: Option<bool>,
}

/// Follow an account, or open a follow request when the target is locked.
///
/// Mirrors Mastodon `FollowService`: unknown target -> 404, self-follow -> 403
/// `Following your own account is not allowed`, a block in either direction ->
/// 403 `This action is not allowed`. Otherwise the store creates an active
/// follow, or a pending request when the target is locked and no active
/// follow exists yet (the store keeps an already-active follow active).
pub fn follow(store: &dyn Store, now_ms: u64, cmd: FollowCommand) -> Result<FollowOutcome> {
    let target = require_account(store, cmd.target_id)?;
    if cmd.follower_id == cmd.target_id {
        return Err(Error::Forbidden(
            "Following your own account is not allowed".into(),
        ));
    }
    let pair = store.pair_state(cmd.follower_id, cmd.target_id, now_ms)?;
    if pair.blocking || pair.blocked_by {
        return Err(Error::Forbidden("This action is not allowed".into()));
    }
    let opts = FollowOptions {
        show_reblogs: cmd.show_reblogs.unwrap_or(true),
        notify: cmd.notify.unwrap_or(false),
    };
    // A request only opens when there is no ACTIVE follow yet; an existing
    // row keeps its state (the store updates the options idempotently).
    let request = target.locked && !pair.following;
    store.follow(cmd.follower_id, cmd.target_id, opts, request, now_ms)
}

/// Unfollow an account or cancel a pending follow request.
///
/// Returns the removed `Follow`, or `Ok(None)` when there was nothing to
/// remove. Unknown follower or target -> 404.
pub fn unfollow(
    store: &dyn Store,
    follower_id: Id,
    target_id: Id,
    now_ms: u64,
) -> Result<Option<Follow>> {
    require_account(store, follower_id)?;
    require_account(store, target_id)?;
    store.unfollow(follower_id, target_id, now_ms)
}

/// Remove an inbound follow (drop one of your own followers). Returns the
/// removed `Follow`, or `Ok(None)` when they never followed you. Unknown
/// follower or account -> 404.
pub fn remove_follower(
    store: &dyn Store,
    account_id: Id,
    follower_id: Id,
    now_ms: u64,
) -> Result<Option<Follow>> {
    require_account(store, account_id)?;
    require_account(store, follower_id)?;
    store.remove_follower(account_id, follower_id, now_ms)
}

/// Block an account (`POST /api/v1/accounts/:id/block`): mutual unfollow,
/// rejection of any pending inbound request, and endorsement cleanup happen
/// inside the store's single transaction.
///
/// Returns `Ok(None)` when the block already existed — including the
/// self-block case, which is a no-op rather than an error. Unknown follower
/// or target -> 404.
pub fn block(
    store: &dyn Store,
    account_id: Id,
    target_id: Id,
    now_ms: u64,
) -> Result<Option<Block>> {
    require_account(store, account_id)?;
    require_account(store, target_id)?;
    store.block(account_id, target_id, now_ms)
}

/// Unblock an account (no-op -> `Ok(None)`). Unknown follower or target ->
/// 404.
pub fn unblock(
    store: &dyn Store,
    account_id: Id,
    target_id: Id,
    now_ms: u64,
) -> Result<Option<Block>> {
    require_account(store, account_id)?;
    require_account(store, target_id)?;
    store.unblock(account_id, target_id, now_ms)
}

/// Mute an account (`POST /api/v1/accounts/:id/mute`).
///
/// `duration_seconds` is 0 for a permanent mute; `hide_notifications` is the
/// `notifications` flag inverted (nil -> true, i.e. notifications are hidden
/// unless explicitly kept). Unknown follower or target -> 404.
pub fn mute(
    store: &dyn Store,
    account_id: Id,
    target_id: Id,
    duration_seconds: u64,
    hide_notifications: bool,
    now_ms: u64,
) -> Result<Mute> {
    require_account(store, account_id)?;
    require_account(store, target_id)?;
    store.mute(
        account_id,
        target_id,
        duration_seconds,
        hide_notifications,
        now_ms,
    )
}

/// Unmute an account (no-op -> `Ok(None)`). Unknown follower or target -> 404.
pub fn unmute(
    store: &dyn Store,
    account_id: Id,
    target_id: Id,
    now_ms: u64,
) -> Result<Option<Mute>> {
    require_account(store, account_id)?;
    require_account(store, target_id)?;
    store.unmute(account_id, target_id, now_ms)
}

/// Set (or with a blank comment, delete) the viewer's private note on the
/// target (`POST /api/v1/accounts/relationships`).
///
/// Mastodon deletes on `params[:comment].blank?`, so a whitespace-only
/// comment is treated as empty. Longer than
/// `ACCOUNT_NOTE_MAX_LEN` graphemes -> 422
/// `Validation failed: Comment is too long (maximum is 2000 characters)`;
/// unknown target -> 404.
pub fn set_account_note(
    store: &dyn Store,
    viewer_id: Id,
    target_id: Id,
    comment: &str,
    now_ms: u64,
) -> Result<Option<AccountNote>> {
    require_account(store, target_id)?;
    // Blank deletes (Mastodon's `.blank?` covers whitespace-only input).
    let comment = if comment.trim().is_empty() { "" } else { comment };
    if comment.chars().count() > ACCOUNT_NOTE_MAX_LEN {
        return Err(Error::InvalidInput(format!(
            "Validation failed: Comment is too long (maximum is {ACCOUNT_NOTE_MAX_LEN} characters)"
        )));
    }
    store.set_account_note(viewer_id, target_id, comment, now_ms)
}

/// The 422 message for an input that `normalize_domain` rejects: upstream's
/// `AccountDomainBlock` presence and `DomainValidator` messages, as the
/// `RecordInvalid` error renders them.
fn invalid_domain_message(raw: &str) -> &'static str {
    let stripped = raw.trim().trim_start_matches('@').trim();
    if stripped.is_empty() {
        "Validation failed: Domain can't be blank"
    } else {
        "Validation failed: Domain is invalid"
    }
}

/// Block a domain (`POST /api/v1/domain_blocks`).
///
/// The domain is normalized (trim, strip one leading `@`, lowercase) before
/// it is stored. Blank input -> 422 `Validation failed: Domain can't be
/// blank`; malformed input (scheme, port, path, spaces) -> 422 `Validation
/// failed: Domain is invalid`.
pub fn domain_block(
    store: &dyn Store,
    account_id: Id,
    domain: &str,
    now_ms: u64,
) -> Result<DomainBlock> {
    let domain = normalize_domain(domain)
        .ok_or_else(|| Error::InvalidInput(invalid_domain_message(domain).into()))?;
    store.domain_block(account_id, &domain, now_ms)
}

/// Unblock a domain (`DELETE /api/v1/domain_blocks`), returning the removed
/// row (`Ok(None)` when it was not blocked).
///
/// The input is normalized exactly like `domain_block`; a domain that fails
/// normalization can never have been stored, so there is nothing to remove.
pub fn unblock_domain(store: &dyn Store, account_id: Id, domain: &str) -> Result<Option<DomainBlock>> {
    let Some(domain) = normalize_domain(domain) else {
        return Ok(None);
    };
    store.unblock_domain(account_id, &domain)
}

/// Blocked domains, newest first, paged by row id (`GET /api/v1/domain_blocks`).
pub fn list_domain_blocks(
    store: &dyn Store,
    account_id: Id,
    max_id: Option<Id>,
    since_id: Option<Id>,
    limit: usize,
) -> Result<Vec<DomainBlock>> {
    store.domain_blocks(account_id, max_id, since_id, limit)
}

/// Endorse (pin) an account you follow (`POST /api/v1/accounts/:id/pin`).
///
/// Unknown target -> 404; the store enforces the "must already follow"
/// requirement and its `InvalidInput` propagates as-is.
pub fn endorse(store: &dyn Store, account_id: Id, target_id: Id, now_ms: u64) -> Result<Endorsement> {
    require_account(store, target_id)?;
    store.endorse(account_id, target_id, now_ms)
}

/// Remove an endorsement (no-op -> `Ok(None)`). Unknown follower or target ->
/// 404.
pub fn unendorse(
    store: &dyn Store,
    account_id: Id,
    target_id: Id,
) -> Result<Option<Endorsement>> {
    require_account(store, account_id)?;
    require_account(store, target_id)?;
    store.unendorse(account_id, target_id)
}

/// Endorsed account ids, newest first, cursor-bounded (`GET
/// /api/v1/endorsements`).
pub fn list_endorsements(
    store: &dyn Store,
    account_id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Id>> {
    store.endorsements(account_id, max_id, limit)
}

/// Pending inbound follow requests, newest first (`GET
/// /api/v1/follow_requests`).
pub fn list_follow_requests(
    store: &dyn Store,
    target_id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Follow>> {
    store.follow_requests(target_id, max_id, limit)
}

/// Accept a pending follow request (`POST /api/v1/follow_requests/:id/authorize`):
/// the request becomes an active follow. Unknown follower or target -> 404;
/// a missing request is `NotFound` from the store.
pub fn authorize_request(
    store: &dyn Store,
    target_id: Id,
    follower_id: Id,
    now_ms: u64,
) -> Result<Follow> {
    require_account(store, target_id)?;
    require_account(store, follower_id)?;
    store.authorize_follow_request(target_id, follower_id, now_ms)
}

/// Reject a pending follow request (`POST /api/v1/follow_requests/:id/reject`),
/// returning the dropped request (`Ok(None)` when there was none). Unknown
/// follower or target -> 404.
pub fn reject_request(
    store: &dyn Store,
    target_id: Id,
    follower_id: Id,
    now_ms: u64,
) -> Result<Option<Follow>> {
    require_account(store, target_id)?;
    require_account(store, follower_id)?;
    store.reject_follow_request(target_id, follower_id, now_ms)
}

/// Followers of `account_id`, newest first, cursor-bounded (`GET
/// /api/v1/accounts/:id/followers`).
pub fn followers_of(
    store: &dyn Store,
    account_id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Id>> {
    store.followers(account_id, max_id, limit)
}

/// Accounts followed by `account_id`, newest first, cursor-bounded (`GET
/// /api/v1/accounts/:id/following`).
pub fn following_of(
    store: &dyn Store,
    account_id: Id,
    max_id: Option<Id>,
    limit: usize,
) -> Result<Vec<Id>> {
    store.following(account_id, max_id, limit)
}

/// Relationship aggregates for the given target ids, in input order; unknown
/// targets are silently skipped (Mastodon skips deleted/suspended).
pub fn relationships_for(
    store: &dyn Store,
    viewer_id: Id,
    target_ids: &[Id],
    now_ms: u64,
) -> Result<Vec<Relationship>> {
    let mut targets: Vec<(Id, String)> = Vec::with_capacity(target_ids.len());
    for &id in target_ids {
        if let Some(account) = store.account_by_id(id)? {
            targets.push((id, account.domain));
        }
    }
    store.relationships(viewer_id, &targets, now_ms)
}

/// Single relationship (for write-endpoint responses). None when target missing.
pub fn relationship_for(
    store: &dyn Store,
    viewer_id: Id,
    target_id: Id,
    now_ms: u64,
) -> Result<Option<Relationship>> {
    let Some(target) = store.account_by_id(target_id)? else {
        return Ok(None);
    };
    let relationships = store.relationships(viewer_id, &[(target_id, target.domain)], now_ms)?;
    Ok(relationships.into_iter().next())
}

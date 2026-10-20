//! Announcements: reads, dismissals, reactions and admin authoring.
//!
//! Canonical home: `crate::application::announcements` (the parent wires
//! `pub mod announcements;` into `application/mod.rs` and swaps the
//! transitional backing described below; until then this file is compiled via
//! a `#[path]` shim in `endpoints/announcements.rs` — see the parent snippets
//! at the bottom of this file).
//!
//! Upstream pin (`8ff11c2b`):
//! - `app/controllers/api/v1/announcements_controller.rb:1-22` — `index`
//!   renders published announcements chronological, no scope check (only
//!   `require_user!`); `dismiss` (`write`+`write:accounts`) does
//!   `find_or_create_by!` on the mute and `render_empty` (`200 {}`, per
//!   `app/controllers/api/base_controller.rb:86-88`).
//! - `app/controllers/api/v1/announcements/reactions_controller.rb:1-28` —
//!   `update`/`destroy` (`write`+`write:favourites`) on
//!   `Announcement.published`, `render_empty` (`200 {}`); no feature flag
//!   gates either action at the pin (verified by grep over
//!   `app/controllers/api/v1/`).
//! - `app/models/announcement.rb` — `published`/`chronological` scopes
//!   (`COALESCE(starts_at, scheduled_at, published_at, created_at)` ascending),
//!   `set_published` on create, `publish!`/`unpublish!`, `reactions(account)`
//!   grouped by name with a `me` column and custom-emoji preload.
//! - `app/validators/reaction_validator.rb` — name must be unicode (from
//!   `emoji_map.json`) or a local enabled custom emoji; at most 8 distinct
//!   names per announcement.
//! - `app/serializers/rest/announcement_serializer.rb` — `id, content,
//!   starts_at, ends_at, all_day, published_at, updated_at`, `read` (when
//!   signed in), `mentions` (`id, username, url, acct`), `statuses`, `tags`
//!   (`name, url` via `ShallowTagSerializer`), `emojis`, `reactions` (`name,
//!   count`, `me` when signed in, `url`/`static_url` when custom).
//! - `app/controllers/admin/announcements_controller.rb` — HTML admin UI
//!   (`index/new/create/edit/update/publish/unpublish/destroy`,
//!   `resource_params`: `text, scheduled_at, starts_at, ends_at, all_day`);
//!   `app/models/user_role.rb` + `config/roles.yml` + `app/policies/
//!   announcement_policy.rb` — `manage_announcements` is administration-only
//!   (Admin/Owner roles; Moderator lacks it), so the API authoring surface
//!   below gates on `AdminArea::AdminOnly`.
//!
//! ## Transitional backing
//!
//! The `Store`/`redb` extension is parent-owned, so this module defines the
//! proposed [`AnnouncementStore`] trait plus every use case against it, and
//! ships a process-local [`MemoryAnnouncements`] implementing it. Endpoints
//! use one `MemoryAnnouncements` per `Arc<dyn Store>` identity until the
//! parent lands the `redb` tables and swaps the source (one line per handler).
//! The in-memory rows are NOT durable across restarts — the parent's wiring
//! snippet (below) replaces them with durable tables.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::domain::{Error, Id, Result};

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// One announcement row (mirrors upstream's `announcements` table columns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnouncementRecord {
    pub id: Id,
    pub text: String,
    pub all_day: bool,
    pub starts_at_ms: Option<u64>,
    pub ends_at_ms: Option<u64>,
    /// `None` = draft/scheduled, never publicly listed.
    pub published: bool,
    pub published_at_ms: Option<u64>,
    pub scheduled_at_ms: Option<u64>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// One reaction row (mirrors `announcement_reactions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnouncementReaction {
    pub id: Id,
    pub announcement_id: Id,
    pub account_id: Id,
    pub name: String,
    /// Shortcode when the name resolved to a local enabled custom emoji.
    pub custom_emoji_shortcode: Option<String>,
    pub created_at_ms: u64,
}

/// Grouped reaction for the serializer (`name, count, me` + custom-emoji key).
#[derive(Debug, Clone)]
pub struct ReactionView {
    pub name: String,
    pub count: u64,
    pub me: bool,
    pub custom_emoji_shortcode: Option<String>,
}

/// Input for admin create (mirrors upstream `resource_params`).
#[derive(Debug, Clone, Default)]
pub struct NewAnnouncement {
    pub text: String,
    pub all_day: bool,
    pub starts_at_ms: Option<u64>,
    pub ends_at_ms: Option<u64>,
    pub scheduled_at_ms: Option<u64>,
}

/// Field-level admin patch (`None` = unchanged; `Some(None)` clears).
#[derive(Debug, Clone, Default)]
pub struct AnnouncementPatch {
    pub text: Option<String>,
    pub all_day: Option<bool>,
    pub starts_at_ms: Option<Option<u64>>,
    pub ends_at_ms: Option<Option<u64>>,
    pub scheduled_at_ms: Option<Option<u64>>,
}

// ---------------------------------------------------------------------------
// Proposed Store extension (parent-owned snippet at the bottom of this file).
// ---------------------------------------------------------------------------

/// Minimal persistence surface the parent adds to `Store` + `redb`.
pub trait AnnouncementStore: Send + Sync {
    fn announcement_by_id(&self, id: Id) -> Result<Option<AnnouncementRecord>>;
    /// Every row, oldest id first (callers sort/filter; bounded in `redb`).
    fn all_announcements(&self, cap: usize) -> Result<Vec<AnnouncementRecord>>;
    fn insert_announcement(&self, record: AnnouncementRecord) -> Result<AnnouncementRecord>;
    fn update_announcement(&self, record: AnnouncementRecord) -> Result<AnnouncementRecord>;
    fn delete_announcement(&self, id: Id) -> Result<bool>;
    /// Idempotent mute insert (upstream `find_or_create_by!`).
    fn mute_announcement(&self, announcement_id: Id, account_id: Id, now_ms: u64) -> Result<()>;
    fn announcement_muted(&self, announcement_id: Id, account_id: Id) -> Result<bool>;
    /// Insert a reaction row; the use case enforces idempotency/validity.
    fn insert_reaction(&self, reaction: AnnouncementReaction) -> Result<AnnouncementReaction>;
    /// The viewer's reaction with `name` on `announcement_id`, if any.
    fn viewer_reaction(
        &self,
        announcement_id: Id,
        account_id: Id,
        name: &str,
    ) -> Result<Option<AnnouncementReaction>>;
    /// Every reaction row on `announcement_id`, oldest first.
    fn reactions_of(&self, announcement_id: Id) -> Result<Vec<AnnouncementReaction>>;
    /// Delete one reaction row by id; `false` when already gone.
    fn delete_reaction(&self, id: Id) -> Result<bool>;
}

// ---------------------------------------------------------------------------
// Pure use cases (generic over `&dyn AnnouncementStore`).
// ---------------------------------------------------------------------------

/// Upstream `Announcement.published.chronological`: published rows ordered by
/// `COALESCE(starts_at, scheduled_at, published_at, created_at)` ascending
/// (`app/models/announcement.rb:12-13,28-36` at the pin).
pub fn list_published(store: &dyn AnnouncementStore) -> Result<Vec<AnnouncementRecord>> {
    let mut rows: Vec<_> = all(store)?
        .into_iter()
        .filter(|a| a.published)
        .collect();
    rows.sort_by_key(chronology_key);
    Ok(rows)
}

/// Admin index: every row (published and drafts), newest chronology first
/// (mirrors upstream admin `reverse_chronological`).
pub fn list_all(store: &dyn AnnouncementStore) -> Result<Vec<AnnouncementRecord>> {
    let mut rows = all(store)?;
    rows.sort_by_key(chronology_key);
    rows.reverse();
    Ok(rows)
}

/// Upstream `dismiss`: 404 unless the announcement is published; otherwise an
/// idempotent mute. Returns the muted row.
pub fn dismiss(
    store: &dyn AnnouncementStore,
    announcement_id: Id,
    account_id: Id,
    now_ms: u64,
) -> Result<AnnouncementRecord> {
    let row = published_or_not_found(store, announcement_id)?;
    store.mute_announcement(announcement_id, account_id, now_ms)?;
    Ok(row)
}

/// Upstream reactions `update`: 404 unless published; validates the name
/// (ReactionValidator); idempotent per (viewer, name) — a repeated PUT keeps
/// a single row (upstream `create!` would duplicate; PUT idempotency wins).
pub fn add_reaction(
    store: &dyn AnnouncementStore,
    announcement_id: Id,
    account_id: Id,
    name: &str,
    custom_emoji_shortcode: Option<&str>,
    now_ms: u64,
    next_id: Id,
) -> Result<AnnouncementReaction> {
    let row = published_or_not_found(store, announcement_id)?;
    validate_reaction_name(store, &row, name, custom_emoji_shortcode)?;
    if let Some(existing) = store.viewer_reaction(announcement_id, account_id, name)? {
        return Ok(existing);
    }
    store.insert_reaction(AnnouncementReaction {
        id: next_id,
        announcement_id,
        account_id,
        name: name.to_string(),
        custom_emoji_shortcode: custom_emoji_shortcode.map(str::to_string),
        created_at_ms: now_ms,
    })
}

/// Upstream reactions `destroy`: 404 unless published AND the viewer reacted
/// with `name` (`find_by!`).
pub fn remove_reaction(
    store: &dyn AnnouncementStore,
    announcement_id: Id,
    account_id: Id,
    name: &str,
) -> Result<()> {
    published_or_not_found(store, announcement_id)?;
    match store.viewer_reaction(announcement_id, account_id, name)? {
        Some(existing) => {
            store.delete_reaction(existing.id)?;
            Ok(())
        }
        None => Err(Error::NotFound("reaction".into())),
    }
}

/// Grouped reactions for one announcement with the viewer's `me` flags
/// (mirrors `Announcement#reactions(account)`: group by name, order by first
/// reaction, custom-emoji preload). Pure grouping: callers enforce the
/// published scope (admin views render unpublished rows too).
pub fn reaction_views(
    store: &dyn AnnouncementStore,
    announcement_id: Id,
    viewer_id: Id,
) -> Result<Vec<ReactionView>> {
    let rows = store.reactions_of(announcement_id)?;
    // First-seen order per name (upstream `MIN(created_at)` ascending).
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<&str, u64> = HashMap::new();
    let mut custom: HashMap<&str, Option<String>> = HashMap::new();
    let mut mine: HashSet<&str> = HashSet::new();
    for row in &rows {
        if !counts.contains_key(row.name.as_str()) {
            order.push(row.name.clone());
            custom.insert(
                row.name.as_str(),
                row.custom_emoji_shortcode.clone(),
            );
        }
        *counts.entry(row.name.as_str()).or_insert(0) += 1;
        if row.account_id == viewer_id {
            mine.insert(row.name.as_str());
        }
    }
    Ok(order
        .into_iter()
        .map(|name| ReactionView {
            count: counts.get(name.as_str()).copied().unwrap_or(0),
            me: mine.contains(name.as_str()),
            custom_emoji_shortcode: custom
                .get(name.as_str())
                .cloned()
                .flatten(),
            name,
        })
        .collect())
}

/// Admin create: upstream validations (`text` presence; `starts_at` iff
/// `ends_at`) plus `set_published` on create (blank/past `scheduled_at` →
/// published immediately with `published_at = now`).
pub fn create_announcement(
    store: &dyn AnnouncementStore,
    input: NewAnnouncement,
    now_ms: u64,
    id: Id,
) -> Result<AnnouncementRecord> {
    validate_text(&input.text)?;
    validate_window(input.starts_at_ms, input.ends_at_ms)?;
    let published = match input.scheduled_at_ms {
        None => true,
        Some(at) => at <= now_ms,
    };
    store.insert_announcement(AnnouncementRecord {
        id,
        text: input.text,
        all_day: input.all_day,
        starts_at_ms: input.starts_at_ms,
        ends_at_ms: input.ends_at_ms,
        published,
        published_at_ms: published.then_some(now_ms),
        scheduled_at_ms: input.scheduled_at_ms,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    })
}

/// Admin update: same validations; publishing state only changes via
/// `publish`/`unpublish` (mirrors upstream, where `update` never flips
/// `published` by itself).
pub fn update_announcement(
    store: &dyn AnnouncementStore,
    id: Id,
    patch: AnnouncementPatch,
    now_ms: u64,
) -> Result<AnnouncementRecord> {
    let mut row = store
        .announcement_by_id(id)?
        .ok_or_else(|| Error::NotFound("announcement".into()))?;
    if let Some(text) = patch.text {
        validate_text(&text)?;
        row.text = text;
    }
    if let Some(all_day) = patch.all_day {
        row.all_day = all_day;
    }
    if let Some(starts_at_ms) = patch.starts_at_ms {
        row.starts_at_ms = starts_at_ms;
    }
    if let Some(ends_at_ms) = patch.ends_at_ms {
        row.ends_at_ms = ends_at_ms;
    }
    if let Some(scheduled_at_ms) = patch.scheduled_at_ms {
        row.scheduled_at_ms = scheduled_at_ms;
    }
    validate_window(row.starts_at_ms, row.ends_at_ms)?;
    row.updated_at_ms = now_ms;
    store.update_announcement(row)
}

/// Admin `publish!`: published with `published_at = now`, schedule cleared.
pub fn publish_announcement(
    store: &dyn AnnouncementStore,
    id: Id,
    now_ms: u64,
) -> Result<AnnouncementRecord> {
    let mut row = store
        .announcement_by_id(id)?
        .ok_or_else(|| Error::NotFound("announcement".into()))?;
    row.published = true;
    row.published_at_ms = Some(now_ms);
    row.scheduled_at_ms = None;
    row.updated_at_ms = now_ms;
    store.update_announcement(row)
}

/// Admin `unpublish!`.
pub fn unpublish_announcement(
    store: &dyn AnnouncementStore,
    id: Id,
    now_ms: u64,
) -> Result<AnnouncementRecord> {
    let mut row = store
        .announcement_by_id(id)?
        .ok_or_else(|| Error::NotFound("announcement".into()))?;
    row.published = false;
    row.scheduled_at_ms = None;
    row.updated_at_ms = now_ms;
    store.update_announcement(row)
}

/// Admin `destroy`.
pub fn destroy_announcement(store: &dyn AnnouncementStore, id: Id) -> Result<()> {
    if store.delete_announcement(id)? {
        Ok(())
    } else {
        Err(Error::NotFound("announcement".into()))
    }
}

fn all(store: &dyn AnnouncementStore) -> Result<Vec<AnnouncementRecord>> {
    store.all_announcements(10_000)
}

fn chronology_key(a: &AnnouncementRecord) -> u64 {
    a.starts_at_ms
        .or(a.scheduled_at_ms)
        .or(a.published_at_ms)
        .unwrap_or(a.created_at_ms)
}

fn published_or_not_found(
    store: &dyn AnnouncementStore,
    id: Id,
) -> Result<AnnouncementRecord> {
    match store.announcement_by_id(id)? {
        Some(row) if row.published => Ok(row),
        _ => Err(Error::NotFound("announcement".into())),
    }
}

fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(Error::InvalidInput("Text can't be blank".into()));
    }
    Ok(())
}

fn validate_window(starts_at_ms: Option<u64>, ends_at_ms: Option<u64>) -> Result<()> {
    match (starts_at_ms, ends_at_ms) {
        (Some(_), None) => Err(Error::InvalidInput("Ends at can't be blank".into())),
        (None, Some(_)) => Err(Error::InvalidInput("Starts at can't be blank".into())),
        _ => Ok(()),
    }
}

/// Mirrors `ReactionValidator` (`app/validators/reaction_validator.rb` at the
/// pin): the name must be a known custom emoji or a unicode emoji, and an
/// announcement holds at most 8 distinct names (`LIMIT = 8`).
///
/// Divergence: without upstream's `emoji_map.json` asset this server accepts
/// any non-shortcode-shaped name as unicode. A shortcode-shaped name
/// (`[A-Za-z0-9_]{2,}`) with no matching local enabled custom emoji is
/// refused, mirroring the validator's structure.
fn validate_reaction_name(
    store: &dyn AnnouncementStore,
    row: &AnnouncementRecord,
    name: &str,
    custom_emoji_shortcode: Option<&str>,
) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidInput("Name is not a recognized emoji".into()));
    }
    if custom_emoji_shortcode.is_none() && is_shortcode_shaped(name) {
        return Err(Error::InvalidInput("Name is not a recognized emoji".into()));
    }
    let distinct = {
        let mut names = HashSet::new();
        for reaction in store.reactions_of(row.id)? {
            names.insert(reaction.name);
        }
        names
    };
    if !distinct.contains(name) && distinct.len() >= 8 {
        return Err(Error::InvalidInput(
            "Limit of different reactions reached".into(),
        ));
    }
    Ok(())
}

/// Upstream shortcode shape (`CustomEmoji::SHORTCODE_RE_FRAGMENT`).
fn is_shortcode_shaped(name: &str) -> bool {
    let len = name.chars().count();
    (2..=128).contains(&len)
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Transitional in-memory backing (replaced by the parent's `redb` wiring).
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct MemoryInner {
    announcements: HashMap<Id, AnnouncementRecord>,
    mutes: HashSet<(Id, Id)>,
    reactions: HashMap<Id, AnnouncementReaction>,
}

/// Process-local [`AnnouncementStore`] until the parent lands durable tables.
#[derive(Debug, Default)]
pub struct MemoryAnnouncements {
    inner: Mutex<MemoryInner>,
}

impl MemoryAnnouncements {
    pub fn new() -> Self {
        Self::default()
    }
}

impl AnnouncementStore for MemoryAnnouncements {
    fn announcement_by_id(&self, id: Id) -> Result<Option<AnnouncementRecord>> {
        Ok(self.inner.lock().expect("lock").announcements.get(&id).cloned())
    }

    fn all_announcements(&self, cap: usize) -> Result<Vec<AnnouncementRecord>> {
        let inner = self.inner.lock().expect("lock");
        let mut rows: Vec<_> = inner.announcements.values().cloned().collect();
        rows.sort_by_key(|a| a.id);
        rows.truncate(cap);
        Ok(rows)
    }

    fn insert_announcement(&self, record: AnnouncementRecord) -> Result<AnnouncementRecord> {
        self.inner
            .lock()
            .expect("lock")
            .announcements
            .insert(record.id, record.clone());
        Ok(record)
    }

    fn update_announcement(&self, record: AnnouncementRecord) -> Result<AnnouncementRecord> {
        let mut inner = self.inner.lock().expect("lock");
        if !inner.announcements.contains_key(&record.id) {
            return Err(Error::NotFound("announcement".into()));
        }
        inner.announcements.insert(record.id, record.clone());
        Ok(record)
    }

    fn delete_announcement(&self, id: Id) -> Result<bool> {
        let mut inner = self.inner.lock().expect("lock");
        let removed = inner.announcements.remove(&id).is_some();
        inner.mutes.retain(|(aid, _)| *aid != id);
        inner.reactions.retain(|_, r| r.announcement_id != id);
        Ok(removed)
    }

    fn mute_announcement(
        &self,
        announcement_id: Id,
        account_id: Id,
        _now_ms: u64,
    ) -> Result<()> {
        self.inner
            .lock()
            .expect("lock")
            .mutes
            .insert((announcement_id, account_id));
        Ok(())
    }

    fn announcement_muted(&self, announcement_id: Id, account_id: Id) -> Result<bool> {
        Ok(self
            .inner
            .lock()
            .expect("lock")
            .mutes
            .contains(&(announcement_id, account_id)))
    }

    fn insert_reaction(&self, reaction: AnnouncementReaction) -> Result<AnnouncementReaction> {
        self.inner
            .lock()
            .expect("lock")
            .reactions
            .insert(reaction.id, reaction.clone());
        Ok(reaction)
    }

    fn viewer_reaction(
        &self,
        announcement_id: Id,
        account_id: Id,
        name: &str,
    ) -> Result<Option<AnnouncementReaction>> {
        Ok(self
            .inner
            .lock()
            .expect("lock")
            .reactions
            .values()
            .find(|r| {
                r.announcement_id == announcement_id
                    && r.account_id == account_id
                    && r.name == name
            })
            .cloned())
    }

    fn reactions_of(&self, announcement_id: Id) -> Result<Vec<AnnouncementReaction>> {
        let inner = self.inner.lock().expect("lock");
        let mut rows: Vec<_> = inner
            .reactions
            .values()
            .filter(|r| r.announcement_id == announcement_id)
            .cloned()
            .collect();
        rows.sort_by_key(|r| (r.created_at_ms, r.id));
        Ok(rows)
    }

    fn delete_reaction(&self, id: Id) -> Result<bool> {
        Ok(self.inner.lock().expect("lock").reactions.remove(&id).is_some())
    }
}

// ---------------------------------------------------------------------------
// Parent-owned wiring snippets (do NOT apply here — owner: parent/integrator).
// ---------------------------------------------------------------------------
//
// 1. `src/application/ports.rs` — add to the `Store` supertrait list (both the
//    trait bound and the blanket impl):
//
//    ```ignore
//    + crate::application::announcements::AnnouncementStore
//    ```
//
//    and re-export the record types where `ports.rs` re-exports domain types:
//    `pub use crate::application::announcements::{AnnouncementRecord,
//    AnnouncementReaction, AnnouncementPatch, NewAnnouncement};`
//
// 2. `src/application/mod.rs` — `pub mod announcements;`, and remove the
//    `#[path]` shim at the top of `endpoints/announcements.rs`.
//
// 3. `redb` schema (new tables; all values `serde_json`, keys big-endian `u64`
//    of `Id::value()` unless noted):
//    - `announcements`: id -> `AnnouncementRecord`
//    - `announcement_mutes`: `(announcement_id, account_id)` 16-byte key -> `()`
//    - `announcement_reactions`: reaction id -> `AnnouncementReaction`
//    - `announcement_reactions_by_announcement`: `(announcement_id,
//      created_at_ms, reaction_id)` 24-byte key -> `()` (for `reactions_of`)
//    - `announcement_reactions_by_viewer`: `(announcement_id, account_id,
//      name_hash_or_bytes)` -> reaction id (for `viewer_reaction`)
//    All mutating methods commit record + index + mute/reaction rows in ONE
//    `redb` transaction (no jobs: announcements carry no fan-out here;
//    scheduled publishing enqueues `PublishScheduledAnnouncement` when
//    `scheduled_at_ms` is in the future).
//
// 4. `src/api/mastodon/router.rs` — mount the admin handlers already defined
//    in `endpoints/announcements.rs` (same shapes as the test router in
//    `tests/fix4_announcements.rs`):
//
//    ```ignore
//    .route("/api/v1/admin/announcements",
//        get(endpoints::announcements::admin_index)
//            .post(endpoints::announcements::admin_create))
//    .route("/api/v1/admin/announcements/:id",
//        get(endpoints::announcements::admin_show)
//            .put(endpoints::announcements::admin_update)
//            .patch(endpoints::announcements::admin_update)
//            .delete(endpoints::announcements::admin_destroy))
//    .route("/api/v1/admin/announcements/:id/publish",
//        post(endpoints::announcements::admin_publish))
//    .route("/api/v1/admin/announcements/:id/unpublish",
//        post(endpoints::announcements::admin_unpublish))
//    ```
//
//    Admin gate: `AdminArea::AdminOnly` + `admin:read` / `admin:write` (upstream
//    `manage_announcements` is administration-only: Admin/Owner, never
//    Moderator — `config/roles.yml` at the pin).
//
// 5. Endpoint swap: replace `db_for(&state.context.store)` with
//    `&*state.context.store as &dyn AnnouncementStore` once `Store`
//    supertraits it, and delete the `ANN` registry.
//
// 6. `docs/endpoint-manifest.md` — update the §Announcements rows: reads serve
//    real rows; dismiss/react answer `200 {}`; admin surface is new (no
//    upstream REST equivalent — the upstream authoring UI is HTML).

#[cfg(test)]
mod tests {
    use super::*;

    fn canned(published: bool, id_value: u64, chrono_ms: u64) -> AnnouncementRecord {
        AnnouncementRecord {
            id: Id::new(id_value),
            text: "hello".into(),
            all_day: false,
            starts_at_ms: None,
            ends_at_ms: None,
            published,
            published_at_ms: published.then_some(chrono_ms),
            scheduled_at_ms: None,
            created_at_ms: chrono_ms,
            updated_at_ms: chrono_ms,
        }
    }

    #[test]
    fn published_list_is_chronological() {
        let db = MemoryAnnouncements::new();
        for (id, chrono) in [(3, 300), (1, 100), (2, 200)] {
            db.insert_announcement(canned(true, id, chrono)).unwrap();
        }
        db.insert_announcement(canned(false, 4, 50)).unwrap();
        let rows = list_published(&db).unwrap();
        let ids: Vec<u64> = rows.iter().map(|r| r.id.value()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn admin_list_includes_drafts_newest_first() {
        let db = MemoryAnnouncements::new();
        db.insert_announcement(canned(true, 1, 100)).unwrap();
        db.insert_announcement(canned(false, 2, 400)).unwrap();
        let rows = list_all(&db).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id.value(), 2);
    }

    #[test]
    fn dismiss_requires_published() {
        let db = MemoryAnnouncements::new();
        db.insert_announcement(canned(false, 1, 100)).unwrap();
        assert!(dismiss(&db, Id::new(1), Id::new(9), 1000).is_err());
        assert!(dismiss(&db, Id::new(404), Id::new(9), 1000).is_err());
    }

    #[test]
    fn create_validates_text_and_window() {
        let db = MemoryAnnouncements::new();
        let blank = NewAnnouncement { text: "  ".into(), ..Default::default() };
        assert!(create_announcement(&db, blank, 1000, Id::new(1)).is_err());
        let half_window = NewAnnouncement {
            text: "x".into(),
            starts_at_ms: Some(100),
            ..Default::default()
        };
        assert!(create_announcement(&db, half_window, 1000, Id::new(1)).is_err());
    }

    #[test]
    fn create_publishes_immediately_without_future_schedule() {
        let db = MemoryAnnouncements::new();
        let row = create_announcement(
            &db,
            NewAnnouncement { text: "now".into(), ..Default::default() },
            1000,
            Id::new(1),
        )
        .unwrap();
        assert!(row.published);
        assert_eq!(row.published_at_ms, Some(1000));
        let future = create_announcement(
            &db,
            NewAnnouncement {
                text: "later".into(),
                scheduled_at_ms: Some(2000),
                ..Default::default()
            },
            1000,
            Id::new(2),
        )
        .unwrap();
        assert!(!future.published);
    }

    #[test]
    fn reaction_limit_is_eight_distinct_names() {
        let db = MemoryAnnouncements::new();
        db.insert_announcement(canned(true, 1, 100)).unwrap();
        for i in 0..8 {
            add_reaction(
                &db,
                Id::new(1),
                Id::new(10 + i),
                &format!("e{i}"),
                Some(&format!("e{i}")),
                1000,
                Id::new(100 + i),
            )
            .unwrap();
        }
        let ninth = add_reaction(
            &db,
            Id::new(1),
            Id::new(99),
            "zz",
            Some("zz"),
            1000,
            Id::new(200),
        );
        assert!(ninth.is_err());
        // An existing name still reacts fine for a new viewer.
        assert!(
            add_reaction(&db, Id::new(1), Id::new(99), "e0", Some("e0"), 1000, Id::new(201))
                .is_ok()
        );
    }

    #[test]
    fn shortcode_shaped_unknown_name_is_refused() {
        let db = MemoryAnnouncements::new();
        db.insert_announcement(canned(true, 1, 100)).unwrap();
        assert!(
            add_reaction(&db, Id::new(1), Id::new(9), "nope_missing", None, 1000, Id::new(5))
                .is_err()
        );
        // Unicode names pass without a custom emoji.
        assert!(
            add_reaction(&db, Id::new(1), Id::new(9), "🎉", None, 1000, Id::new(6)).is_ok()
        );
    }
}

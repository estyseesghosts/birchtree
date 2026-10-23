//! Batched page loader for status views (S10).
//!
//! [`StatusViewBatch`] snapshots every row `super::status_view::StatusViewContext::prefetch`
//! loads for a page, lookup-for-lookup, so the cutover replaces per-status (N+1) loads with one
//! batched snapshot returning identical rows. It is pure data: no store handle, no clock, no
//! per-request state beyond what the loaders already key on.
//!
//! Lookup parity with `prefetch` (same tables, same bounds, same order semantics):
//!
//! | Map | Store loader | Bound / order rule |
//! |---|---|---|
//! | `statuses` (roots) | `statuses_by_ids` | non-deleted rows among the ids, id-descending |
//! | `statuses` (closure) | `statuses_by_ids` | missing reblog targets, then missing quoted statuses, per round up to [`MAX_PREFETCH_ROUNDS`] |
//! | `quotes` | `quotes_by_quoting_ids` | every unprobed status id, once per page (a round never repeats an id) |
//! | `accounts` | `accounts_by_ids` | authors plus mentioned accounts, sorted and de-duplicated |
//! | `stats` | `account_stats_by_ids` | distinct authors only, sorted and de-duplicated |
//! | `media` | `media_by_ids` | every attachment id on the page, sorted and de-duplicated |
//! | `remote_sources` | `remote_media_sources_by_ids` | same id set as `media`; skipped when empty; empty-string sources dropped (local media) |
//! | `polls` | `polls_by_ids` | every poll id on the page, sorted and de-duplicated |
//! | `places` | `places_by_ids` | every place id on the page, sorted and de-duplicated; skipped when empty |
//! | `ballots` | `ballots` | the page's poll ids for `viewer`; viewer-gated |
//! | `muted_by_status` | `conversations::muted_map_for_statuses` | whole page in one call; viewer-gated |
//! | `flags` | `viewer_status_flags` | every row id plus every proper (reblog-target) id, sorted and de-duplicated; viewer-gated |
//! | `pairs` | `batch_pair_states` | every account id on the page, both directions; viewer-gated |
//! | `emoji_entities` | `emojis_by_shortcodes` | shortcodes from `source_text` plus `spoiler_text`, sorted and de-duplicated; skipped when empty |
//!
//! Ordering: assembled views iterate the caller's id slice in order (see
//! [`StatusViewBatch::ordered_statuses`]); the maps themselves are keyed lookups, exactly like
//! the `prefetch` caches, so view order is identical to today.
//!
//! Point-lookup fallbacks (chains beyond the round bound, remote emoji, remote-media source for
//! one attachment) stay in `super::status_view`; this batch only replaces the batched rounds.
//!
//! Rows compose `crate::api::mastodon::dto::StatusView`. S10 follow-up: swap that path to
//! `crate::api::mastodon::dto::status::StatusView` once the sibling `dto/` split lands; the
//! field types here (`Status`, `Account`, `Poll`, and friends) are the same domain rows either
//! path assembles from.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::api::mastodon::errors::ApiError;
use crate::application::conversations;
use crate::application::ports::{PairState, StatusViewFlags, Store};
use crate::application::statuses;
use crate::domain::{
    Account, AccountStats, CustomEmoji, Id, MediaAttachment, Place, Poll, QuoteRecord, Status,
};

/// Re-exported so the batch and the context share one round bound; chains
/// deeper than this fall back to point lookups, never to missing data.
use super::status_view::MAX_PREFETCH_ROUNDS;

/// Every batched row a page of status views needs, loaded once per page.
///
/// Field names mirror the `StatusViewContext` caches so the cutover is mechanical: `prefetch`
/// fills these maps, `view_for` reads them. See the module docs for the loader-to-map table.
pub struct StatusViewBatch {
    /// Every status known to the page: roots, reblog targets, quoted statuses.
    pub statuses: HashMap<Id, Status>,
    /// Authors plus mentioned accounts.
    pub accounts: HashMap<Id, Account>,
    /// Counters for the distinct authors.
    pub stats: HashMap<Id, AccountStats>,
    /// Quote records keyed by quoting status id.
    pub quotes: HashMap<Id, QuoteRecord>,
    /// Attachments in author order per status (map is unordered; order comes from the rows).
    pub media: HashMap<Id, MediaAttachment>,
    /// Stored remote origin URLs by media id (absent = local media; empty strings never stored).
    pub remote_sources: HashMap<Id, String>,
    /// Polls attached to the page's statuses.
    pub polls: HashMap<Id, Poll>,
    /// The viewer's ballot choices by poll id (viewer-gated; empty when anonymous).
    pub ballots: HashMap<Id, Vec<u64>>,
    /// Places attached to the page's statuses (dangling ids read as no place).
    pub places: HashMap<Id, Place>,
    /// Viewer flags keyed by row-exact status id (viewer-gated; empty when anonymous).
    pub flags: HashMap<Id, StatusViewFlags>,
    /// Conversation-mute flags keyed by row-exact status id (viewer-gated; empty when anonymous).
    pub muted_by_status: HashMap<Id, bool>,
    /// `(incoming, outgoing)` pair states keyed by the other account id (viewer-gated).
    pub pairs: HashMap<Id, (PairState, PairState)>,
    /// Local custom emoji referenced by the page's text, by shortcode.
    pub emoji_entities: HashMap<String, CustomEmoji>,
}

impl StatusViewBatch {
    /// Load a batch for raw status ids. Roots come from `statuses_by_ids` (non-deleted rows
    /// among `ids`, id-descending); the reblog/quote closure and every other map then load
    /// exactly as `prefetch` does today.
    pub fn load(
        store: &dyn Store,
        viewer: Option<Id>,
        ids: &[Id],
        now_ms: u64,
    ) -> Result<Self, ApiError> {
        let roots = store.statuses_by_ids(ids)?;
        Self::load_for_statuses(store, viewer, &roots, now_ms)
    }

    /// Load a batch for already-fetched root rows (the timeline/endpoint path, which holds
    /// `Status` rows before rendering). Mirrors `prefetch` lookup-for-lookup.
    pub fn load_for_statuses(
        store: &dyn Store,
        viewer: Option<Id>,
        roots: &[Status],
        now_ms: u64,
    ) -> Result<Self, ApiError> {
        let mut batch = Self::empty();
        for status in roots {
            batch.statuses.insert(status.id, status.clone());
        }
        batch.close_reblog_quote_rounds(store)?;
        batch.load_accounts_and_stats(store)?;
        batch.load_media_polls_places(store, viewer)?;
        batch.load_viewer_maps(store, viewer, now_ms)?;
        batch.load_emoji(store)?;
        Ok(batch)
    }

    /// Create an empty batch (maps fill as the loader stages run).
    pub(crate) fn empty() -> Self {
        Self {
            statuses: HashMap::new(),
            accounts: HashMap::new(),
            stats: HashMap::new(),
            quotes: HashMap::new(),
            media: HashMap::new(),
            remote_sources: HashMap::new(),
            polls: HashMap::new(),
            ballots: HashMap::new(),
            places: HashMap::new(),
            flags: HashMap::new(),
            muted_by_status: HashMap::new(),
            pairs: HashMap::new(),
            emoji_entities: HashMap::new(),
        }
    }

    /// Rows for `ids` in the caller's order, skipping ids with no row (deleted or never
    /// present). Assembling views over this order keeps page order identical to today.
    pub fn ordered_statuses(&self, ids: &[Id]) -> Vec<Status> {
        ids.iter()
            .filter_map(|id| self.statuses.get(id).cloned())
            .collect()
    }

    /// Number of statuses known to this batch (roots plus closure).
    pub fn len(&self) -> usize {
        self.statuses.len()
    }

    /// Whether the batch holds no statuses.
    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty()
    }

    /// Walk the reblog/quote closure in bounded rounds, one batched lookup per kind per round:
    /// missing reblog targets, then unprobed quote records, then missing quoted statuses.
    /// Stops at fixpoint; chains deeper than [`MAX_PREFETCH_ROUNDS`] fall back to point lookups
    /// at assembly time, exactly like `prefetch`.
    fn close_reblog_quote_rounds(&mut self, store: &dyn Store) -> Result<(), ApiError> {
        let mut quote_probed: HashSet<Id> = HashSet::new();
        for _ in 0..MAX_PREFETCH_ROUNDS {
            let mut changed = false;

            let missing_targets: Vec<Id> = self
                .statuses
                .values()
                .filter_map(|status| status.reblog_of_id)
                .filter(|id| !self.statuses.contains_key(id))
                .collect();
            if !missing_targets.is_empty() {
                changed = true;
                for target in store.statuses_by_ids(&missing_targets)? {
                    self.statuses.insert(target.id, target);
                }
            }

            let unprobed: Vec<Id> = self
                .statuses
                .keys()
                .copied()
                .filter(|id| !quote_probed.contains(id))
                .collect();
            if !unprobed.is_empty() {
                changed = true;
                let quotes = store.quotes_by_quoting_ids(&unprobed)?;
                for id in &unprobed {
                    quote_probed.insert(*id);
                }
                for (id, quote) in quotes {
                    self.quotes.insert(id, quote);
                }
            }

            let missing_quoted: Vec<Id> = self
                .quotes
                .values()
                .map(|quote| quote.quoted_status_id)
                .filter(|id| !self.statuses.contains_key(id))
                .collect();
            if !missing_quoted.is_empty() {
                changed = true;
                for status in store.statuses_by_ids(&missing_quoted)? {
                    self.statuses.insert(status.id, status);
                }
            }

            if !changed {
                break;
            }
        }
        Ok(())
    }

    /// Authors plus mentioned accounts (`accounts_by_ids`), then counters for the distinct
    /// authors (`account_stats_by_ids`). Both id sets are sorted and de-duplicated.
    fn load_accounts_and_stats(&mut self, store: &dyn Store) -> Result<(), ApiError> {
        let mut account_ids: Vec<Id> = Vec::new();
        for status in self.statuses.values() {
            account_ids.push(status.account_id);
            account_ids.extend(status.mentions.iter().copied());
        }
        self.accounts
            .extend(store.accounts_by_ids(&sorted_unique(account_ids))?);

        let authors: Vec<Id> = self.statuses.values().map(|s| s.account_id).collect();
        self.stats
            .extend(store.account_stats_by_ids(&sorted_unique(authors))?);
        Ok(())
    }

    /// Attachments (`media_by_ids`), remote origin URLs (`remote_media_sources_by_ids`), polls
    /// (`polls_by_ids`), places (`places_by_ids`) and the viewer's ballots (`ballots`).
    fn load_media_polls_places(
        &mut self,
        store: &dyn Store,
        viewer: Option<Id>,
    ) -> Result<(), ApiError> {
        let mut media_ids: Vec<Id> = Vec::new();
        let mut poll_ids: Vec<Id> = Vec::new();
        for status in self.statuses.values() {
            media_ids.extend(status.media_ids.iter().copied());
            if let Some(poll_id) = status.poll_id {
                poll_ids.push(poll_id);
            }
        }
        let media_ids = sorted_unique(media_ids);
        let poll_ids = sorted_unique(poll_ids);
        self.media.extend(store.media_by_ids(&media_ids)?);
        // Missing ids are local media; empty strings never count as a source (ingest stores
        // the attachment URL verbatim).
        if !media_ids.is_empty() {
            for (id, source) in store.remote_media_sources_by_ids(&media_ids)? {
                if !source.is_empty() {
                    self.remote_sources.insert(id, source);
                }
            }
        }
        self.polls.extend(store.polls_by_ids(&poll_ids)?);

        let place_ids: Vec<Id> = sorted_unique(
            self.statuses
                .values()
                .filter_map(|status| status.place_id)
                .collect(),
        );
        if !place_ids.is_empty() {
            self.places.extend(store.places_by_ids(&place_ids)?);
        }

        if let Some(viewer) = viewer {
            self.ballots.extend(store.ballots(&poll_ids, viewer)?);
        }
        Ok(())
    }

    /// Viewer-gated maps: conversation mutes for the whole page in one call, flags for every
    /// row and every proper (reblog-target) id in one call, and pair states (both directions)
    /// for every account on the page in one call.
    fn load_viewer_maps(
        &mut self,
        store: &dyn Store,
        viewer: Option<Id>,
        now_ms: u64,
    ) -> Result<(), ApiError> {
        let Some(viewer) = viewer else {
            return Ok(());
        };
        let page_statuses: Vec<Status> = self.statuses.values().cloned().collect();
        self.muted_by_status
            .extend(conversations::muted_map_for_statuses(
                store,
                viewer,
                &page_statuses,
            )?);

        let mut flag_ids: Vec<Id> = Vec::new();
        for status in self.statuses.values() {
            flag_ids.push(status.id);
            flag_ids.push(status.reblog_of_id.unwrap_or(status.id));
        }
        self.flags
            .extend(store.viewer_status_flags(viewer, &sorted_unique(flag_ids))?);

        let others: Vec<Id> = self.accounts.keys().copied().collect();
        self.pairs = store.batch_pair_states(viewer, &others, now_ms)?;
        Ok(())
    }

    /// Local custom emoji referenced by the page's `source_text` plus `spoiler_text`, in one
    /// lookup over the sorted, de-duplicated shortcode set.
    fn load_emoji(&mut self, store: &dyn Store) -> Result<(), ApiError> {
        let mut shortcodes: Vec<String> = Vec::new();
        for status in self.statuses.values() {
            shortcodes.extend(statuses::extract_emoji_shortcodes(&status.source_text));
            if let Some(spoiler) = &status.spoiler_text {
                shortcodes.extend(statuses::extract_emoji_shortcodes(spoiler));
            }
        }
        shortcodes.sort();
        shortcodes.dedup();
        if !shortcodes.is_empty() {
            for emoji in store.emojis_by_shortcodes(&shortcodes)? {
                self.emoji_entities.insert(emoji.shortcode.clone(), emoji);
            }
        }
        Ok(())
    }
}

/// Sort and de-duplicate an id set so every batched loader issues the same bounded key set as
/// `prefetch` does today.
fn sorted_unique(mut ids: Vec<Id>) -> Vec<Id> {
    ids.sort_unstable();
    ids.dedup();
    ids
}

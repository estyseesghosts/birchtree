//! Viewer-aware `StatusView` assembly with a batched page loader.
//!
//! [`StatusViewContext`] resolves the nested records a status DTO needs
//! (author + stats, reblog target, media, mentions, poll + ballot, quote) and
//! the per-viewer flags in *batched* rounds: one transaction per lookup kind
//! for the whole page instead of one transaction per status per kind. Single
//! status reads use the same context over a one-element page, so every read
//! surface shares identical behavior and output.
//!
//! Cache misses (possible only for pathological reblog/quote chains beyond
//! [`MAX_PREFETCH_ROUNDS`]) fall back to a point lookup, keeping the assembly
//! semantics identical to a naive per-status load.

use std::collections::{HashMap, HashSet};

use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::{ServerInfo, dto};
use crate::application::ports::{PairState, StatusViewFlags, Store};
use crate::domain::{
    Account, AccountStats, CustomEmoji, Id, MediaAttachment, Poll, QuoteRecord, Status,
};

/// Prefetch rounds over the discovered reblog/quote closure. Each round is a
/// bounded set of batched lookups; chains deeper than this fall back to point
/// lookups (never to missing data).
const MAX_PREFETCH_ROUNDS: usize = 8;

/// Load a single assembled status view.
pub fn load_status_view(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Option<Id>,
    status: Status,
    now_ms: u64,
) -> Result<dto::StatusView, ApiError> {
    let mut context = StatusViewContext::new(server, store, viewer, now_ms);
    context.prefetch(std::slice::from_ref(&status))?;
    context.view_for(status)
}

/// Load assembled views for a page of statuses (the page loader: batched
/// lookups, constant transaction count in the page size).
pub fn load_status_views(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Option<Id>,
    statuses: Vec<Status>,
    now_ms: u64,
) -> Result<Vec<dto::StatusView>, ApiError> {
    let mut context = StatusViewContext::new(server, store, viewer, now_ms);
    context.prefetch(&statuses)?;
    statuses
        .into_iter()
        .map(|status| context.view_for(status))
        .collect()
}

/// Viewer flags for one status (proper-resolved; see [`load_status_view`]).
pub fn status_view_flags(
    store: &dyn Store,
    viewer: Id,
    status: &Status,
) -> Result<StatusViewFlags, ApiError> {
    let row_id = status.id;
    let proper_id = status.reblog_of_id.unwrap_or(status.id);
    let mut per_id = store
        .viewer_status_flags(viewer, &[row_id, proper_id])
        .map_err(ApiError::from)?;
    let row = per_id.remove(&row_id).unwrap_or_default();
    let proper = per_id.remove(&proper_id).unwrap_or(row);
    Ok(StatusViewFlags {
        favourited: proper.favourited,
        bookmarked: proper.bookmarked,
        reblogged: proper.reblogged,
        pinned: row.pinned,
        muted: crate::application::conversations::status_muted(store, viewer, status)
            .map_err(ApiError::from)?,
    })
}

/// Batched loader for one page of statuses (or one status).
///
/// Structure: [`StatusViewContext::prefetch`] walks the reblog/quote closure
/// of the page in bounded rounds, then issues one batched lookup per record
/// kind (accounts, stats, media, polls, ballots, quotes, flags, pair states).
/// [`StatusViewContext::view_for`] assembles from the cache, falling back to
/// point lookups on a miss.
pub struct StatusViewContext<'a> {
    server: &'a ServerInfo,
    store: &'a dyn Store,
    viewer: Option<Id>,
    now_ms: u64,
    /// Every status known to this page (rows, reblog targets, quoted
    /// statuses), by id.
    statuses: HashMap<Id, Status>,
    accounts: HashMap<Id, Account>,
    stats: HashMap<Id, AccountStats>,
    /// Quote records keyed by quoting status id.
    quotes: HashMap<Id, QuoteRecord>,
    /// Statuses whose quote lookup already ran (so a round never repeats it).
    quote_probed: HashSet<Id>,
    media: HashMap<Id, MediaAttachment>,
    polls: HashMap<Id, Poll>,
    ballots: HashMap<Id, Vec<u64>>,
    /// Core extension CORE-P: places by id, batch-loaded per page.
    places: HashMap<Id, crate::domain::Place>,
    /// Viewer flags keyed by row-exact status id.
    flags: HashMap<Id, StatusViewFlags>,
    /// Conversation-mute flags keyed by row-exact status id (Phase 7).
    muted_by_status: HashMap<Id, bool>,
    /// `(incoming, outgoing)` pair states keyed by the other account id.
    pairs: HashMap<Id, (PairState, PairState)>,
    /// Custom emoji referenced by the page's statuses, by shortcode.
    emoji_entities: HashMap<String, CustomEmoji>,
}

impl<'a> StatusViewContext<'a> {
    pub fn new(
        server: &'a ServerInfo,
        store: &'a dyn Store,
        viewer: Option<Id>,
        now_ms: u64,
    ) -> Self {
        Self {
            server,
            store,
            viewer,
            now_ms,
            statuses: HashMap::new(),
            accounts: HashMap::new(),
            stats: HashMap::new(),
            quotes: HashMap::new(),
            quote_probed: HashSet::new(),
            media: HashMap::new(),
            polls: HashMap::new(),
            ballots: HashMap::new(),
            places: HashMap::new(),
            flags: HashMap::new(),
            muted_by_status: HashMap::new(),
            pairs: HashMap::new(),
            emoji_entities: HashMap::new(),
        }
    }

    /// Discover the reblog/quote closure of `roots`, then batch-load every
    /// record kind the assembly reads.
    pub fn prefetch(&mut self, roots: &[Status]) -> Result<(), ApiError> {
        for status in roots {
            self.statuses.insert(status.id, status.clone());
        }

        // Rounds: reblog targets and quoted statuses pull in more statuses;
        // each round adds at least one batched lookup and stops at fixpoint.
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
                let targets = self.store.statuses_by_ids(&missing_targets)?;
                for target in targets {
                    self.statuses.insert(target.id, target);
                }
            }

            let unprobed: Vec<Id> = self
                .statuses
                .keys()
                .copied()
                .filter(|id| !self.quote_probed.contains(id))
                .collect();
            if !unprobed.is_empty() {
                changed = true;
                let quotes = self.store.quotes_by_quoting_ids(&unprobed)?;
                for id in &unprobed {
                    self.quote_probed.insert(*id);
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
                let quoted = self.store.statuses_by_ids(&missing_quoted)?;
                for status in quoted {
                    self.statuses.insert(status.id, status);
                }
            }

            if !changed {
                break;
            }
        }

        // Authors, mentioned accounts and quoted authors, batched once.
        let mut account_ids: Vec<Id> = Vec::new();
        for status in self.statuses.values() {
            account_ids.push(status.account_id);
            account_ids.extend(status.mentions.iter().copied());
        }
        account_ids.sort_unstable();
        account_ids.dedup();
        self.accounts.extend(self.store.accounts_by_ids(&account_ids)?);

        let authors: Vec<Id> = {
            let mut authors: Vec<Id> = self.statuses.values().map(|s| s.account_id).collect();
            authors.sort_unstable();
            authors.dedup();
            authors
        };
        self.stats.extend(self.store.account_stats_by_ids(&authors)?);

        // Attachments and polls, batched.
        let mut media_ids: Vec<Id> = Vec::new();
        let mut poll_ids: Vec<Id> = Vec::new();
        for status in self.statuses.values() {
            media_ids.extend(status.media_ids.iter().copied());
            if let Some(poll_id) = status.poll_id {
                poll_ids.push(poll_id);
            }
        }
        media_ids.sort_unstable();
        media_ids.dedup();
        poll_ids.sort_unstable();
        poll_ids.dedup();
        self.media.extend(self.store.media_by_ids(&media_ids)?);
        self.polls.extend(self.store.polls_by_ids(&poll_ids)?);
        // Core extension CORE-P: places attached to the page's statuses,
        // batched once (missing rows read as no place at pack time).
        let mut place_ids: Vec<Id> = self
            .statuses
            .values()
            .filter_map(|status| status.place_id)
            .collect();
        place_ids.sort_unstable();
        place_ids.dedup();
        if !place_ids.is_empty() {
            self.places.extend(self.store.places_by_ids(&place_ids)?);
        }
        if let Some(viewer) = self.viewer {
            self.ballots.extend(self.store.ballots(&poll_ids, viewer)?);
            // Conversation mutes for the whole page in ONE batched call.
            let page_statuses: Vec<Status> =
                self.statuses.values().cloned().collect();
            self.muted_by_status.extend(
                crate::application::conversations::muted_map_for_statuses(
                    self.store,
                    viewer,
                    &page_statuses,
                )?,
            );
        }

        // Viewer flags for every row and every proper target in ONE call.
        if let Some(viewer) = self.viewer {
            let mut flag_ids: Vec<Id> = Vec::new();
            for status in self.statuses.values() {
                flag_ids.push(status.id);
                flag_ids.push(status.reblog_of_id.unwrap_or(status.id));
            }
            flag_ids.sort_unstable();
            flag_ids.dedup();
            self.flags.extend(self.store.viewer_status_flags(viewer, &flag_ids)?);
        }

        // Pair states (both directions) for every account on the page.
        if let Some(viewer) = self.viewer {
            let others: Vec<Id> = self.accounts.keys().copied().collect();
            self.pairs = self.store.batch_pair_states(viewer, &others, self.now_ms)?;
        }

        // Custom emoji referenced by the page's text, in ONE lookup.
        let shortcodes: Vec<String> = {
            let mut shortcodes: Vec<String> = Vec::new();
            for status in self.statuses.values() {
                shortcodes.extend(crate::application::statuses::extract_emoji_shortcodes(
                    &status.source_text,
                ));
                if let Some(spoiler) = &status.spoiler_text {
                    shortcodes.extend(crate::application::statuses::extract_emoji_shortcodes(
                        spoiler,
                    ));
                }
            }
            shortcodes.sort();
            shortcodes.dedup();
            shortcodes
        };
        if !shortcodes.is_empty() {
            for emoji in self.store.emojis_by_shortcodes(&shortcodes)? {
                self.emoji_entities.insert(emoji.shortcode.clone(), emoji);
            }
        }

        Ok(())
    }

    /// Assemble one view from the cache (point lookups on a miss).
    pub fn view_for(&mut self, status: Status) -> Result<dto::StatusView, ApiError> {
        let account = self.account(status.account_id)?;
        let account_stats = self.stats(status.account_id)?;

        // Wrapped reblog target (one level; reblogs of reblogs unwrap to
        // proper). A missing target row is an internal inconsistency, matching
        // the previous per-status loader.
        let reblog = match status.reblog_of_id {
            Some(proper_id) => {
                let proper = self
                    .status_row(proper_id)?
                    .ok_or_else(ApiError::internal)?;
                let inner = self.view_for(proper)?;
                Some(Box::new(inner))
            }
            None => None,
        };

        let flags = match self.viewer {
            Some(viewer) => self.flags_for(viewer, &status)?,
            None => StatusViewFlags::default(),
        };

        let mut media = Vec::with_capacity(status.media_ids.len());
        for media_id in &status.media_ids {
            if let Some(attachment) = self.media(*media_id)? {
                media.push(attachment);
            }
        }

        let mut mentions = Vec::with_capacity(status.mentions.len());
        for mention_id in &status.mentions {
            if let Some(mention) = self.account_opt(*mention_id)? {
                mentions.push(mention);
            }
        }

        let poll = match status.poll_id {
            Some(poll_id) => self.poll(poll_id)?,
            None => None,
        };
        let viewer_ballot = match (poll.as_ref(), self.viewer) {
            (Some(poll), Some(viewer)) => self.ballot(poll.id, viewer)?,
            _ => None,
        };

        let quote = self.quote_view(status.clone())?;
        let quote_current_user = self.quote_current_user(&status)?;

        // Custom emoji referenced by this status's text (page-batched).
        let mut shortcodes = crate::application::statuses::extract_emoji_shortcodes(
            &status.source_text,
        );
        if let Some(spoiler) = &status.spoiler_text {
            shortcodes.extend(crate::application::statuses::extract_emoji_shortcodes(spoiler));
        }
        shortcodes.sort();
        shortcodes.dedup();
        let mut emojis: Vec<serde_json::Value> = shortcodes
            .iter()
            .filter_map(|shortcode| self.emoji_entities.get(shortcode))
            .map(|emoji| dto::custom_emoji_json(self.server, emoji, false))
            .collect();
        // Remote emoji (federation tail, G-D): shortcodes with no local row
        // render from the remote table (never picker-visible, never listed).
        let missing: Vec<String> = shortcodes
            .iter()
            .filter(|shortcode| !self.emoji_entities.contains_key(*shortcode))
            .cloned()
            .collect();
        if !missing.is_empty() {
            for row in self.store.remote_emojis_by_shortcodes(&missing)? {
                // Staged bytes serve through the `system/` asset route, like
                // local emoji (the row stores the asset path, not the origin
                // URL, so renders never hotlink the origin).
                let url = self.server.asset_url(&row.url);
                emojis.push(crate::activitypub::objects::remote_emoji_entity(
                    &row.shortcode,
                    &url,
                ));
            }
        }

        // Core extension CORE-P: the attached place (batched above;
        // dangling ids read as no place).
        let place = status.place_id.and_then(|id| self.places.get(&id).cloned());
        Ok(dto::StatusView {
            status,
            account,
            account_stats: Some(account_stats),
            flags,
            reblog,
            poll,
            viewer_ballot,
            media,
            mentions,
            quote,
            quote_current_user,
            shallow_quote: false,
            filtered: Vec::new(),
            emojis,
            place,
        })
    }

    // -- cache accessors (point lookup fallback on a miss) -------------------

    fn account(&mut self, id: Id) -> Result<Account, ApiError> {
        self.account_opt(id)?.ok_or_else(ApiError::internal)
    }

    fn account_opt(&mut self, id: Id) -> Result<Option<Account>, ApiError> {
        if let Some(account) = self.accounts.get(&id) {
            return Ok(Some(account.clone()));
        }
        let fetched = self.store.accounts_by_ids(&[id])?;
        let account = fetched.get(&id).cloned();
        if let Some(account) = &account {
            self.accounts.insert(id, account.clone());
        }
        Ok(account)
    }

    fn stats(&mut self, id: Id) -> Result<AccountStats, ApiError> {
        if let Some(stats) = self.stats.get(&id) {
            return Ok(*stats);
        }
        let fetched = self
            .store
            .account_stats_by_ids(std::slice::from_ref(&id))?;
        let stats = fetched.get(&id).copied().unwrap_or_default();
        self.stats.insert(id, stats);
        Ok(stats)
    }

    fn status_row(&mut self, id: Id) -> Result<Option<Status>, ApiError> {
        if let Some(status) = self.statuses.get(&id) {
            return Ok(Some(status.clone()));
        }
        let fetched = self.store.statuses_by_ids(std::slice::from_ref(&id))?;
        let status = fetched.into_iter().next();
        if let Some(status) = &status {
            self.statuses.insert(id, status.clone());
        }
        Ok(status)
    }

    fn media(&mut self, id: Id) -> Result<Option<MediaAttachment>, ApiError> {
        if let Some(media) = self.media.get(&id) {
            return Ok(Some(media.clone()));
        }
        let fetched = self.store.media_by_ids(std::slice::from_ref(&id))?;
        let media = fetched.get(&id).cloned();
        if let Some(media) = &media {
            self.media.insert(id, media.clone());
        }
        Ok(media)
    }

    fn poll(&mut self, id: Id) -> Result<Option<Poll>, ApiError> {
        if let Some(poll) = self.polls.get(&id) {
            return Ok(Some(poll.clone()));
        }
        let fetched = self.store.polls_by_ids(std::slice::from_ref(&id))?;
        let poll = fetched.get(&id).cloned();
        if let Some(poll) = &poll {
            self.polls.insert(id, poll.clone());
        }
        Ok(poll)
    }

    fn ballot(&mut self, poll_id: Id, viewer: Id) -> Result<Option<Vec<u64>>, ApiError> {
        if let Some(ballot) = self.ballots.get(&poll_id) {
            return Ok(Some(ballot.clone()));
        }
        let fetched = self.store.ballots(std::slice::from_ref(&poll_id), viewer)?;
        let ballot = fetched.get(&poll_id).cloned();
        if let Some(ballot) = &ballot {
            self.ballots.insert(poll_id, ballot.clone());
        }
        Ok(ballot)
    }

    /// Proper-resolved viewer flags for a status (row flags for `pinned`,
    /// proper-status flags for the rest), from the batched cache.
    fn flags_for(&mut self, viewer: Id, status: &Status) -> Result<StatusViewFlags, ApiError> {
        let row_id = status.id;
        let proper_id = status.reblog_of_id.unwrap_or(status.id);
        let row = self.flag_for(viewer, row_id)?;
        let proper = self.flag_for(viewer, proper_id)?;
        Ok(StatusViewFlags {
            favourited: proper.favourited,
            bookmarked: proper.bookmarked,
            reblogged: proper.reblogged,
            pinned: row.pinned,
            muted: self.muted_for(viewer, status)?,
        })
    }

    /// Conversation-mute flag for one status: the batched page map wins, with
    /// a point lookup fallback (only reachable for chains beyond the prefetch
    /// rounds).
    fn muted_for(&mut self, viewer: Id, status: &Status) -> Result<bool, ApiError> {
        if let Some(muted) = self.muted_by_status.get(&status.id) {
            return Ok(*muted);
        }
        let muted = crate::application::conversations::status_muted(self.store, viewer, status)?;
        self.muted_by_status.insert(status.id, muted);
        Ok(muted)
    }

    /// Flags for one id, from the batched cache (point lookup on a miss —
    /// only reachable for chains beyond the prefetch rounds).
    fn flag_for(&mut self, viewer: Id, id: Id) -> Result<StatusViewFlags, ApiError> {
        if let Some(flags) = self.flags.get(&id) {
            return Ok(*flags);
        }
        let mut fetched = self.store.viewer_status_flags(viewer, &[id])?;
        let flags = fetched.remove(&id).unwrap_or_default();
        self.flags.insert(id, flags);
        Ok(flags)
    }

    fn quote_view(&mut self, status: Status) -> Result<Option<dto::QuoteView>, ApiError> {
        let Some(quote) = self.quote_record(status.id)? else {
            return Ok(None);
        };

        let quoted = self.status_row(quote.quoted_status_id)?;

        let Some(quoted_status) = quoted else {
            // Accepted but the quoted status is gone; otherwise the stored
            // state stands (never leaking quoted content).
            let state = if quote.state == crate::domain::QuoteState::Accepted {
                "deleted"
            } else {
                quote_state_str(quote.state)
            };
            return Ok(Some(dto::QuoteView {
                state: state.into(),
                quoted_status: None,
                quoted_status_id: Some(quote.quoted_status_id),
            }));
        };

        if quote.state != crate::domain::QuoteState::Accepted {
            // Unaccepted quotes never leak quoted content.
            return Ok(Some(dto::QuoteView {
                state: quote_state_str(quote.state).into(),
                quoted_status: None,
                quoted_status_id: Some(quote.quoted_status_id),
            }));
        }

        let pair = self.incoming_pair(quoted_status.account_id)?;
        let fetchable =
            crate::application::policy::can_fetch(self.viewer, &quoted_status, pair);
        let viewer_pair = self.outgoing_pair(quoted_status.account_id)?;
        let state = if !fetchable {
            "unauthorized"
        } else if viewer_pair.muting {
            "muted_account"
        } else {
            "accepted"
        };

        let quoted_view = if fetchable && !quoted_status.is_reblog() {
            let mut inner = self.view_for(quoted_status)?;
            inner.shallow_quote = true;
            Some(Box::new(inner))
        } else {
            None
        };

        Ok(Some(dto::QuoteView {
            state: state.into(),
            quoted_status: quoted_view,
            quoted_status_id: Some(quote.quoted_status_id),
        }))
    }

    /// `quote_approval.current_user` for this viewer on `status`.
    fn quote_current_user(&mut self, status: &Status) -> Result<String, ApiError> {
        let Some(viewer) = self.viewer else {
            return Ok("denied".into());
        };
        if viewer == status.account_id {
            return Ok("automatic".into());
        }
        let pair = self.incoming_pair(status.account_id)?;
        match crate::application::policy::quote_permission(Some(viewer), status, pair) {
            crate::application::policy::QuotePermission::SelfQuote
            | crate::application::policy::QuotePermission::Automatic => Ok("automatic".into()),
            crate::application::policy::QuotePermission::Denied => Ok("denied".into()),
        }
    }

    /// `pair_state(account, viewer)` — does the account block/follow/mute the
    /// viewer?
    fn incoming_pair(&mut self, account: Id) -> Result<PairState, ApiError> {
        if let Some((incoming, _)) = self.pairs.get(&account) {
            return Ok(*incoming);
        }
        let Some(viewer) = self.viewer else {
            return Ok(PairState::default());
        };
        let fetched = self.store.batch_pair_states(viewer, std::slice::from_ref(&account), self.now_ms)?;
        let pair = fetched
            .get(&account)
            .map(|(incoming, _)| *incoming)
            .unwrap_or_default();
        self.pairs.insert(account, (pair, PairState::default()));
        Ok(pair)
    }

    /// `pair_state(viewer, account)` — does the viewer block/follow/mute the
    /// account?
    fn outgoing_pair(&mut self, account: Id) -> Result<PairState, ApiError> {
        if let Some((_, outgoing)) = self.pairs.get(&account) {
            return Ok(*outgoing);
        }
        let Some(viewer) = self.viewer else {
            return Ok(PairState::default());
        };
        let fetched = self.store.batch_pair_states(viewer, std::slice::from_ref(&account), self.now_ms)?;
        let pair = fetched
            .get(&account)
            .map(|(_, outgoing)| *outgoing)
            .unwrap_or_default();
        self.pairs.insert(account, (PairState::default(), pair));
        Ok(pair)
    }

    fn quote_record(&mut self, quoting_status_id: Id) -> Result<Option<QuoteRecord>, ApiError> {
        if let Some(quote) = self.quotes.get(&quoting_status_id) {
            return Ok(Some(quote.clone()));
        }
        if self.quote_probed.contains(&quoting_status_id) {
            return Ok(None);
        }
        let fetched = self.store.quotes_by_quoting_ids(std::slice::from_ref(&quoting_status_id))?;
        self.quote_probed.insert(quoting_status_id);
        let quote = fetched.get(&quoting_status_id).cloned();
        if let Some(quote) = &quote {
            self.quotes.insert(quoting_status_id, quote.clone());
        }
        Ok(quote)
    }
}

fn quote_state_str(state: crate::domain::QuoteState) -> &'static str {
    use crate::domain::QuoteState;
    match state {
        QuoteState::Pending => "pending",
        QuoteState::Accepted => "accepted",
        QuoteState::Rejected => "rejected",
        QuoteState::Revoked => "revoked",
        QuoteState::Deleted => "deleted",
    }
}

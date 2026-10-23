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

use super::status_view_batch::StatusViewBatch;
use crate::application::ports::{PairState, StatusViewFlags, Store};
use crate::domain::{
    Account, AccountStats, Id, MediaAttachment, Poll, QuoteRecord, Status,
};

/// Prefetch rounds over the discovered reblog/quote closure. Each round is a
/// bounded set of batched lookups; chains deeper than this fall back to point
/// lookups (never to missing data).
pub(crate) const MAX_PREFETCH_ROUNDS: usize = 8;

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
    /// Every record kind the assembly reads, loaded once per page (see
    /// [`StatusViewBatch`]).
    batch: StatusViewBatch,
    /// Statuses whose quote lookup already ran (so a round never repeats it).
    quote_probed: HashSet<Id>,
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
            batch: StatusViewBatch::empty(),
            quote_probed: HashSet::new(),
        }
    }

    /// Discover the reblog/quote closure of `roots`, then batch-load every
    /// record kind the assembly reads (one batched snapshot for the page).
    pub fn prefetch(&mut self, roots: &[Status]) -> Result<(), ApiError> {
        self.batch = StatusViewBatch::load_for_statuses(self.store, self.viewer, roots, self.now_ms)?;
        Ok(())
    }

    /// Assemble one view from the cache (point lookups on a miss).
    pub fn view_for(&mut self, status: Status) -> Result<dto::StatusView, ApiError> {
        let account = self.account(status.account_id)?;
        let account_stats = self.stats(status.account_id)?;
        let reblog = self.assemble_reblog(&status)?;
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

        let (poll, viewer_ballot) = self.assemble_poll_section(&status)?;

        let quote = self.quote_view(status.clone())?;
        let quote_current_user = self.quote_current_user(&status)?;

        let (emojis, remote_media_sources) = self.assemble_emoji_section(&status, &media)?;

        // Core extension CORE-P: the attached place (batched above;
        // dangling ids read as no place).
        let place = status.place_id.and_then(|id| self.batch.places.get(&id).cloned());
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
            remote_media_sources,
        })
    }

    /// Wrapped reblog target (one level; reblogs of reblogs unwrap to
    /// proper). A missing target row is an internal inconsistency, matching
    /// the previous per-status loader.
    fn assemble_reblog(
        &mut self,
        status: &Status,
    ) -> Result<Option<Box<dto::StatusView>>, ApiError> {
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
        Ok(reblog)
    }

    /// Poll plus the viewer's ballot on it (verbatim section).
    fn assemble_poll_section(
        &mut self,
        status: &Status,
    ) -> Result<(Option<Poll>, Option<Vec<u64>>), ApiError> {
        let poll = match status.poll_id {
            Some(poll_id) => self.poll(poll_id)?,
            None => None,
        };
        let viewer_ballot = match (poll.as_ref(), self.viewer) {
            (Some(poll), Some(viewer)) => self.ballot(poll.id, viewer)?,
            _ => None,
        };
        Ok((poll, viewer_ballot))
    }

    /// Custom emoji plus per-attachment remote origin URLs (verbatim section).
    fn assemble_emoji_section(
        &mut self,
        status: &Status,
        media: &[MediaAttachment],
    ) -> Result<(Vec<serde_json::Value>, HashMap<Id, String>), ApiError> {
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
            .filter_map(|shortcode| self.batch.emoji_entities.get(shortcode))
            .map(|emoji| dto::custom_emoji_json(self.server, emoji, false))
            .collect();
        // Remote emoji (federation tail, G-D): shortcodes with no local row
        // render from the remote table (never picker-visible, never listed).
        let missing: Vec<String> = shortcodes
            .iter()
            .filter(|shortcode| !self.batch.emoji_entities.contains_key(*shortcode))
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

        // Remote origin URLs for this status's attachments (batch-loaded
        // above; point lookup only for chains beyond the prefetch rounds).
        let mut remote_media_sources = HashMap::with_capacity(media.len());
        for attachment in media {
            if let Some(source) = self.remote_source(attachment.id)? {
                remote_media_sources.insert(attachment.id, source);
            }
        }
        Ok((emojis, remote_media_sources))
    }

    // -- cache accessors (point lookup fallback on a miss) -------------------

    fn account(&mut self, id: Id) -> Result<Account, ApiError> {
        self.account_opt(id)?.ok_or_else(ApiError::internal)
    }

    fn account_opt(&mut self, id: Id) -> Result<Option<Account>, ApiError> {
        if let Some(account) = self.batch.accounts.get(&id) {
            return Ok(Some(account.clone()));
        }
        let fetched = self.store.accounts_by_ids(&[id])?;
        let account = fetched.get(&id).cloned();
        if let Some(account) = &account {
            self.batch.accounts.insert(id, account.clone());
        }
        Ok(account)
    }

    fn stats(&mut self, id: Id) -> Result<AccountStats, ApiError> {
        if let Some(stats) = self.batch.stats.get(&id) {
            return Ok(*stats);
        }
        let fetched = self
            .store
            .account_stats_by_ids(std::slice::from_ref(&id))?;
        let stats = fetched.get(&id).copied().unwrap_or_default();
        self.batch.stats.insert(id, stats);
        Ok(stats)
    }

    fn status_row(&mut self, id: Id) -> Result<Option<Status>, ApiError> {
        if let Some(status) = self.batch.statuses.get(&id) {
            return Ok(Some(status.clone()));
        }
        let fetched = self.store.statuses_by_ids(std::slice::from_ref(&id))?;
        let status = fetched.into_iter().next();
        if let Some(status) = &status {
            self.batch.statuses.insert(id, status.clone());
        }
        Ok(status)
    }

    fn media(&mut self, id: Id) -> Result<Option<MediaAttachment>, ApiError> {
        if let Some(media) = self.batch.media.get(&id) {
            return Ok(Some(media.clone()));
        }
        let fetched = self.store.media_by_ids(std::slice::from_ref(&id))?;
        let media = fetched.get(&id).cloned();
        if let Some(media) = &media {
            self.batch.media.insert(id, media.clone());
        }
        Ok(media)
    }

    /// Stored remote origin URL for one attachment, if any (empty strings
    /// never count). Served from the page-batched cache; the point lookup
    /// is only reachable for chains beyond the prefetch rounds.
    fn remote_source(&mut self, id: Id) -> Result<Option<String>, ApiError> {
        if let Some(source) = self.batch.remote_sources.get(&id) {
            return Ok(Some(source.clone()));
        }
        let fetched = self.store.remote_media_source(id)?;
        match fetched.filter(|source| !source.is_empty()) {
            Some(source) => {
                self.batch.remote_sources.insert(id, source.clone());
                Ok(Some(source))
            }
            None => Ok(None),
        }
    }

    fn poll(&mut self, id: Id) -> Result<Option<Poll>, ApiError> {
        if let Some(poll) = self.batch.polls.get(&id) {
            return Ok(Some(poll.clone()));
        }
        let fetched = self.store.polls_by_ids(std::slice::from_ref(&id))?;
        let poll = fetched.get(&id).cloned();
        if let Some(poll) = &poll {
            self.batch.polls.insert(id, poll.clone());
        }
        Ok(poll)
    }

    fn ballot(&mut self, poll_id: Id, viewer: Id) -> Result<Option<Vec<u64>>, ApiError> {
        if let Some(ballot) = self.batch.ballots.get(&poll_id) {
            return Ok(Some(ballot.clone()));
        }
        let fetched = self.store.ballots(std::slice::from_ref(&poll_id), viewer)?;
        let ballot = fetched.get(&poll_id).cloned();
        if let Some(ballot) = &ballot {
            self.batch.ballots.insert(poll_id, ballot.clone());
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
        if let Some(muted) = self.batch.muted_by_status.get(&status.id) {
            return Ok(*muted);
        }
        let muted = crate::application::conversations::status_muted(self.store, viewer, status)?;
        self.batch.muted_by_status.insert(status.id, muted);
        Ok(muted)
    }

    /// Flags for one id, from the batched cache (point lookup on a miss —
    /// only reachable for chains beyond the prefetch rounds).
    fn flag_for(&mut self, viewer: Id, id: Id) -> Result<StatusViewFlags, ApiError> {
        if let Some(flags) = self.batch.flags.get(&id) {
            return Ok(*flags);
        }
        let mut fetched = self.store.viewer_status_flags(viewer, &[id])?;
        let flags = fetched.remove(&id).unwrap_or_default();
        self.batch.flags.insert(id, flags);
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
        if let Some((incoming, _)) = self.batch.pairs.get(&account) {
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
        self.batch.pairs.insert(account, (pair, PairState::default()));
        Ok(pair)
    }

    /// `pair_state(viewer, account)` — does the viewer block/follow/mute the
    /// account?
    fn outgoing_pair(&mut self, account: Id) -> Result<PairState, ApiError> {
        if let Some((_, outgoing)) = self.batch.pairs.get(&account) {
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
        self.batch.pairs.insert(account, (PairState::default(), pair));
        Ok(pair)
    }

    fn quote_record(&mut self, quoting_status_id: Id) -> Result<Option<QuoteRecord>, ApiError> {
        if let Some(quote) = self.batch.quotes.get(&quoting_status_id) {
            return Ok(Some(quote.clone()));
        }
        if self.quote_probed.contains(&quoting_status_id) {
            return Ok(None);
        }
        let fetched = self.store.quotes_by_quoting_ids(std::slice::from_ref(&quoting_status_id))?;
        self.quote_probed.insert(quoting_status_id);
        let quote = fetched.get(&quoting_status_id).cloned();
        if let Some(quote) = &quote {
            self.batch.quotes.insert(quoting_status_id, quote.clone());
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

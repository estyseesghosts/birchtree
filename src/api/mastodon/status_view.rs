//! Viewer-aware `StatusView` assembly.
//!
//! One place resolves the nested records a status DTO needs (author + stats,
//! reblog target, media, mentions, poll + ballot, quote) and the per-viewer
//! flags, so endpoints stay thin and every read surface shares identical
//! behavior. Batch lookups are grouped by author/poll to avoid N+1 where it
//! matters (Phase 4 revisits measured page loaders).

use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::{ServerInfo, dto};
use crate::application::ports::{PairState, StatusViewFlags, Store};
use crate::domain::{Id, QuoteState, Status};

/// Load a single assembled status view.
pub fn load_status_view(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Option<Id>,
    status: Status,
    now_ms: u64,
) -> Result<dto::StatusView, ApiError> {
    let account = store
        .account_by_id(status.account_id)
        .map_err(ApiError::from)?
        .ok_or_else(ApiError::internal)?;
    let account_stats = store.account_stats(status.account_id).map_err(ApiError::from)?;

    // Wrapped reblog target (one level; reblogs of reblogs unwrap to proper).
    let reblog = match status.reblog_of_id {
        Some(proper_id) => {
            let proper = store
                .status_by_id(proper_id)
                .map_err(ApiError::from)?
                .filter(|s| !s.is_deleted())
                .ok_or_else(ApiError::internal)?;
            let inner = load_status_view(server, store, viewer, proper, now_ms)?;
            Some(Box::new(inner))
        }
        None => None,
    };

    // Per-viewer flags: `favourited`/`bookmarked`/`reblogged` resolve through
    // `status.proper` (Mastodon `Account#favourited?`/`bookmarked?`/
    // `reblogged?` all read `status.proper.*`), while `pinned` is row-exact
    // (`Account#pinned?`). For a reblog wrapper the proper status is the
    // original; for everything else it is the row itself.
    let flags = match viewer {
        Some(viewer) => status_view_flags(store, viewer, &status)?,
        None => StatusViewFlags::default(),
    };

    // Media in the author's order.
    let mut media = Vec::with_capacity(status.media_ids.len());
    for media_id in &status.media_ids {
        if let Some(attachment) = store.media_by_id(*media_id).map_err(ApiError::from)? {
            media.push(attachment);
        }
    }

    // Mentioned accounts in order.
    let mut mentions = Vec::with_capacity(status.mentions.len());
    for mention_id in &status.mentions {
        if let Some(mention) = store.account_by_id(*mention_id).map_err(ApiError::from)? {
            mentions.push(mention);
        }
    }

    // Poll + viewer ballot.
    let poll = match status.poll_id {
        Some(poll_id) => store.poll_by_id(poll_id).map_err(ApiError::from)?,
        None => None,
    };
    let viewer_ballot = match (poll.as_ref(), viewer) {
        (Some(poll), Some(viewer)) => store
            .ballot(poll.id, viewer)
            .map_err(ApiError::from)?,
        _ => None,
    };

    // Quote association and its resolved wire state.
    let quote = load_quote_view(server, store, viewer, &status, now_ms)?;

    // `quote_approval.current_user` for this viewer.
    let quote_current_user = match viewer {
        None => "denied".to_string(),
        Some(viewer) if viewer == status.account_id => "automatic".to_string(),
        Some(viewer) => {
            let pair = store
                .pair_state(status.account_id, viewer, now_ms)
                .map_err(ApiError::from)?;
            match crate::application::policy::quote_permission(Some(viewer), &status, pair) {
                crate::application::policy::QuotePermission::SelfQuote
                | crate::application::policy::QuotePermission::Automatic => {
                    "automatic".to_string()
                }
                crate::application::policy::QuotePermission::Denied => "denied".to_string(),
            }
        }
    };

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
    })
}

/// Load assembled views for a page of statuses.
pub fn load_status_views(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Option<Id>,
    statuses: Vec<Status>,
    now_ms: u64,
) -> Result<Vec<dto::StatusView>, ApiError> {
    statuses
        .into_iter()
        .map(|status| load_status_view(server, store, viewer, status, now_ms))
        .collect()
}

/// Viewer flags for one status (proper-resolved; see `load_status_view`).
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
    })
}

/// Resolve the quote association of `status` for the viewer.
fn load_quote_view(
    server: &ServerInfo,
    store: &dyn Store,
    viewer: Option<Id>,
    status: &Status,
    now_ms: u64,
) -> Result<Option<dto::QuoteView>, ApiError> {
    let Some(quote) = store
        .quote_by_quoting_status(status.id)
        .map_err(ApiError::from)?
    else {
        return Ok(None);
    };

    let quoted = match store
        .status_by_id(quote.quoted_status_id)
        .map_err(ApiError::from)?
        .filter(|s| !s.is_deleted())
    {
        Some(quoted) => quoted,
        // Accepted but the quoted status is gone.
        None if quote.state == QuoteState::Accepted => {
            return Ok(Some(dto::QuoteView {
                state: "deleted".into(),
                quoted_status: None,
                quoted_status_id: Some(quote.quoted_status_id),
            }));
        }
        None => {
            return Ok(Some(dto::QuoteView {
                state: state_str(quote.state).into(),
                quoted_status: None,
                quoted_status_id: Some(quote.quoted_status_id),
            }));
        }
    };

    if quote.state != QuoteState::Accepted {
        // Unaccepted quotes never leak quoted content.
        return Ok(Some(dto::QuoteView {
            state: state_str(quote.state).into(),
            quoted_status: None,
            quoted_status_id: Some(quote.quoted_status_id),
        }));
    }

    // Resolved state + content gating for accepted quotes.
    let pair = match viewer {
        Some(viewer) => store
            .pair_state(quoted.account_id, viewer, now_ms)
            .map_err(ApiError::from)?,
        None => PairState::default(),
    };
    let fetchable = crate::application::policy::can_fetch(viewer, &quoted, pair);
    let viewer_pair = match viewer {
        Some(viewer) => store
            .pair_state(viewer, quoted.account_id, now_ms)
            .map_err(ApiError::from)?,
        None => PairState::default(),
    };
    let state = if !fetchable {
        "unauthorized"
    } else if viewer_pair.muting {
        "muted_account"
    } else {
        "accepted"
    };

    let quoted_status = if fetchable && !quoted.is_reblog() {
        let mut inner = load_status_view(server, store, viewer, quoted, now_ms)?;
        inner.shallow_quote = true;
        Some(Box::new(inner))
    } else {
        None
    };

    Ok(Some(dto::QuoteView {
        state: state.into(),
        quoted_status,
        quoted_status_id: Some(quote.quoted_status_id),
    }))
}

fn state_str(state: QuoteState) -> &'static str {
    match state {
        QuoteState::Pending => "pending",
        QuoteState::Accepted => "accepted",
        QuoteState::Rejected => "rejected",
        QuoteState::Revoked => "revoked",
        QuoteState::Deleted => "deleted",
    }
}

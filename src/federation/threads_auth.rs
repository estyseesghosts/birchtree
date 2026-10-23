//! Quote-authorization re-verification chain (S7 slice group A, step 4).
//!
//! Extraction of the quote-auth chain from `src/federation/threads.rs`
//! (`check_authorization_remote`, `run_quote_reverify` guard). The thread
//! completion entry points stay in `threads.rs`; the parent wires them to
//! drive these types.
//!
//! Source spans (read-only audit): `threads.rs:31` `MAX_PARENT_HOPS`,
//! `threads.rs:320-329` quote re-verify caps, `threads.rs:379-428`
//! `run_quote_reverify`, `threads.rs:462-567` `check_authorization_remote`.
//!
//! Constants below are byte-identical to `threads.rs`. The canonical
//! definitions stay in `threads.rs` until the parent dedups them; this module
//! duplicates the values so each stage stays reviewable on its own.
//!
//! Scalar params are unchanged: every helper takes plain ids, counters and
//! timestamps. [`QuoteAuthCtx`] only bundles the borrow pair (`ctx`,
//! `store`) with those scalars so one chain hop can be passed along without
//! a new context object.

use crate::activitypub::FedContext;
use crate::application::ports::Store;
use crate::domain::Id;
use crate::federation::delivery::DeliveryError;

/// Parent hops per chain (contract D: a proposal; upstream has no hop cap).
pub const MAX_PARENT_HOPS: u64 = 40;

/// Hops per re-verify chain: one job re-verifies one quote; a quote whose
/// quoted status is itself a quoting status with its own live quote enqueues
/// the next hop. Past this, the chain stops with the remaining quotes kept.
pub const QUOTE_REVERIFY_MAX_HOPS: u64 = 8;

/// Fetches per re-verify chain: each hop performs at most one approval fetch.
/// Past this budget the chain stops with every remaining quote kept as-is.
pub const QUOTE_REVERIFY_FETCH_BUDGET: u64 = 100;

/// Largest accepted quote-authorization document (matches the remote-document
/// body cap in the federation contract).
pub const QUOTE_REVERIFY_MAX_BODY_BYTES: u64 = 200 * 1024;

/// One quote-authorization hop: the borrow pair plus plain scalars.
///
/// Bundles exactly the seven `check_authorization_remote` params
/// (`ctx`, `store`, `quote_id`, `hop`, `fetches`, `chain`, `now_ms`) without
/// introducing a new context object; every constructor and method arg stays a
/// scalar, a reference, or an `Option<Id>`.
pub struct QuoteAuthCtx<'a> {
    /// Federation context (HTTP policy, media client, clock, store handle).
    pub ctx: &'a FedContext,
    /// Persistence for quotes, statuses and the re-verify enqueue.
    pub store: &'a dyn Store,
    /// The quote this hop re-verifies.
    pub quote_id: Id,
    /// Hop index in this chain (stops at [`QUOTE_REVERIFY_MAX_HOPS`]).
    pub hop: u64,
    /// Approval fetches spent in this chain (stops at
    /// [`QUOTE_REVERIFY_FETCH_BUDGET`]).
    pub fetches: u64,
    /// The nested quote to continue with after this hop settles.
    pub chain: Option<Id>,
    /// Wall-clock time for enqueues and tracing.
    pub now_ms: u64,
}

impl<'a> QuoteAuthCtx<'a> {
    /// Bundle one hop's scalars without behavior change.
    pub fn new(
        ctx: &'a FedContext,
        store: &'a dyn Store,
        quote_id: Id,
        hop: u64,
        fetches: u64,
        chain: Option<Id>,
        now_ms: u64,
    ) -> Self {
        Self {
            ctx,
            store,
            quote_id,
            hop,
            fetches,
            chain,
            now_ms,
        }
    }

    /// Planned hops for this chain as an iterator (current, then chained).
    ///
    /// Yields this hop, then — when `chain` names a nested quote — one more
    /// [`QuoteAuthCtx`] with `hop + 1` and the same `fetches` count (the async
    /// step bumps `fetches` only when it actually performs the approval
    /// fetch, exactly as `check_authorization_remote` does). Lets the parent
    /// drive the chain with a plain `for` loop instead of manual recursion.
    pub fn steps(self) -> impl Iterator<Item = QuoteAuthCtx<'a>> {
        let next = self.chain.map(|next_quote_id| QuoteAuthCtx {
            ctx: self.ctx,
            store: self.store,
            quote_id: next_quote_id,
            hop: self.hop + 1,
            fetches: self.fetches,
            chain: None,
            now_ms: self.now_ms,
        });
        std::iter::once(self).chain(next)
    }

    /// Run one authorization step: fetch the quote's authorization document
    /// and settle it.
    ///
    /// Verbatim behavior of `threads.rs::check_authorization_remote`: 404/410
    /// revokes (the quoted author withdrew it); a usable document keeps the
    /// quote; anything else retries with backoff or keeps the quote without
    /// touching state. A refused authorization URL is never evidence of
    /// death. On success the declared `chain` continues one hop deeper with
    /// the same enqueue payloads (`hop + 1`, `fetches` or `fetches + 1`).
    /// Returns the enqueued next hop when the chain continues, `None` when
    /// the chain ends here.
    pub async fn authorize_step(self) -> Result<Option<QuoteAuthCtx<'a>>, DeliveryError> {
        use super::threads_auth_stages as stages;

        match stages::load_and_gate(&self)? {
            stages::GateOutcome::End => Ok(None),
            stages::GateOutcome::ForwardChain { next } => {
                Ok(Some(stages::enqueue_next(&self, next, self.fetches)?))
            }
            stages::GateOutcome::Fetch { approval } => {
                match stages::fetch_authorization(&self, &approval).await? {
                    stages::FetchOutcome::Keep => Ok(None),
                    stages::FetchOutcome::Revoked => {
                        let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
                        revoke_verified(self.store, self.quote_id, self.now_ms).map_err(retry)?;
                        Ok(None)
                    }
                    stages::FetchOutcome::Document(body) => stages::settle_document(self, &body),
                }
            }
        }
    }
}

/// Whether a re-verify chain is exhausted: hop cap or fetch budget reached.
///
/// Verbatim guard from `run_quote_reverify`: the chain stops with every
/// remaining quote kept as-is. Scalar params only.
pub fn budget_exhausted(hop: u64, fetches: u64) -> bool {
    hop >= QUOTE_REVERIFY_MAX_HOPS || fetches >= QUOTE_REVERIFY_FETCH_BUDGET
}

/// Revoke a quote whose death is positively evidenced, and queue its
/// `Delete` of the `QuoteAuthorization` for the quoting server (a no-op when
/// the quoted author is remote — the event worker only sends for locals).
///
/// Verbatim from `threads::revoke_verified`: no-ops on a missing row or a
/// non-accepted quote, keeps the quoted status's counter in step via
/// `set_quote_state`, and enqueues the same `quote_revoke` event.
pub(crate) fn revoke_verified(
    store: &dyn Store,
    quote_id: Id,
    now_ms: u64,
) -> crate::domain::Result<()> {
    use crate::domain::QuoteState;

    let Some(quote) = store.quote_by_id(quote_id)? else {
        return Ok(());
    };
    if quote.state != QuoteState::Accepted {
        return Ok(());
    }
    store.set_quote_state(quote.id, QuoteState::Revoked, None, None)?;
    store.enqueue_event(
        quote.quoted_account_id,
        serde_json::json!({"event": "quote_revoke", "quote_id": quote.id.value()}),
        now_ms,
    )?;
    tracing::info!(
        target: "birchtree::federation",
        direction = "inbound",
        kind = "QuoteReverify",
        quote = quote.id.value(),
        outcome = "revoked: quoted status is gone",
        "thread"
    );
    Ok(())
}

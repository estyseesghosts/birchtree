//! Stage helpers for [`QuoteAuthCtx::authorize_step`] (S7/S12 leftover sweep).
//!
//! `threads_auth::authorize_step` mixed three concerns in 136 lines: loading
//! and gating the hop, fetching the authorization document, and settling a
//! fetched document. This module holds one stage per concern as a free
//! function over `&QuoteAuthCtx` (one `impl` block cannot span files, and no
//! new context object is wanted, so the stages borrow the existing one).
//! Reason strings, hop/budget constants and enqueue payloads are byte-identical
//! to `threads.rs::check_authorization_remote`.

use url::Url;

use crate::domain::{Id, JobKind, QuoteState};
use crate::federation::delivery::DeliveryError;
use crate::federation::threads_auth::{QUOTE_REVERIFY_MAX_BODY_BYTES, QuoteAuthCtx};

/// What [`load_and_gate`] decided for this hop.
pub(super) enum GateOutcome {
    /// Nothing to do: missing row, non-accepted quote, or no chained quote.
    End,
    /// A local-policy quote with no remote authorization to check: continue
    /// with the chained quote at the same fetch budget.
    ForwardChain { next: Id },
    /// Fetch the remote authorization document at this URI.
    Fetch { approval: String },
}

/// What [`fetch_authorization`] observed.
pub(super) enum FetchOutcome {
    /// Keep the quote without touching state (policy refusal, non-terminal
    /// client answer, body cap breach).
    Keep,
    /// The quoted status is gone (404/410): the caller revokes.
    Revoked,
    /// A fetched document to settle (parse plus chain accounting).
    Document(Vec<u8>),
}

fn retry(error: crate::domain::Error) -> DeliveryError {
    DeliveryError::Retry(error.to_string())
}

/// Load the quote row and apply the state gate (verbatim first stage).
pub(super) fn load_and_gate(ctx: &QuoteAuthCtx<'_>) -> Result<GateOutcome, DeliveryError> {
    let Some(quote) = ctx.store.quote_by_id(ctx.quote_id).map_err(retry)? else {
        return Ok(GateOutcome::End);
    };
    if quote.state != QuoteState::Accepted {
        return Ok(GateOutcome::End);
    }
    let Some(approval) = quote.approval_uri.clone() else {
        // A local-policy quote carries no remote authorization to check.
        // Still continue a nested chain, if any.
        return Ok(match ctx.chain {
            Some(next) => GateOutcome::ForwardChain { next },
            None => GateOutcome::End,
        });
    };
    Ok(GateOutcome::Fetch { approval })
}

/// Fetch the authorization document (verbatim second stage: policy gate,
/// status dispatch, bounded read).
pub(super) async fn fetch_authorization(
    ctx: &QuoteAuthCtx<'_>,
    approval: &str,
) -> Result<FetchOutcome, DeliveryError> {
    use futures_util::StreamExt as _;

    let url = Url::parse(approval).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
    if let Err(policy) =
        crate::federation::policy::check_url(&url, ctx.ctx.http.allow_http, &ctx.ctx.http.allowlist)
    {
        // A refused authorization URL is not evidence of death: keep the quote.
        tracing::info!(
            target: "birchtree::federation",
            direction = "inbound",
            kind = "QuoteReverify",
            quote = ctx.quote_id.value(),
            outcome = %format!("kept: authorization url refused by policy: {policy}"),
            "thread"
        );
        return Ok(FetchOutcome::Keep);
    }
    let response = ctx
        .ctx
        .media_client
        .get(url)
        .header(
            reqwest::header::ACCEPT,
            "application/activity+json, application/ld+json",
        )
        .send()
        .await
        .map_err(|e| DeliveryError::Retry(e.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
        return Ok(FetchOutcome::Revoked);
    }
    if status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err(DeliveryError::Retry(format!(
            "authorization server answered {status}"
        )));
    }
    if !status.is_success() {
        // Any other client answer is not evidence of death: keep the quote.
        return Ok(FetchOutcome::Keep);
    }
    if let Some(declared) = response.content_length()
        && declared > QUOTE_REVERIFY_MAX_BODY_BYTES
    {
        return Ok(FetchOutcome::Keep);
    }
    // Bounded read: the authorization document is tiny; anything past the
    // remote-document cap is not evidence of anything — keep the quote.
    let mut body: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DeliveryError::Retry(e.to_string()))?;
        if body.len() as u64 + chunk.len() as u64 > QUOTE_REVERIFY_MAX_BODY_BYTES {
            return Ok(FetchOutcome::Keep);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchOutcome::Document(body))
}

/// Enqueue the next chain hop with `fetches` spent so far and build its
/// context (shared by the ungated local-policy path and the settled path).
pub(super) fn enqueue_next<'a>(
    ctx: &QuoteAuthCtx<'a>,
    next: Id,
    fetches: u64,
) -> Result<QuoteAuthCtx<'a>, DeliveryError> {
    ctx.store
        .enqueue(
            JobKind::QuoteReverify,
            serde_json::json!({ "quote_id": next.value(), "hop": ctx.hop + 1, "fetches": fetches }),
            ctx.now_ms,
        )
        .map_err(retry)?;
    Ok(QuoteAuthCtx {
        ctx: ctx.ctx,
        store: ctx.store,
        quote_id: next,
        hop: ctx.hop + 1,
        fetches,
        chain: None,
        now_ms: ctx.now_ms,
    })
}

/// Settle a fetched document (verbatim third stage): an unparsable body
/// keeps, otherwise the declared chain continues with one more fetch spent.
pub(super) fn settle_document<'a>(
    ctx: QuoteAuthCtx<'a>,
    body: &[u8],
) -> Result<Option<QuoteAuthCtx<'a>>, DeliveryError> {
    let _doc: serde_json::Value = match serde_json::from_slice(body) {
        Ok(doc) => doc,
        Err(_) => return Ok(None),
    };
    if let Some(next) = ctx.chain {
        let advanced = enqueue_next(&ctx, next, ctx.fetches + 1)?;
        tracing::info!(
            target: "birchtree::federation",
            direction = "inbound",
            kind = "QuoteReverify",
            quote = ctx.quote_id.value(),
            outcome = "kept: authorization still resolves",
            "thread"
        );
        return Ok(Some(advanced));
    }
    tracing::info!(
        target: "birchtree::federation",
        direction = "inbound",
        kind = "QuoteReverify",
        quote = ctx.quote_id.value(),
        outcome = "kept: authorization still resolves",
        "thread"
    );
    Ok(None)
}

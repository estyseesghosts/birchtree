//! Quote use cases: listing accepted quotes of a status, revoking one, and
//! setting a status's quote interaction policy.

use crate::application::ports::{PairState, Store};
use crate::application::policy;
use crate::domain::{Error, Id, QuotePolicy, QuoteRecord, Result, Status};

/// `GET /api/v1/statuses/:id/quotes` — accepted quotes of `quoted_status_id`
/// with their quoting statuses, newest first and cursor-bounded.
///
/// The quoted status's owner sees every accepted quote; other viewers only
/// see quoting statuses they themselves may fetch. The root status gates the
/// whole listing (`NotFound("status")` when missing/deleted/invisible).
pub fn quotes_of(
    store: &dyn Store,
    viewer_id: Option<Id>,
    quoted_status_id: Id,
    max_id: Option<Id>,
    limit: usize,
    now_ms: u64,
) -> Result<Vec<(QuoteRecord, Status)>> {
    let root = store
        .status_by_id(quoted_status_id)?
        .ok_or_else(|| Error::NotFound("status".into()))?;
    let root_pair = match viewer_id {
        Some(viewer) => store.pair_state(root.account_id, viewer, now_ms)?,
        None => PairState::default(),
    };
    let root_facts = match viewer_id {
        Some(viewer) => policy::viewer_side_facts(store, viewer, root.account_id, now_ms)?,
        None => policy::FetchFacts::default(),
    };
    if !policy::can_fetch_full(viewer_id, &root, root_pair, root_facts) {
        return Err(Error::NotFound("status".into()));
    }

    let quotes = store.quotes_of_status(quoted_status_id, max_id, limit)?;
    if viewer_id == Some(root.account_id) {
        return Ok(quotes);
    }

    let mut visible = Vec::with_capacity(quotes.len());
    for (quote, quoting_status) in quotes {
        let (pair, facts) = match viewer_id {
            Some(viewer) => (
                store.pair_state(quoting_status.account_id, viewer, now_ms)?,
                policy::viewer_side_facts(store, viewer, quoting_status.account_id, now_ms)?,
            ),
            None => (PairState::default(), policy::FetchFacts::default()),
        };
        if policy::can_fetch_full(viewer_id, &quoting_status, pair, facts) {
            visible.push((quote, quoting_status));
        }
    }
    Ok(visible)
}

/// `DELETE /api/v1/statuses/:id/quotes/:quoting_status_id` — revoke an
/// accepted quote of the viewer's own status.
///
/// Only the QUOTED status's author may revoke (the quoting status's author
/// may not); unknown root/quote -> `NotFound("Record not found")`, anyone
/// else -> `Forbidden("This action is not allowed")`.
pub fn revoke_quote(
    store: &dyn Store,
    viewer_id: Id,
    quoted_status_id: Id,
    quoting_status_id: Id,
    now_ms: u64,
) -> Result<QuoteRecord> {
    let root = store
        .status_by_id(quoted_status_id)?
        .filter(|status| !status.is_deleted())
        .ok_or_else(|| Error::NotFound("Record not found".into()))?;
    if !policy::is_owner(viewer_id, &root) {
        return Err(Error::Forbidden("This action is not allowed".into()));
    }
    store
        .revoke_quote(quoted_status_id, quoting_status_id, now_ms)
        .map_err(|err| match err {
            Error::NotFound(_) => Error::NotFound("Record not found".into()),
            other => other,
        })
}

/// Update a status's `quote_approval_policy` (`public`/`followers`/`nobody`)
/// without an edit snapshot or `edited_at` bump.
///
/// Own status only (`Forbidden("This action is not allowed")` otherwise),
/// missing/deleted -> `NotFound("status")`, unknown value ->
/// `InvalidInput("Record invalid")` (422 `{"error":"Record invalid"}`).
pub fn set_interaction_policy(
    store: &dyn Store,
    viewer_id: Id,
    status_id: Id,
    quote_approval_policy: &str,
    now_ms: u64,
) -> Result<Status> {
    let status = store
        .status_by_id(status_id)?
        .filter(|status| !status.is_deleted())
        .ok_or_else(|| Error::NotFound("status".into()))?;
    if !policy::is_owner(viewer_id, &status) {
        return Err(Error::Forbidden("This action is not allowed".into()));
    }
    let policy_value = QuotePolicy::from_wire(quote_approval_policy)
        .ok_or_else(|| Error::InvalidInput("Record invalid".into()))?;
    // The policy write carries no timestamp; `now_ms` keeps the use-case
    // signature uniform with the rest of the module.
    let _ = now_ms;
    store.set_quote_policy(status_id, policy_value)
}

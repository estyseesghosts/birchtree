//! Instance endpoints (`GET /api/v1/instance`, `GET /api/v2/instance`).
//!
//! Both are public. They expose static, config-derived facts plus live counts
//! and an optional contact account.

use std::collections::HashSet;

use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::api::mastodon::ApiState;
use crate::api::mastodon::dto::{self, InstanceStats};
use crate::application::ports::Store;
use crate::domain::{Account, Id};

/// Bounded full-table scans backing the instance counters (round 3, D31).
const INSTANCE_SCAN_CAP: usize = 200_000;

/// `GET /api/v1/instance`.
pub async fn v1(State(state): State<ApiState>) -> Response {
    let (stats, contact) = gather(&state);
    axum::Json(dto::instance_v1_json(&state.server, stats, contact.as_ref())).into_response()
}

/// `GET /api/v2/instance`.
pub async fn v2(State(state): State<ApiState>) -> Response {
    let (stats, contact) = gather(&state);
    axum::Json(dto::instance_v2_json(&state.server, stats, contact.as_ref())).into_response()
}

/// Live counts and the resolved contact account, if one is configured.
///
/// Round 3 (D31): real local-only counts — local accounts, non-deleted
/// statuses by local authors, distinct known remote domains — computed from
/// the same bounded metric scans as the admin measures. On scan failure the
/// counters fall back to the cheap totals rather than failing the document.
fn gather(state: &ApiState) -> (InstanceStats, Option<Account>) {
    let store: &dyn Store = &*state.context.store;
    let stats = match (
        store.admin_accounts_overview(INSTANCE_SCAN_CAP),
        store.admin_status_metrics(INSTANCE_SCAN_CAP),
    ) {
        (Ok(accounts), Ok(statuses)) => {
            let local_ids: HashSet<Id> = accounts
                .iter()
                .filter(|account| account.local)
                .map(|account| account.id)
                .collect();
            let domains: HashSet<&str> = accounts
                .iter()
                .filter(|account| !account.local)
                .map(|account| account.domain.as_str())
                .collect();
            InstanceStats {
                user_count: local_ids.len() as u64,
                status_count: statuses
                    .iter()
                    .filter(|row| local_ids.contains(&row.account_id))
                    .count() as u64,
                domain_count: domains.len() as u64,
                active_month: active_month_count(store, state.context.now_ms()),
            }
        }
        _ => InstanceStats {
            user_count: store.account_count().unwrap_or(0),
            status_count: 0,
            domain_count: 0,
            active_month: active_month_count(store, state.context.now_ms()),
        },
    };
    let contact = state
        .server
        .site_contact_username
        .as_deref()
        .and_then(|username| {
            let normalized = crate::domain::account::normalize_username(username);
            store
                .account_by_handle(&normalized, &state.server.domain)
                .ok()
                .flatten()
        });
    (stats, contact)
}

/// Unique sign-in-log accounts over the trailing 4 weeks, like NodeInfo
/// `activeMonth` (round 3, D31 follow-up). Failure degrades to zero.
fn active_month_count(store: &dyn Store, now: u64) -> u64 {
    use std::collections::HashSet;
    const WEEK_MS: u64 = 7 * 86_400_000;
    store
        .admin_sign_ins(INSTANCE_SCAN_CAP)
        .map(|rows| {
            rows.iter()
                .filter(|(_, at)| *at >= now.saturating_sub(4 * WEEK_MS) && *at <= now)
                .map(|(who, _)| *who)
                .collect::<HashSet<_>>()
                .len() as u64
        })
        .unwrap_or(0)
}

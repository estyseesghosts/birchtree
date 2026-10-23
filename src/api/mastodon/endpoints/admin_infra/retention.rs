use std::collections::HashMap;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::application::ports::{StatusMetricRow, Store};
use crate::domain::{AdminAccountOverview, Id};

use super::{DAY_MS, METRIC_SCAN_CAP, date_only, month_start_ms, require_metrics};

struct Snapshots {
    accounts: Vec<AdminAccountOverview>,
    status_rows: Vec<StatusMetricRow>,
}

/// Bounded account + status scans shared by every cohort.
fn load_snapshots(store: &dyn Store) -> ApiResult<Snapshots> {
    Ok(Snapshots {
        accounts: store
            .admin_accounts_overview(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        status_rows: store
            .admin_status_metrics(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
    })
}

/// Required `start_at`/`end_at` (upstream `params.require` → 400 when
/// missing), clamped like upstream (day → trailing 31 days, month →
/// trailing 12 months). Returns `(start_ms, end_ms, is_month)`.
fn parse_window(body: &[(String, String)]) -> ApiResult<(u64, u64, bool)> {
    let start_raw = forms::get(body, "start_at")
        .unwrap_or("")
        .trim()
        .to_string();
    let end_raw = forms::get(body, "end_at").unwrap_or("").trim().to_string();
    if start_raw.is_empty() || end_raw.is_empty() {
        let missing = if start_raw.is_empty() {
            "start_at"
        } else {
            "end_at"
        };
        return Err(ApiError::bad_request(format!(
            "param is missing or the value is empty: {missing}"
        )));
    }
    let parse_dt = |raw: &str| -> Option<u64> {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
            return u64::try_from(dt.timestamp_millis()).ok();
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            return u64::try_from(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis()).ok();
        }
        raw.parse::<i64>().ok().and_then(|num| {
            let ms = if num.abs() < 10_000_000_000 {
                num * 1_000
            } else {
                num
            };
            u64::try_from(ms).ok()
        })
    };
    let (Some(start_ms), Some(end_ms)) = (parse_dt(&start_raw), parse_dt(&end_raw)) else {
        return Err(ApiError::bad_request("Invalid start_at or end_at"));
    };
    if start_ms > end_ms {
        return Err(ApiError::bad_request("Invalid start_at or end_at"));
    }
    let is_month = forms::get(body, "frequency").unwrap_or("day") == "month";
    // Upstream clamp: day → trailing 31 days, month → trailing 12 months.
    let start_ms =
        start_ms.max(end_ms.saturating_sub(if is_month { 366 * DAY_MS } else { 31 * DAY_MS }));
    Ok((start_ms, end_ms, is_month))
}

/// Cohort rows: accounts grouped by creation day (or month) Start, with the
/// share still active in each later period.
fn render_cohorts(
    store: &dyn Store,
    snapshots: &Snapshots,
    start_ms: u64,
    end_ms: u64,
    is_month: bool,
) -> Vec<Value> {
    let frequency = if is_month { "month" } else { "day" };
    let accounts = &snapshots.accounts;
    let status_rows = &snapshots.status_rows;

    // Cohort key: creation day-start, or creation month-start.
    let cohort_of = |ms: u64| -> u64 {
        if frequency == "month" {
            month_start_ms(ms)
        } else {
            ms - ms % DAY_MS
        }
    };
    let mut cohorts: HashMap<u64, Vec<Id>> = HashMap::new();
    for account in accounts {
        if account.created_at_ms < start_ms || account.created_at_ms > end_ms {
            continue;
        }
        cohorts
            .entry(cohort_of(account.created_at_ms))
            .or_default()
            .push(account.id);
    }
    let mut cohort_keys: Vec<u64> = cohorts.keys().copied().collect();
    cohort_keys.sort_unstable();

    let active_periods = collect_active_periods(store, status_rows);

    let mut out = Vec::new();
    for cohort_start in cohort_keys {
        let members = &cohorts[&cohort_start];
        let created_count = members.len() as u64;
        let mut data = Vec::new();
        for period in cohort_periods(cohort_start, end_ms, is_month) {
            let period_end = period
                .saturating_add(cohort_step_ms(period, is_month))
                .saturating_sub(1);
            let active_count = members
                .iter()
                .filter(|id| {
                    active_periods
                        .get(*id)
                        .is_some_and(|times| times.iter().any(|&t| t >= period && t <= period_end))
                })
                .count() as u64;
            let rate = if created_count > 0 {
                active_count as f64 / created_count as f64
            } else {
                0.0
            };
            data.push(json!({
                "date": date_only(period),
                "rate": rate,
                "value": active_count.to_string(),
            }));
        }
        out.push(json!({
            "period": date_only(cohort_start),
            "frequency": frequency,
            "data": data,
        }));
    }
    out
}

/// Active periods per account: authored statuses plus password sign-ins
/// (round 3 sign-in cohorts; either signal counts).
fn collect_active_periods(
    store: &dyn Store,
    status_rows: &[StatusMetricRow],
) -> HashMap<Id, Vec<u64>> {
    let mut active_periods: HashMap<Id, Vec<u64>> = HashMap::new();
    for row in status_rows {
        active_periods
            .entry(row.account_id)
            .or_default()
            .push(row.created_at_ms);
    }
    if let Ok(sign_ins) = store.admin_sign_ins(METRIC_SCAN_CAP) {
        for (who, at) in sign_ins {
            active_periods.entry(who).or_default().push(at);
        }
    }
    for periods in active_periods.values_mut() {
        periods.sort_unstable();
    }
    active_periods
}

/// Retention periods from one cohort start to `end_ms`, one step each.
fn cohort_periods(cohort_start: u64, end_ms: u64, is_month: bool) -> Vec<u64> {
    let mut periods = Vec::new();
    let mut cursor = cohort_start;
    while cursor <= end_ms {
        periods.push(cursor);
        cursor = cursor.saturating_add(cohort_step_ms(cursor, is_month).max(1));
        if periods.len() > 366 {
            break;
        }
    }
    periods
}

/// One retention step from `from_ms`: a month (via the 32-day probe) or a day.
fn cohort_step_ms(from_ms: u64, is_month: bool) -> u64 {
    if is_month {
        month_start_ms(from_ms.saturating_add(32 * DAY_MS)).saturating_sub(from_ms)
    } else {
        DAY_MS
    }
}

/// `POST /api/v1/admin/retention` with read scope upstream (kept).
///
/// Cohorts of accounts by creation day (or month), with the share still
/// active in each later period. Upstream measures sign-ins
/// (`users.current_sign_in_at`); this server unions two real activity
/// signals: the password sign-in log (round 2 telemetry — API calls on an
/// existing token are not sign-ins) and authored statuses (posting implies an
/// active session; bearer tokens here never expire, so sign-ins alone would
/// miss API-only users). Members with neither signal are never active —
///
/// `start_at`/`end_at` are required (upstream
/// `params.require` → 400 when missing); `frequency` is `day` (default) or
/// `month`. The window is clamped like upstream (31 days / 12 months back).
/// Moderators pass ([`require_metrics`]).
pub async fn retention(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = require_metrics(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let (start_ms, end_ms, is_month) = match parse_window(&body) {
        Ok(window) => window,
        Err(err) => return err.into_response(),
    };
    let snapshots = match load_snapshots(store) {
        Ok(snapshots) => snapshots,
        Err(err) => return err.into_response(),
    };
    Json(render_cohorts(
        store, &snapshots, start_ms, end_ms, is_month,
    ))
    .into_response()
}

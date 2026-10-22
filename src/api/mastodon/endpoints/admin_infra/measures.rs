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
use crate::domain::{
    AdminAccountOverview, AdminMediaOwner, AdminReportTarget, FollowMetricRow, Id,
};

use super::{
    DAY_MS, METRIC_SCAN_CAP, bool_cast, daily_counts, daily_data, daily_distinct,
    daily_distinct_domains, daily_unique, day_buckets, human_size, metric_window, require_metrics,
    sum_in_window, tag_metric_rows, tag_name_for_id,
};

struct Snapshots {
    accounts: Vec<AdminAccountOverview>,
    status_rows: Vec<StatusMetricRow>,
    report_targets: Vec<AdminReportTarget>,
    media_owners: Vec<AdminMediaOwner>,
    follows: Vec<FollowMetricRow>,
    sign_ins: Vec<(Id, u64)>,
    resolved_times: Vec<u64>,
}

/// One bounded scan per source table, shared by every requested key.
fn load_snapshots(store: &dyn Store) -> ApiResult<Snapshots> {
    Ok(Snapshots {
        accounts: store
            .admin_accounts_overview(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        status_rows: store
            .admin_status_metrics(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        report_targets: store
            .admin_report_targets(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        media_owners: store
            .admin_media_owners(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        follows: store
            .admin_follow_metrics(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        sign_ins: store
            .admin_sign_ins(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        resolved_times: store
            .admin_report_resolved_times(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
    })
}

struct Buckets {
    start: u64,
    end: u64,
    prev_start: u64,
    prev_end: u64,
    buckets: Vec<u64>,
    domains: HashMap<Id, (String, bool)>,
    wanted: String,
    include_sub: bool,
    local_domain: String,
}

/// Day buckets, previous-window bounds, and the domain-scope inputs shared by
/// every requested key.
fn bucketize(
    body: &[(String, String)],
    local_domain: &str,
    snapshots: &Snapshots,
    start: u64,
    end: u64,
) -> Buckets {
    let buckets = day_buckets(start, end);
    let window_len = end.saturating_sub(start);
    let prev_start = start.saturating_sub(window_len);
    let prev_end = start.saturating_sub(1);
    let domain_param = forms::get(body, "domain").map(|item| item.to_string());
    let include_sub = forms::get(body, "include_subdomains").is_some_and(bool_cast);
    let domains: HashMap<Id, (String, bool)> = snapshots
        .accounts
        .iter()
        .map(|a| (a.id, (a.domain.clone(), a.local)))
        .collect();
    let wanted = domain_param.unwrap_or_else(|| local_domain.to_string());
    Buckets {
        start,
        end,
        prev_start,
        prev_end,
        buckets,
        domains,
        wanted,
        include_sub,
        local_domain: local_domain.to_string(),
    }
}

/// Render one upstream-shaped measure row per requested key. Unknown keys
/// and unresolvable `tag_*` keys are omitted like upstream's `filter_map`
/// (never fabricated, never zero-filled).
fn render_keys(
    store: &dyn Store,
    ctx: Buckets,
    snapshots: &Snapshots,
    body: &[(String, String)],
    keys: &[String],
) -> Response {
    let Buckets {
        start,
        end,
        prev_start,
        prev_end,
        buckets,
        domains,
        wanted,
        include_sub,
        local_domain,
    } = ctx;
    let accounts = &snapshots.accounts;
    let status_rows = &snapshots.status_rows;
    let report_targets = &snapshots.report_targets;
    let media_owners = &snapshots.media_owners;
    let follows = &snapshots.follows;
    let sign_ins = &snapshots.sign_ins;
    let resolved_times = &snapshots.resolved_times;
    let wanted: &str = &wanted;
    let local_domain: &str = &local_domain;
    let in_scope = |domain: &str, local: bool| -> bool {
        if domain == wanted {
            return true;
        }
        if include_sub {
            if domain.ends_with(&format!(".{wanted}")) {
                return true;
            }
            // A bare local-domain request with subdomains also counts local
            // accounts (upstream's `by_domain_and_subdomains`).
            if local && wanted == local_domain {
                return true;
            }
        }
        false
    };
    let account_in_scope = |id: &Id| -> bool {
        domains
            .get(id)
            .is_some_and(|(domain, local)| in_scope(domain, *local))
    };

    let mut out = Vec::new();
    for key in keys {
        match key.as_str() {
            "new_users" => {
                let times: Vec<u64> = accounts.iter().map(|a| a.created_at_ms).collect();
                let total = sum_in_window(&times, start, end);
                let previous = sum_in_window(&times, prev_start, prev_end);
                let counts = daily_counts(&times, &buckets);
                out.push(json!({
                    "key": "new_users",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_accounts" => {
                let scoped: Vec<&AdminAccountOverview> = accounts
                    .iter()
                    .filter(|a| in_scope(&a.domain, a.local))
                    .collect();
                let total = scoped.len() as u64;
                let times: Vec<u64> = scoped.iter().map(|a| a.created_at_ms).collect();
                let counts = daily_counts(&times, &buckets);
                out.push(json!({
                    "key": "instance_accounts",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_statuses" => {
                let scoped: Vec<&StatusMetricRow> = status_rows
                    .iter()
                    .filter(|row| account_in_scope(&row.account_id))
                    .collect();
                let total = scoped.len() as u64;
                let times: Vec<u64> = scoped.iter().map(|row| row.created_at_ms).collect();
                let windowed: Vec<u64> = times
                    .into_iter()
                    .filter(|&t| t >= start && t <= end)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_statuses",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_reports" => {
                let scoped: Vec<&AdminReportTarget> = report_targets
                    .iter()
                    .filter(|report| account_in_scope(&report.target_account_id))
                    .collect();
                let total = scoped.len() as u64;
                let times: Vec<u64> = scoped.iter().map(|report| report.created_at_ms).collect();
                let windowed: Vec<u64> = times
                    .into_iter()
                    .filter(|&t| t >= start && t <= end)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_reports",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "opened_reports" => {
                let times: Vec<u64> = report_targets.iter().map(|r| r.created_at_ms).collect();
                let total = sum_in_window(&times, start, end);
                let previous = sum_in_window(&times, prev_start, prev_end);
                let counts = daily_counts(&times, &buckets);
                out.push(json!({
                    "key": "opened_reports",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_media_attachments" => {
                let scoped: Vec<&AdminMediaOwner> = media_owners
                    .iter()
                    .filter(|media| account_in_scope(&media.owner_id))
                    .collect();
                let total_bytes: u64 = scoped.iter().map(|media| media.file_size).sum();
                let windowed: Vec<u64> = scoped
                    .iter()
                    .filter(|media| media.created_at_ms >= start && media.created_at_ms <= end)
                    .map(|media| media.created_at_ms)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_media_attachments",
                    "unit": "bytes",
                    "total": total_bytes.to_string(),
                    "human_value": human_size(total_bytes),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "resolved_reports" => {
                // Upstream `ResolvedReportsMeasure`: resolutions by
                // `action_taken_at`, with a previous-window total.
                let total = sum_in_window(resolved_times, start, end);
                let previous = sum_in_window(resolved_times, prev_start, prev_end);
                let counts = daily_counts(resolved_times, &buckets);
                out.push(json!({
                    "key": "resolved_reports",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "active_users" => {
                // D39 sign-in telemetry: daily *unique* password logins
                // (upstream's `:unique` activity tracker). Totals are sums
                // of the daily uniques, like upstream.
                let in_window: Vec<(Id, u64)> = sign_ins
                    .iter()
                    .copied()
                    .filter(|(_, t)| *t >= start && *t <= end)
                    .collect();
                let in_prev: Vec<(Id, u64)> = sign_ins
                    .iter()
                    .copied()
                    .filter(|(_, t)| *t >= prev_start && *t <= prev_end)
                    .collect();
                let counts = daily_unique(&in_window, &buckets);
                let total: u64 = counts.iter().sum();
                let previous: u64 = daily_unique(&in_prev, &buckets).iter().sum();
                out.push(json!({
                    "key": "active_users",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_follows" => {
                // Upstream `InstanceFollowsMeasure`: follows by *target*
                // domain. `total_in_time_range?` is false, so the total is
                // the all-time count and there is no `previous_total`.
                let scoped: Vec<&FollowMetricRow> = follows
                    .iter()
                    .filter(|row| {
                        domains
                            .get(&row.target_id)
                            .is_some_and(|(domain, local)| in_scope(domain, *local))
                    })
                    .collect();
                let total = scoped.len() as u64;
                let windowed: Vec<u64> = scoped
                    .iter()
                    .map(|row| row.created_at_ms)
                    .filter(|&t| t >= start && t <= end)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_follows",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "instance_followers" => {
                // Upstream `InstanceFollowersMeasure`: follows by
                // *follower* domain; otherwise shaped like
                // `instance_follows`.
                let scoped: Vec<&FollowMetricRow> = follows
                    .iter()
                    .filter(|row| {
                        domains
                            .get(&row.follower_id)
                            .is_some_and(|(domain, local)| in_scope(domain, *local))
                    })
                    .collect();
                let total = scoped.len() as u64;
                let windowed: Vec<u64> = scoped
                    .iter()
                    .map(|row| row.created_at_ms)
                    .filter(|&t| t >= start && t <= end)
                    .collect();
                let counts = daily_counts(&windowed, &buckets);
                out.push(json!({
                    "key": "instance_followers",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            "interactions" => {
                // D39 served (round 4): the unified interaction journal —
                // non-self replies, favourites, reblogs, votes and new
                // follows/requests (upstream's five `activity:interactions`
                // tracker call sites). Shape matches upstream's
                // `MeasureSerializer` (`key/unit/total/previous_total/data`,
                // daily values as strings): totals are sums of the daily
                // buckets, like `active_users` above. One bounded range scan
                // per bucket plus one for the previous window; the end bound
                // is inclusive on the wire, exclusive in the store call.
                let end_excl = end.saturating_add(1);
                let mut counts = Vec::with_capacity(buckets.len());
                for &day in &buckets {
                    let low = day.max(start);
                    let high = day.saturating_add(DAY_MS).min(end_excl);
                    let bucket_count = if low < high {
                        match store.count_interactions(None, low, high) {
                            Ok(bucket_count) => bucket_count,
                            Err(err) => return ApiError::from(err).into_response(),
                        }
                    } else {
                        0
                    };
                    counts.push(bucket_count);
                }
                let total: u64 = counts.iter().sum();
                let previous =
                    match store.count_interactions(None, prev_start, prev_end.saturating_add(1)) {
                        Ok(previous) => previous,
                        Err(err) => return ApiError::from(err).into_response(),
                    };
                out.push(json!({
                    "key": "interactions",
                    "unit": Value::Null,
                    "total": total.to_string(),
                    "previous_total": previous.to_string(),
                    "data": daily_data(&buckets, &counts),
                }));
            }
            _ => {
                // `tag_accounts`, `tag_uses` and `tag_servers` are served
                // above when `<key>[id]>` names a tag; without a resolvable id
                // they stay omitted ([`REFUSED_TAG_NO_ID`] /
                // [`REFUSED_TAG_UNKNOWN_ID`]). Unknown keys drop like
                // upstream's `filter_map`.
                if let Some(tag_key) = ["tag_accounts", "tag_uses", "tag_servers"]
                    .into_iter()
                    .find(|known| *known == key.as_str())
                {
                    let param_key = format!("{tag_key}[id]");
                    let tag = forms::get(body, param_key.as_str())
                        .and_then(|raw| raw.trim().parse::<Id>().ok())
                        .and_then(|id| tag_name_for_id(store, id));
                    let Some(tag) = tag else {
                        continue;
                    };
                    // Real local counts from the hashtag index (never
                    // fabricated, never zero-filled): uses, distinct
                    // accounts and distinct author domains per day.
                    let rows = tag_metric_rows(store, &tag);
                    let in_win: Vec<(u64, Id)> = rows
                        .iter()
                        .filter(|(t, _, _)| *t >= start && *t <= end)
                        .map(|(t, who, _)| (*t, *who))
                        .collect();
                    let in_prev: Vec<(u64, Id)> = rows
                        .iter()
                        .filter(|(t, _, _)| *t >= prev_start && *t <= prev_end)
                        .map(|(t, who, _)| (*t, *who))
                        .collect();
                    match tag_key {
                        "tag_accounts" => {
                            let pairs: Vec<(Id, u64)> =
                                in_win.iter().map(|(t, who)| (*who, *t)).collect();
                            let prev_pairs: Vec<(Id, u64)> =
                                in_prev.iter().map(|(t, who)| (*who, *t)).collect();
                            let counts = daily_distinct(&pairs, &buckets);
                            let total: u64 = counts.iter().sum();
                            let previous: u64 = daily_distinct(&prev_pairs, &buckets).iter().sum();
                            out.push(json!({
                                "key": "tag_accounts",
                                "unit": Value::Null,
                                "total": total.to_string(),
                                "previous_total": previous.to_string(),
                                "data": daily_data(&buckets, &counts),
                            }));
                        }
                        "tag_uses" => {
                            let times: Vec<u64> = in_win.iter().map(|(t, _)| *t).collect();
                            let prev_times: Vec<u64> = in_prev.iter().map(|(t, _)| *t).collect();
                            let total = times.len() as u64;
                            let previous = prev_times.len() as u64;
                            let counts = daily_counts(&times, &buckets);
                            out.push(json!({
                                "key": "tag_uses",
                                "unit": Value::Null,
                                "total": total.to_string(),
                                "previous_total": previous.to_string(),
                                "data": daily_data(&buckets, &counts),
                            }));
                        }
                        _ => {
                            // `tag_servers`: distinct author domains.
                            let mut ids: Vec<Id> = rows.iter().map(|(_, who, _)| *who).collect();
                            ids.sort_unstable();
                            ids.dedup();
                            let authors = store.accounts_by_ids(&ids).unwrap_or_default();
                            let domain_of = |who: &Id| {
                                authors
                                    .get(who)
                                    .map(|account| account.domain.clone())
                                    .unwrap_or_else(|| local_domain.to_string())
                            };
                            let dom_win: Vec<(String, u64)> =
                                in_win.iter().map(|(t, who)| (domain_of(who), *t)).collect();
                            let dom_prev: Vec<(String, u64)> = in_prev
                                .iter()
                                .map(|(t, who)| (domain_of(who), *t))
                                .collect();
                            let counts = daily_distinct_domains(&dom_win, &buckets);
                            let total = {
                                use std::collections::HashSet;
                                dom_win
                                    .iter()
                                    .map(|(d, _)| d.as_str())
                                    .collect::<HashSet<_>>()
                                    .len() as u64
                            };
                            let previous = {
                                use std::collections::HashSet;
                                dom_prev
                                    .iter()
                                    .map(|(d, _)| d.as_str())
                                    .collect::<HashSet<_>>()
                                    .len() as u64
                            };
                            out.push(json!({
                                "key": "tag_servers",
                                "unit": Value::Null,
                                "total": total.to_string(),
                                "previous_total": previous.to_string(),
                                "data": daily_data(&buckets, &counts),
                            }));
                        }
                    }
                }
                // Omitted like unknown keys upstream (`filter_map` drops
                // them).
            }
        }
    }
    Json(out).into_response()
}

/// `POST /api/v1/admin/measures` with read scope upstream (kept). Computes
/// only keys backed by stored data; unknown or uncomputable keys are omitted
/// (upstream's `filter_map` also drops unknown keys — never fabricated).
/// Uncomputable known keys are recorded as planned gaps in the manifest.
/// Moderators pass ([`require_metrics`]); every other admin-infra route
/// stays admin-only.
pub async fn measures(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = require_metrics(&auth.0, &*state.context.store, &["admin:read"]) {
        return err.into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let now = state.context.now_ms();
    let (start, end) = metric_window(&body, now);
    let snapshots = match load_snapshots(store) {
        Ok(snapshots) => snapshots,
        Err(err) => return err.into_response(),
    };
    let keys: Vec<String> = forms::all(&body, "keys[]")
        .into_iter()
        .map(|item| item.to_string())
        .chain(forms::get(&body, "keys").map(|item| item.to_string()))
        .collect();
    let ctx = bucketize(&body, &state.server.domain, &snapshots, start, end);
    render_keys(store, ctx, &snapshots, &body, &keys)
}

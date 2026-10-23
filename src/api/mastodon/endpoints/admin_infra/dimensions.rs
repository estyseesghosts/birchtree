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
use crate::domain::{AdminAccountOverview, AdminMediaOwner, FollowMetricRow, Id};

use super::{
    METRIC_SCAN_CAP, human_size, metric_window, require_metrics, tag_metric_rows, tag_name_for_id,
    top_counts,
};

struct Snapshots {
    status_rows: Vec<StatusMetricRow>,
    media_owners: Vec<AdminMediaOwner>,
    accounts: Vec<AdminAccountOverview>,
    follows: Vec<FollowMetricRow>,
}

/// One bounded scan per source table, shared by every requested key.
fn load_snapshots(store: &dyn Store) -> ApiResult<Snapshots> {
    Ok(Snapshots {
        status_rows: store
            .admin_status_metrics(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        media_owners: store
            .admin_media_owners(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        accounts: store
            .admin_accounts_overview(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
        follows: store
            .admin_follow_metrics(METRIC_SCAN_CAP)
            .map_err(ApiError::from)?,
    })
}

struct Buckets {
    start: u64,
    end: u64,
    limit: usize,
    domains: HashMap<Id, String>,
    server_domain: String,
}

/// Window bounds, result limit, and the author-domain index shared by every
/// requested key.
fn bucketize(
    body: &[(String, String)],
    snapshots: &Snapshots,
    server_domain: &str,
    start: u64,
    end: u64,
) -> Buckets {
    let limit = forms::get(body, "limit")
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(10)
        .min(500)
        .max(1);
    let domains: HashMap<Id, String> = snapshots
        .accounts
        .iter()
        .map(|a| (a.id, a.domain.clone()))
        .collect();
    Buckets {
        start,
        end,
        limit,
        domains,
        server_domain: server_domain.to_string(),
    }
}

/// Render one upstream-shaped dimension row per requested key. Unknown keys,
/// unresolvable `tag_*` keys and `software_versions` stay omitted like
/// upstream's `filter_map` (never fabricated, never zero-filled).
/// Shared inputs for one dimension key: the store, the window, the
/// domain scope and the pre-loaded snapshot rows.
struct DimensionScope<'a> {
    store: &'a dyn Store,
    start: u64,
    end: u64,
    limit: usize,
    domains: &'a HashMap<Id, String>,
    server_domain: &'a str,
    status_rows: &'a [StatusMetricRow],
    media_owners: &'a [AdminMediaOwner],
    accounts: &'a [AdminAccountOverview],
    follows: &'a [FollowMetricRow],
    body: &'a [(String, String)],
}

fn in_dim_window(scope: &DimensionScope<'_>, t: u64) -> bool {
    t >= scope.start && t <= scope.end
}

/// Render one upstream-shaped dimension row per requested key. Unknown keys
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
        limit,
        domains,
        server_domain,
    } = ctx;
    let scope = DimensionScope {
        store,
        start,
        end,
        limit,
        domains: &domains,
        server_domain: &server_domain,
        status_rows: &snapshots.status_rows,
        media_owners: &snapshots.media_owners,
        accounts: &snapshots.accounts,
        follows: &snapshots.follows,
        body,
    };

    let mut out = Vec::new();
    for key in keys {
        let step = match key.as_str() {
            "languages" | "sources" | "servers" | "space_usage" | "instance_accounts"
            | "instance_languages" => render_count_dimensions(&scope, key, &mut out),
            _ => render_tag_dimensions(&scope, key, &mut out),
        };
        if let Err(response) = step {
            return response;
        }
    }
    Json(out).into_response()
}

/// Count-family dimensions: languages, sources, servers, space usage and the
/// per-domain instance projections.
fn render_count_dimensions(
    scope: &DimensionScope<'_>,
    key: &str,
    out: &mut Vec<Value>,
) -> Result<(), Response> {
    match key {
        "languages" => {
            let mut counts: HashMap<String, u64> = HashMap::new();
            for row in scope.status_rows {
                if !in_dim_window(scope, row.created_at_ms) {
                    continue;
                }
                let locale = row.language.clone().unwrap_or_else(|| "und".to_string());
                *counts.entry(locale).or_insert(0) += 1;
            }
            out.push(json!({
                "key": "languages",
                "data": top_counts(counts, scope.limit),
            }));
        }
        "sources" => {
            let mut counts: HashMap<String, u64> = HashMap::new();
            for row in scope.status_rows {
                if !in_dim_window(scope, row.created_at_ms) {
                    continue;
                }
                let name = row
                    .application_name
                    .clone()
                    .unwrap_or_else(|| "web".to_string());
                *counts.entry(name).or_insert(0) += 1;
            }
            out.push(json!({
                "key": "sources",
                "data": top_counts(counts, scope.limit),
            }));
        }
        "servers" => {
            // Status counts by author domain in the window (upstream's
            // `ServersDimension` shape: domain → status count).
            let mut counts: HashMap<String, u64> = HashMap::new();
            for row in scope.status_rows {
                if !in_dim_window(scope, row.created_at_ms) {
                    continue;
                }
                let domain = scope
                    .domains
                    .get(&row.account_id)
                    .cloned()
                    .unwrap_or_else(|| scope.server_domain.to_string());
                *counts.entry(domain).or_insert(0) += 1;
            }
            out.push(json!({
                "key": "servers",
                "data": top_counts(counts, scope.limit),
            }));
        }
        "space_usage" => {
            let media_bytes: u64 = scope.media_owners.iter().map(|media| media.file_size).sum();
            let mut data = vec![json!({
                "key": "media",
                "human_key": "Media",
                "value": media_bytes.to_string(),
                "unit": "bytes",
                "human_value": human_size(media_bytes),
            })];
            // Round 3 (D39): the live database size, measured from the
            // filesystem — never estimated. postgres/redis/search entries
            // are never reported: this server has neither (no search
            // index on disk — the embedded index lives in the same
            // database file — and no postgres/redis to measure).
            if let Ok(Some(db_bytes)) = scope.store.admin_db_size_bytes() {
                data.push(json!({
                    "key": "database",
                    "human_key": "Database",
                    "value": db_bytes.to_string(),
                    "unit": "bytes",
                    "human_value": human_size(db_bytes),
                }));
            }
            data.truncate(scope.limit);
            out.push(json!({ "key": "space_usage", "data": data }));
        }
        "instance_accounts" => {
            // Upstream `InstanceAccountsDimension`: accounts of one
            // domain (exact match) by follower count, most-followed
            // first. The domain defaults to the local one (upstream
            // `params.require`s it — lenient here, documented).
            let wanted = forms::get(scope.body, "domain").unwrap_or(scope.server_domain);
            let mut counts: HashMap<String, u64> = HashMap::new();
            let mut names: HashMap<Id, String> = HashMap::new();
            for account in scope.accounts {
                if account.domain == wanted {
                    counts.entry(account.username.clone()).or_insert(0);
                    names.insert(account.id, account.username.clone());
                }
            }
            for row in scope.follows {
                if let Some(name) = names.get(&row.target_id) {
                    *counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
            out.push(json!({
                "key": "instance_accounts",
                "data": top_counts(counts, scope.limit),
            }));
        }
        "instance_languages" => {
            // Upstream `InstanceLanguagesDimension`: one domain's
            // non-reblog statuses by language. `human_key` carries the
            // bare language code (upstream localizes it; no locale
            // catalog here — documented).
            let wanted = forms::get(scope.body, "domain").unwrap_or(scope.server_domain);
            let mut counts: HashMap<String, u64> = HashMap::new();
            for row in scope.status_rows {
                if row.is_reblog {
                    continue;
                }
                if scope
                    .domains
                    .get(&row.account_id)
                    .is_some_and(|d| d == wanted)
                {
                    let locale = row.language.clone().unwrap_or_else(|| "und".to_string());
                    *counts.entry(locale).or_insert(0) += 1;
                }
            }
            out.push(json!({
                "key": "instance_languages",
                "data": top_counts(counts, scope.limit),
            }));
        }
        _ => {}
    }
    Ok(())
}

/// Tag-family dimensions: `tag_servers` / `tag_languages` are served when
/// `<key>[id]` names a tag; without a resolvable id they stay omitted
/// ([`REFUSED_TAG_NO_ID`] / [`REFUSED_TAG_UNKNOWN_ID`]).
/// `software_versions` stays refused (D39, [`REFUSED_SOFTWARE_VERSIONS`]).
/// Unknown keys drop like upstream's `filter_map`.
fn render_tag_dimensions(
    scope: &DimensionScope<'_>,
    key: &str,
    out: &mut Vec<Value>,
) -> Result<(), Response> {
    if let Some(tag_key) = ["tag_servers", "tag_languages"]
        .into_iter()
        .find(|known| *known == key)
    {
        let param_key = format!("{tag_key}[id]");
        let tag = forms::get(scope.body, param_key.as_str())
            .and_then(|raw| raw.trim().parse::<Id>().ok())
            .and_then(|id| tag_name_for_id(scope.store, id));
        let Some(tag) = tag else {
            return Ok(());
        };
        let rows = tag_metric_rows(scope.store, &tag);
        match tag_key {
            "tag_servers" => {
                let mut ids: Vec<Id> = rows.iter().map(|(_, who, _)| *who).collect();
                ids.sort_unstable();
                ids.dedup();
                let authors = scope.store.accounts_by_ids(&ids).unwrap_or_default();
                let mut counts: HashMap<String, u64> = HashMap::new();
                for (at, who, _) in &rows {
                    if !in_dim_window(scope, *at) {
                        continue;
                    }
                    let domain = authors
                        .get(who)
                        .map(|account| account.domain.clone())
                        .unwrap_or_else(|| scope.server_domain.to_string());
                    *counts.entry(domain).or_insert(0) += 1;
                }
                out.push(json!({
                    "key": "tag_servers",
                    "data": top_counts(counts, scope.limit),
                }));
            }
            _ => {
                let mut counts: HashMap<String, u64> = HashMap::new();
                for (at, _, language) in &rows {
                    if !in_dim_window(scope, *at) {
                        continue;
                    }
                    let locale = language.clone().unwrap_or_else(|| "und".to_string());
                    *counts.entry(locale).or_insert(0) += 1;
                }
                out.push(json!({
                    "key": "tag_languages",
                    "data": top_counts(counts, scope.limit),
                }));
            }
        }
    }
    Ok(())
}

/// `POST /api/v1/admin/dimensions` with read scope upstream (kept). Same
/// omit-unknown-keys rule as measures. Moderators pass ([`require_metrics`]).
pub async fn dimensions(
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
    let ctx = bucketize(&body, &snapshots, &state.server.domain, start, end);
    render_keys(store, ctx, &snapshots, &body, &keys)
}

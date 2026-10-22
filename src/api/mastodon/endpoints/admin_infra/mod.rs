//! Admin infrastructure routes (Phase 7.3, subagent C).
//!
//! Domain allows/blocks, email-domain blocks, IP blocks (with request-path
//! enforcement through the trusted-proxy client IP), canonical email blocks
//! (including the read-scoped `test` action), admin tag flags, and the
//! read-scoped measures/dimensions/retention analytics.
//!
//! Auth: user token + role gate (checked **before** the scope check).
//! Reads take `admin:read` (or the listed sub-scope); writes take
//! `admin:write` (or the listed sub-scope). `measures`, `dimensions`,
//! `retention` and canonical-email `test` take the read scope upstream, kept
//! here.
//!
//! Role split: most routes admit **admins only** ([`require_admin`]);
//! `measures`, `dimensions` and `retention` additionally admit **moderators**
//! ([`require_metrics`]) — the P3 operator decision (moderators read
//! analytics; upstream admits them through the dashboard policy, see the
//! helper). Tags stay admin-only (the finer D35 roadmap split is intact:
//! moderators are still refused everywhere else in this module).
//!
//! Enforcement gaps (recorded in `docs/endpoint-manifest.md`, never stubbed):
//! domain blocks wait for federation (Phase 6); email-domain and canonical
//! email blocks wait for email confirmation (Phase 2 gap); IP `no_access`
//! enforcement and `sign_up_block` registration refusal are provided as
//! [`ip_block_middleware`] / [`is_sign_up_blocked_for`] and wired by the
//! integrator (this module cannot touch `router.rs`).
//!
//! Every write appends one [`crate::domain::AdminAuditRecord`] (proposed
//! default: audit on).

use std::collections::HashMap;

use chrono::Datelike;

use axum::http::{Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::api::forms;
use crate::api::mastodon::ApiState;
use crate::api::mastodon::auth::{AdminArea, Authenticated};
use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::api::mastodon::pagination;
use crate::application::ports::{Store, TimelineFilter, TimelineQuery};
use crate::domain::{Id, Status, UserRole};

/// Scope alternatives for admin reads of a sub-scoped resource.
macro_rules! read_scopes {
    ($sub:literal) => {
        &["admin:read", $sub][..]
    };
}
/// Scope alternatives for admin writes of a sub-scoped resource.
macro_rules! write_scopes {
    ($sub:literal) => {
        &["admin:write", $sub][..]
    };
}

/// The admin-only role gate for this module: user token, then **admin role
/// only**, then scopes. Wraps [`Authenticated::require_admin`] pinned to
/// [`AdminArea::AdminOnly`]; rename here if the permission split changes.
/// (`measures`, `dimensions` and `retention` use [`require_metrics`]
/// instead so moderators pass.)
pub(crate) fn require_admin(
    auth: &Authenticated,
    store: &dyn Store,
    scopes: &[&str],
) -> ApiResult<Id> {
    auth.require_admin(store, AdminArea::AdminOnly, scopes)?;
    auth.require_user()
}

/// The analytics role gate (`measures`, `dimensions`, `retention` only):
/// user token, then **admin or moderator** role, then scopes.
///
/// Upstream admits moderators here. `Admin::PermissionsConcern`
/// (`app/controllers/concerns/admin/permissions_concern.rb` at `8ff11c2b`)
/// only requires a moderator-or-admin role (`Admin::BasePolicy#access?`,
/// `app/policies/admin/base_policy.rb:1-6`: administrator, or any
/// moderation/administration/devops flag), and the per-action
/// `authorize :dashboard, :index?` (`measures_controller.rb:13`,
/// `dimensions_controller.rb:15`, `retention_controller.rb:15`) checks
/// `DashboardPolicy#index?` (`app/policies/dashboard_policy.rb:5-7`:
/// `role.can?(:view_dashboard)`), which the default moderator holds
/// (moderation category, `app/models/user_role.rb:64-77`). Birchtree roles
/// carry no permission bitmask, so admin-or-moderator is the faithful
/// coarse mapping. Every other route in this module keeps [`require_admin`].
pub(crate) fn require_metrics(
    auth: &Authenticated,
    store: &dyn Store,
    scopes: &[&str],
) -> ApiResult<Id> {
    let user_id = auth.require_user()?;
    let role = store
        .user_by_account_id(user_id)
        .map_err(ApiError::from)?
        .map(|user| user.role)
        .unwrap_or(UserRole::None);
    match role {
        UserRole::Admin | UserRole::Moderator => {}
        UserRole::None => return Err(ApiError::role_forbidden()),
    }
    auth.require_scopes(scopes)?;
    Ok(user_id)
}

/// Route fragment for the integrator (`router.rs` mounts it; this module must
/// not edit `router.rs` itself):
///
/// ```ignore
/// .merge(crate::api::mastodon::endpoints::admin_infra::routes())
/// ```
pub fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/admin/domain_allows",
            get(domain_allows_index).post(domain_allows_create),
        )
        .route(
            "/api/v1/admin/domain_allows/:id",
            get(domain_allows_show).delete(domain_allows_destroy),
        )
        .route(
            "/api/v1/admin/domain_blocks",
            get(domain_blocks_index).post(domain_blocks_create),
        )
        .route(
            "/api/v1/admin/domain_blocks/:id",
            get(domain_blocks_show)
                .put(domain_blocks_update)
                .patch(domain_blocks_update)
                .delete(domain_blocks_destroy),
        )
        .route(
            "/api/v1/admin/email_domain_blocks",
            get(email_domain_blocks_index).post(email_domain_blocks_create),
        )
        .route(
            "/api/v1/admin/email_domain_blocks/:id",
            get(email_domain_blocks_show).delete(email_domain_blocks_destroy),
        )
        .route(
            "/api/v1/admin/ip_blocks",
            get(ip_blocks_index).post(ip_blocks_create),
        )
        .route(
            "/api/v1/admin/ip_blocks/:id",
            get(ip_blocks_show)
                .put(ip_blocks_update)
                .patch(ip_blocks_update)
                .delete(ip_blocks_destroy),
        )
        .route(
            "/api/v1/admin/canonical_email_blocks",
            get(canonical_email_blocks_index).post(canonical_email_blocks_create),
        )
        .route(
            "/api/v1/admin/canonical_email_blocks/test",
            post(canonical_email_blocks_test),
        )
        .route(
            "/api/v1/admin/canonical_email_blocks/:id",
            get(canonical_email_blocks_show).delete(canonical_email_blocks_destroy),
        )
        .route("/api/v1/admin/tags", get(tags_index))
        .route(
            "/api/v1/admin/tags/:id",
            get(tags_show).put(tags_update).patch(tags_update),
        )
        .route("/api/v1/admin/measures", post(measures))
        .route("/api/v1/admin/dimensions", post(dimensions))
        .route("/api/v1/admin/retention", post(retention))
}

// ---------------------------------------------------------------------------
// Shared request plumbing.
// ---------------------------------------------------------------------------

pub(crate) fn rfc3339_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

pub(crate) fn date_only(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.date_naive().to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

pub(crate) fn parse_page(uri: &Uri, default: usize, max: usize) -> ApiResult<pagination::Page> {
    let pairs = match uri.query() {
        Some(query) => forms::parse(query.as_bytes()),
        None => Vec::new(),
    };
    pagination::parse(
        forms::get(&pairs, "limit"),
        forms::get(&pairs, "max_id"),
        forms::get(&pairs, "since_id"),
        forms::get(&pairs, "min_id"),
        default,
        max,
    )
}

/// Human-readable size like Rails' `number_to_human_size` (used for
/// `human_value` on byte measures).
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["Bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Paginate store rows newest-first and render them with `render`.
pub(crate) fn paged<T>(
    uri: &Uri,
    default: usize,
    max: usize,
    fetch: impl FnOnce(Option<Id>, Option<Id>, Option<Id>, usize) -> crate::domain::Result<Vec<T>>,
    render: impl Fn(&T) -> Value,
    id_of: impl Fn(&T) -> Id,
) -> Response {
    let page = match parse_page(uri, default, max) {
        Ok(page) => page,
        Err(err) => return err.into_response(),
    };
    if page.limit == 0 {
        return Json(Vec::<Value>::new()).into_response();
    }
    let rows = match fetch(page.max_id, page.since_id, page.min_id, page.limit) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let full = rows.len() == page.limit;
    let next = if full { rows.last().map(&id_of) } else { None };
    let prev = if rows.is_empty() {
        None
    } else {
        rows.first().map(&id_of)
    };
    let body: Vec<Value> = rows.iter().map(&render).collect();
    let mut response = Json(body).into_response();
    if let Some(link) = pagination::link_header_desc(uri, next, prev) {
        if let Ok(value) = header::HeaderValue::from_str(&link) {
            response.headers_mut().insert(header::LINK, value);
        }
    }
    response
}

pub(crate) fn require_id(raw: &str) -> ApiResult<Id> {
    raw.parse::<Id>().map_err(|_| ApiError::not_found())
}

pub(crate) fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// One audit record per admin write (proposed default: on). Failures to
/// append the audit row fail the request: an unaudited admin write must not
/// look successful.
pub(crate) fn audit(
    state: &ApiState,
    actor: Id,
    action: &str,
    target_type: &str,
    target_id: &str,
) -> ApiResult<()> {
    let store: &dyn Store = &*state.context.store;
    store
        .admin_audit(
            actor,
            action,
            target_type,
            target_id,
            state.context.now_ms(),
        )
        .map_err(ApiError::from)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Measures, dimensions, retention (all POST, all read-scoped upstream).
// ---------------------------------------------------------------------------

pub(crate) const METRIC_SCAN_CAP: usize = 200_000;
pub(crate) const DAY_MS: u64 = 86_400_000;

/// D39 refused keys (round 3, narrowed round 4): served-or-refused, never
/// zero-filled. Each known-but-uncomputable key stays absent from the response
/// array (upstream's `filter_map` drops unknown keys the same way); the reason
/// lives here and in the dashboard note, not in the wire shape.
///
/// `interactions` was refused until the unified event journal landed; it is
/// served since round 4 (writers: non-self replies, favourites, reblogs,
/// votes, new follows/requests — the five upstream tracker call sites), so
/// its reason constant is gone.
pub const REFUSED_SOFTWARE_VERSIONS: &str = "no component-version census is collected: upstream `software_versions` reports mastodon/ruby/postgresql/redis/[elasticsearch|opensearch]/libvips/ffmpeg per row (`app/lib/admin/metrics/dimension/software_versions_dimension.rb:13-19`), each probed live (SELECT VERSION(), redis INFO, Chewy client info, Vips.version_string, ffprobe output). This server has no postgresql/redis/search processes to probe and collects no ruby/libvips/ffmpeg version rows, so the key stays absent rather than fabricated";
/// `tag_*` measures and dimensions require `<key>[id]` (upstream
/// `params.require(key).permit(:id)` → 400 when missing). Without the id the
/// key is omitted (lenient where upstream is strict); the reason is recorded
/// here (D39).
pub const REFUSED_TAG_NO_ID: &str =
    "tag measures/dimensions need `<key>[id]` naming a trend-flag or admin-tag row id";
/// An unknown tag id names no trend-flag or admin-tag row: the key is
/// omitted, never zero-filled.
pub const REFUSED_TAG_UNKNOWN_ID: &str = "unknown tag id: no trend-flag or admin-tag row";

/// Bounded tag-status scan backing the `tag_*` measures and dimensions
/// (round 3, D39): the same hashtag-index source as D40 `tag_history`
/// (`trends::tag_history` renders the trailing 7 days for display; metrics
/// need the full request window, so they scan the index directly, newest
/// entries first up to the `redb` hashtag scan cap).
pub(crate) const TAG_METRIC_SCAN: usize = 2_000;

pub(crate) struct AllowAllStatuses;

impl TimelineFilter for AllowAllStatuses {
    fn include(&self, _status: &Status) -> bool {
        true
    }
}

/// Resolve a `tag_*` `<key>[id]>` to its normalized tag name: trend-flag rows
/// first (the admin trends queue mints the numeric `:id`), then admin tag
/// rows. `None` names nothing (the key is omitted).
pub(crate) fn tag_name_for_id(store: &dyn Store, id: Id) -> Option<String> {
    if let Ok(flags) = store.all_trend_tag_flags()
        && let Some(flag) = flags.into_iter().find(|flag| flag.id == id)
    {
        return Some(flag.name);
    }
    if let Ok(Some(row)) = store.admin_tag_by_id(id) {
        return Some(row.name);
    }
    None
}

/// `(created_at_ms, account_id, language)` per live indexed status carrying
/// `tag`, newest first, bounded by [`TAG_METRIC_SCAN`].
pub(crate) fn tag_metric_rows(store: &dyn Store, tag: &str) -> Vec<(u64, Id, Option<String>)> {
    let page = store.hashtag_timeline(
        tag,
        TimelineQuery {
            limit: TAG_METRIC_SCAN,
            ..Default::default()
        },
        &AllowAllStatuses,
    );
    match page {
        Ok(page) => page
            .statuses
            .into_iter()
            .filter(|status| !status.is_deleted())
            .map(|status| {
                (
                    status.created_at_ms,
                    status.account_id,
                    status.language.clone(),
                )
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Count distinct accounts per day bucket.
pub(crate) fn daily_distinct(pairs: &[(Id, u64)], buckets: &[u64]) -> Vec<u64> {
    use std::collections::HashSet;
    let mut sets: Vec<HashSet<Id>> = buckets.iter().map(|_| HashSet::new()).collect();
    for (who, time) in pairs {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            sets[idx].insert(*who);
        }
    }
    sets.iter().map(|set| set.len() as u64).collect()
}

/// Count distinct domains per day bucket.
pub(crate) fn daily_distinct_domains(pairs: &[(String, u64)], buckets: &[u64]) -> Vec<u64> {
    use std::collections::HashSet;
    let mut sets: Vec<HashSet<&str>> = buckets.iter().map(|_| HashSet::new()).collect();
    for (domain, time) in pairs {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            sets[idx].insert(domain.as_str());
        }
    }
    sets.iter().map(|set| set.len() as u64).collect()
}

/// Parse an optional `start_at`/`end_at` form value (RFC 3339 or date).
/// Absent values default to the trailing 30 days ending now.
pub(crate) fn metric_window(body: &[(String, String)], now_ms: u64) -> (u64, u64) {
    let parse = |key: &str| -> Option<u64> {
        let raw = forms::get(body, key)?.trim().to_string();
        if raw.is_empty() {
            return None;
        }
        // Epoch seconds or millis first (cheap), then RFC 3339, then date.
        if let Ok(num) = raw.parse::<i64>() {
            let ms = if num.abs() < 10_000_000_000 {
                num * 1_000
            } else {
                num
            };
            return u64::try_from(ms).ok();
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&raw) {
            return u64::try_from(dt.timestamp_millis()).ok();
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(&raw, "%Y-%m-%d") {
            return u64::try_from(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis()).ok();
        }
        None
    };
    let end = parse("end_at").unwrap_or(now_ms);
    let start = parse("start_at").unwrap_or_else(|| end.saturating_sub(30 * DAY_MS));
    // Upstream clamps the window to two years back.
    let start = start.max(end.saturating_sub(2 * 365 * DAY_MS));
    (start.min(end), end)
}

/// Day-bucket boundaries covering `[start, end]` (each a midnight UTC ms).
pub(crate) fn day_buckets(start_ms: u64, end_ms: u64) -> Vec<u64> {
    let day = |ms: u64| ms - ms % DAY_MS;
    let mut out = Vec::new();
    let mut cursor = day(start_ms);
    let last = day(end_ms);
    while cursor <= last {
        out.push(cursor);
        cursor = cursor.saturating_add(DAY_MS);
        if out.len() > 732 {
            break;
        }
    }
    out
}

/// Count `times` per day bucket.
pub(crate) fn daily_counts(times: &[u64], buckets: &[u64]) -> Vec<u64> {
    let mut counts = vec![0u64; buckets.len()];
    for &time in times {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            counts[idx] += 1;
        }
    }
    counts
}

/// Count distinct `who` per day bucket (D39 `active_users`: unique logins
/// per day, like upstream's `:unique` activity tracker).
pub(crate) fn daily_unique(pairs: &[(Id, u64)], buckets: &[u64]) -> Vec<u64> {
    use std::collections::HashSet;
    let mut sets: Vec<HashSet<Id>> = buckets.iter().map(|_| HashSet::new()).collect();
    for (who, time) in pairs {
        if let Ok(idx) = buckets.binary_search(&(time - time % DAY_MS)) {
            sets[idx].insert(*who);
        }
    }
    sets.iter().map(|set| set.len() as u64).collect()
}

pub(crate) fn daily_data(buckets: &[u64], counts: &[u64]) -> Vec<Value> {
    buckets
        .iter()
        .zip(counts.iter())
        .map(|(day, count)| json!({ "date": date_only(*day), "value": count.to_string() }))
        .collect()
}

pub(crate) fn sum_in_window(times: &[u64], start: u64, end: u64) -> u64 {
    times.iter().filter(|&&t| t >= start && t <= end).count() as u64
}

pub(crate) fn top_counts(counts: HashMap<String, u64>, limit: usize) -> Vec<Value> {
    let mut entries: Vec<(String, u64)> = counts.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries
        .into_iter()
        .take(limit)
        .map(|(key, value)| {
            json!({ "key": key.clone(), "human_key": key, "value": value.to_string() })
        })
        .collect()
}

/// Midnight UTC of the month containing `ms`.
pub(crate) fn month_start_ms(ms: u64) -> u64 {
    let days = (ms / DAY_MS) as i32 + 719_163;
    let date = chrono::NaiveDate::from_num_days_from_ce_opt(days).unwrap();
    let first = chrono::NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap();
    (first.num_days_from_ce() as u64).saturating_sub(719_163) * DAY_MS
}

pub mod canonical_email;
pub mod dimensions;
pub mod domain_allow;
pub mod domain_block;
pub mod email_domain;
pub mod ip_block;
pub mod measures;
pub mod retention;
pub mod tags;

pub use self::canonical_email::{
    canonical_email_blocks_create, canonical_email_blocks_destroy, canonical_email_blocks_index,
    canonical_email_blocks_show, canonical_email_blocks_test,
};
pub use self::dimensions::dimensions;
pub use self::domain_allow::{
    domain_allows_create, domain_allows_destroy, domain_allows_index, domain_allows_show,
};
pub use self::domain_block::{
    domain_blocks_create, domain_blocks_destroy, domain_blocks_index, domain_blocks_show,
    domain_blocks_update,
};
pub use self::email_domain::{
    email_domain_blocks_create, email_domain_blocks_destroy, email_domain_blocks_index,
    email_domain_blocks_show,
};
pub use self::ip_block::{
    ip_block_middleware, ip_blocks_create, ip_blocks_destroy, ip_blocks_index, ip_blocks_show,
    ip_blocks_update, is_sign_up_blocked_for, request_client_ip,
};
pub use self::measures::measures;
pub use self::retention::retention;
pub use self::tags::{tags_index, tags_show, tags_update};

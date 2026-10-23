//! Mastodon admin/trend DTO packers (S10 split of `api::mastodon::dto`).
//!
//! Home of the admin projections plus the annual-report serializers. The
//! public trend shapes stay in the `dto` shim (`super::trend_link_json`;
//! `trend_tag_json` lives in `account.rs`): each admin projection here
//! delegates to its public shape and adds its admin-only keys, which is why
//! this file imports from both.
//!
//! Explicit home map for the admin-adjacent serializers:
//! - `admin_trend_tag_json` → `account.rs` (sits next to `trend_tag_json`,
//!   which it delegates to).
//! - `admin_trend_link_json`, `admin_trend_publisher_json` → here (delegate
//!   to the shim's `trend_link_json`; publisher has no public twin).
//! - `annual_report_json`, `annual_reports_json` → here.
//! - `instance_*` admin bits (contact account, configuration) → `instance.rs`.

use serde_json::{Value, json};

use crate::application::ports::filter_json::rfc3339_ms;
use crate::domain::{AnnualReport, TrendLink, TrendPublisher};

use super::trend_link_json;

/// `REST::Admin::Trends::LinkSerializer` projection: the public link shape
/// plus the numeric `id` and `requires_review` (upstream carries no
/// `trendable` key here).
pub fn admin_trend_link_json(link: &TrendLink) -> Value {
    let mut object = match trend_link_json(link) {
        Value::Object(map) => map,
        other => return other,
    };
    object.insert("id".into(), json!(link.id.to_string()));
    object.insert(
        "requires_review".into(),
        json!(link.reviewed_at_ms.is_none()),
    );
    Value::Object(object)
}

/// `REST::Admin::Trends::Links::PreviewCardProviderSerializer` projection of
/// a trend publisher.
pub fn admin_trend_publisher_json(publisher: &TrendPublisher) -> Value {
    json!({
        "id": publisher.id.to_string(),
        "domain": publisher.domain,
        "trendable": publisher.allowed,
        "reviewed_at": publisher
            .reviewed_at_ms
            .map(rfc3339_ms)
            .map(Value::String)
            .unwrap_or(Value::Null),
        "requested_review_at": Value::Null,
        "requires_review": publisher.reviewed_at_ms.is_none(),
    })
}

/// `REST::AnnualReportSerializer`: `{year, data, schema_version, share_url,
/// account_id}`. Sharing is out of profile (no public wrapstodon route), so
/// `share_url` is always `null`.
pub fn annual_report_json(report: &AnnualReport) -> Value {
    json!({
        "year": report.year,
        "data": report.data,
        "schema_version": report.schema_version,
        "share_url": Value::Null,
        "account_id": report.account_id.to_string(),
    })
}

/// `REST::AnnualReportsSerializer`: the viewer's reports with the referenced
/// accounts and top statuses embedded.
pub fn annual_reports_json(
    reports: &[AnnualReport],
    accounts: Vec<Value>,
    statuses: Vec<Value>,
) -> Value {
    json!({
        "annual_reports": reports.iter().map(annual_report_json).collect::<Vec<_>>(),
        "accounts": accounts,
        "statuses": statuses,
    })
}

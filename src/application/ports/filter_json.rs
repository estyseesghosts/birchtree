//! Filter wire shapes shared by the API and application layers.
//!
//! The `filtered`-array matcher in `application::filters` must not import
//! `api::mastodon::dto` (dependency direction `domain <- application <-
//! adapters`), so the filter serializers live here, next to `FilterBundle`.
//! `api::mastodon::dto` re-exports them for the endpoint layer.

use serde_json::{Map, Value, json};

use super::filter_store::FilterBundle;
use crate::domain::{CustomFilter, FilterKeyword, Id};

/// Format a millisecond timestamp as Mastodon's ISO-8601 with milliseconds.
pub fn rfc3339_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// `REST::FilterSerializer` (v2), with `keywords`/`statuses` only when rules
/// were requested (the CRUD endpoints always request them; status payloads'
/// `filter` embed never does).
pub fn filter_json(bundle: &FilterBundle, with_rules: bool) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(bundle.filter.id.to_string()));
    object.insert("title".into(), json!(bundle.filter.title));
    object.insert("context".into(), json!(bundle.filter.context));
    object.insert(
        "expires_at".into(),
        bundle
            .filter
            .expires_at_ms
            .map(rfc3339_ms)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    object.insert("filter_action".into(), json!(bundle.filter.action.as_str()));
    if with_rules {
        let keywords: Vec<Value> = bundle
            .keywords
            .iter()
            .map(|keyword| {
                json!({
                    "id": keyword.id.to_string(),
                    "keyword": keyword.keyword,
                    "whole_word": keyword.whole_word,
                })
            })
            .collect();
        let statuses: Vec<Value> = bundle
            .statuses
            .iter()
            .map(|rule| {
                json!({
                    "id": rule.id.to_string(),
                    "status_id": rule.status_id.to_string(),
                })
            })
            .collect();
        object.insert("keywords".into(), json!(keywords));
        object.insert("statuses".into(), json!(statuses));
    }
    Value::Object(object)
}

/// `REST::FilterResultSerializer` element of a status's `filtered` array.
pub fn filter_result_json(
    bundle: &FilterBundle,
    keyword_matches: Vec<String>,
    status_matches: Vec<crate::domain::Id>,
) -> Value {
    let filter = filter_json(bundle, false);
    json!({
        "filter": filter,
        "keyword_matches": keyword_matches,
        "status_matches": status_matches
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
    })
}

/// `REST::V1::FilterSerializer`: the v1 API projects one *keyword* row (its
/// `id` is the keyword id) with the parent filter's `context`/`expires_at`
/// and `irreversible` (`filter_action == hide`).
pub fn filter_v1_json(keyword: &FilterKeyword, filter: &CustomFilter, status_ids: &[Id]) -> Value {
    json!({
        "id": keyword.id.to_string(),
        "phrase": keyword.keyword,
        "context": filter.context,
        "whole_word": keyword.whole_word,
        "expires_at": filter
            .expires_at_ms
            .map(rfc3339_ms)
            .map(Value::String)
            .unwrap_or(Value::Null),
        "irreversible": filter.action == crate::domain::FilterAction::Hide,
        "status_ids": status_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
    })
}

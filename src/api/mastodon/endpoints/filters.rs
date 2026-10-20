//! Filter endpoints (Phase 4): v1 (synthesized from v2) + v2 CRUD + the
//! keyword/status rule sub-resources (`Api::V1::FiltersController`,
//! `Api::V2::FiltersController` and `Api::V2::Filters::{Keywords,Statuses}Controller`).
//!
//! Handlers stay thin: form decoding (including Mastodon's indexed
//! `keywords_attributes[N][field]` convention) and scope/param checks live
//! here; every validation rule and 422 message is the filter use case's.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::api::forms::{self, LenientForm};
use crate::api::mastodon::auth::Auth;
use crate::api::mastodon::dto;
use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::application::filters::{
    self, FilterUpdate, KeywordAttr, V1Update,
};
use crate::application::ports::Store;
use crate::domain::{Error as DomainError, FilterKeyword, FilterStatus, Id};

/// Read scopes for filter reads.
const READ_SCOPES: &[&str] = &["read", "read:filters"];
/// Write scopes for filter writes.
const WRITE_SCOPES: &[&str] = &["write", "write:filters"];

/// `Deprecation` header value for the v1 filter API. Upstream
/// `Api::V1::FiltersController` calls `deprecate_api '2022-11-14'`
/// (`DeprecationConcern`, `app/controllers/concerns/deprecation_concern.rb`
/// at pin `8ff11c2b`), rendering `@<unix timestamp>` for midnight UTC on
/// that date.
const V1_DEPRECATION: &str = "@1668384000";

/// Stamp a v1 response as deprecated (upstream `DeprecationConcern`).
fn deprecated(response: axum::response::Response) -> axum::response::Response {
    let mut response = response;
    response.headers_mut().insert(
        "deprecation",
        axum::http::HeaderValue::from_static(V1_DEPRECATION),
    );
    response
}

fn bool_cast(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "t" | "yes" | "on"
    )
}

/// Map filter errors: Mastodon 422 validation bodies pass through, 404s use
/// the `Record not found` body.
fn filter_error(err: DomainError) -> ApiError {
    match err {
        DomainError::NotFound(_) => ApiError::not_found(),
        other => ApiError::from(other),
    }
}

/// All `context[]`/`context` values of a form body.
fn context_values(body: &[(String, String)]) -> Vec<String> {
    forms::all(body, "context")
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// `context` when the key was sent (Mastodon's `params.key?(:context)`), so
/// updates distinguish "absent" (unchanged) from "empty" (422).
fn context_if_present(body: &[(String, String)]) -> Option<Vec<String>> {
    let present = body
        .iter()
        .any(|(key, _)| key == "context" || key == "context[]");
    present.then(|| context_values(body))
}

/// Parse `expires_in` (seconds): absent -> `None` (unchanged / never),
/// blank or `0` -> `Some(None)` (clears the expiry), a number -> duration.
/// Anything else is Mastodon's "Expires in is not a number" 422.
fn parse_expires(raw: Option<&str>) -> Result<Option<Option<u64>>, ApiError> {
    match raw.map(str::trim) {
        None => Ok(None),
        Some("") | Some("0") => Ok(Some(None)),
        Some(raw) => raw
            .parse::<u64>()
            .map(|seconds| Some(Some(seconds)))
            .map_err(|_| ApiError::unprocessable(filters::ERR_EXPIRES)),
    }
}

/// Parse the indexed `keywords_attributes[N][id|keyword|whole_word|_destroy]`
/// form keys into ordered entries (`None` when no such key is present).
fn parse_keyword_attrs(pairs: &[(String, String)]) -> Option<Vec<KeywordAttr>> {
    let mut entries: BTreeMap<usize, KeywordAttr> = BTreeMap::new();
    let mut found = false;
    for (key, value) in pairs {
        let Some(rest) = key.strip_prefix("keywords_attributes[") else {
            continue;
        };
        let Some((index, field)) = rest.split_once("][") else {
            continue;
        };
        let Ok(index) = index.parse::<usize>() else {
            continue;
        };
        let field = field.strip_suffix(']').unwrap_or(field);
        let entry = entries.entry(index).or_default();
        found = true;
        match field {
            "id" => entry.id = value.parse::<Id>().ok(),
            "keyword" => entry.keyword = Some(value.clone()),
            "whole_word" => entry.whole_word = Some(bool_cast(value)),
            "_destroy" => entry.destroy = bool_cast(value),
            _ => {}
        }
    }
    found.then(|| entries.into_values().collect())
}

/// Initial keywords for a create (`keywords_attributes` without `_destroy`).
fn initial_keywords(entries: Option<Vec<KeywordAttr>>) -> Vec<(String, bool)> {
    entries
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| !entry.destroy)
        .map(|entry| {
            (
                entry.keyword.unwrap_or_default(),
                entry.whole_word.unwrap_or(false),
            )
        })
        .collect()
}

/// `REST::FilterKeywordSerializer` standalone shape (the same fields
/// `dto::filter_json` embeds in its `keywords` array).
fn keyword_json(keyword: &FilterKeyword) -> Value {
    json!({
        "id": keyword.id.to_string(),
        "keyword": keyword.keyword,
        "whole_word": keyword.whole_word,
    })
}

/// `REST::FilterStatusSerializer` standalone shape.
fn status_rule_json(rule: &FilterStatus) -> Value {
    json!({
        "id": rule.id.to_string(),
        "status_id": rule.status_id.to_string(),
    })
}

// ---------------------------------------------------------------------------
// v1 (synthesized from v2; a v1 "filter" is one keyword row).
// ---------------------------------------------------------------------------

/// `GET /api/v1/filters`.
pub async fn v1_index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match filters::v1_index(store, viewer) {
        Ok(rows) => {
            let body: Vec<Value> = rows
                .iter()
                .map(|row| dto::filter_v1_json(&row.keyword, &row.filter, &row.status_ids))
                .collect();
            deprecated(axum::Json(body).into_response())
        }
        Err(err) => filter_error(err).into_response(),
    }
}

/// `POST /api/v1/filters` — `phrase` required; creates filter + keyword in
/// ONE transaction.
pub async fn v1_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    // `params.require(:phrase)`.
    let Some(phrase) = forms::get(&body, "phrase") else {
        return ApiError::bad_request("param is missing or the value is empty: phrase")
            .into_response();
    };
    let context = context_values(&body);
    let expires_in_s = match parse_expires(forms::get(&body, "expires_in")) {
        Ok(expires) => expires.flatten(),
        Err(err) => return err.into_response(),
    };
    let irreversible = forms::get(&body, "irreversible").is_some_and(bool_cast);
    let whole_word = forms::get(&body, "whole_word").is_some_and(bool_cast);
    let store: &dyn Store = &*state.context.store;
    match filters::v1_create(
        store,
        viewer,
        phrase,
        &context,
        expires_in_s,
        irreversible,
        whole_word,
        state.context.now_ms(),
    ) {
        Ok(row) => deprecated(
            axum::Json(dto::filter_v1_json(&row.keyword, &row.filter, &row.status_ids))
                .into_response(),
        ),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `GET /api/v1/filters/:id` (the id is a keyword id).
pub async fn v1_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::v1_show(store, viewer, id) {
        Ok(row) => deprecated(
            axum::Json(dto::filter_v1_json(&row.keyword, &row.filter, &row.status_ids))
                .into_response(),
        ),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `PUT /api/v1/filters/:id`.
pub async fn v1_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let expires_in_s = match parse_expires(forms::get(&body, "expires_in")) {
        Ok(expires) => expires,
        Err(err) => return err.into_response(),
    };
    let update = V1Update {
        phrase: forms::get(&body, "phrase").map(str::to_string),
        context: context_if_present(&body),
        expires_in_s,
        irreversible: forms::get(&body, "irreversible").map(bool_cast),
        whole_word: forms::get(&body, "whole_word").map(bool_cast),
    };
    let store: &dyn Store = &*state.context.store;
    match filters::v1_update(store, viewer, id, update, state.context.now_ms()) {
        Ok(row) => deprecated(
            axum::Json(dto::filter_v1_json(&row.keyword, &row.filter, &row.status_ids))
                .into_response(),
        ),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `DELETE /api/v1/filters/:id` — deletes the keyword only (the parent
/// filter row stays).
pub async fn v1_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::v1_destroy(store, viewer, id) {
        Ok(()) => deprecated(axum::Json(json!({})).into_response()),
        Err(err) => filter_error(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// v2 CRUD.
// ---------------------------------------------------------------------------

/// `GET /api/v2/filters` — the viewer's filters with rules.
pub async fn v2_index(State(state): State<ApiState>, auth: Auth) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let store: &dyn Store = &*state.context.store;
    match filters::index(store, viewer, None) {
        Ok(bundles) => {
            let body: Vec<Value> = bundles
                .iter()
                .map(|bundle| dto::filter_json(bundle, true))
                .collect();
            axum::Json(body).into_response()
        }
        Err(err) => filter_error(err).into_response(),
    }
}

/// `POST /api/v2/filters` — `title`/`context[]` required.
pub async fn v2_create(
    State(state): State<ApiState>,
    auth: Auth,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let expires_in_s = match parse_expires(forms::get(&body, "expires_in")) {
        Ok(expires) => expires.flatten(),
        Err(err) => return err.into_response(),
    };
    let title = forms::get(&body, "title").unwrap_or("");
    let context = context_values(&body);
    let action = forms::get(&body, "filter_action").unwrap_or("warn");
    let keywords = initial_keywords(parse_keyword_attrs(&body));
    let store: &dyn Store = &*state.context.store;
    match filters::create(
        store,
        viewer,
        title,
        &context,
        expires_in_s,
        action,
        keywords,
        state.context.now_ms(),
    ) {
        Ok(bundle) => axum::Json(dto::filter_json(&bundle, true)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `GET /api/v2/filters/:id`.
pub async fn v2_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::show(store, viewer, id) {
        Ok(bundle) => axum::Json(dto::filter_json(&bundle, true)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `PUT/PATCH /api/v2/filters/:id` — patch fields and/or replace keywords via
/// `keywords_attributes[N][...]`.
pub async fn v2_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let expires_in_s = match parse_expires(forms::get(&body, "expires_in")) {
        Ok(expires) => expires,
        Err(err) => return err.into_response(),
    };
    let update = FilterUpdate {
        title: forms::get(&body, "title").map(str::to_string),
        context: context_if_present(&body),
        expires_in_s,
        action: forms::get(&body, "filter_action").map(str::to_string),
        keywords: parse_keyword_attrs(&body),
    };
    let store: &dyn Store = &*state.context.store;
    match filters::update(store, viewer, id, update, state.context.now_ms()) {
        Ok(bundle) => axum::Json(dto::filter_json(&bundle, true)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `DELETE /api/v2/filters/:id` — `200 {}`.
pub async fn v2_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::destroy(store, viewer, id) {
        Ok(()) => axum::Json(json!({})).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Keyword sub-resource.
// ---------------------------------------------------------------------------

/// `GET /api/v2/filters/:filter_id/keywords`.
pub async fn keywords_index(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(filter_id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::keyword_index(store, viewer, filter_id) {
        Ok(keywords) => {
            let body: Vec<Value> = keywords.iter().map(keyword_json).collect();
            axum::Json(body).into_response()
        }
        Err(err) => filter_error(err).into_response(),
    }
}

/// `POST /api/v2/filters/:filter_id/keywords` — `keyword` + `whole_word`.
pub async fn keywords_create(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(filter_id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let keyword = forms::get(&body, "keyword").unwrap_or("");
    let whole_word = forms::get(&body, "whole_word").is_some_and(bool_cast);
    let store: &dyn Store = &*state.context.store;
    match filters::keyword_create(
        store,
        viewer,
        filter_id,
        keyword,
        whole_word,
        state.context.now_ms(),
    ) {
        Ok(keyword) => axum::Json(keyword_json(&keyword)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `GET /api/v2/filters/keywords/:id`.
pub async fn keywords_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::keyword_show(store, viewer, id) {
        Ok(keyword) => axum::Json(keyword_json(&keyword)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `PUT/PATCH /api/v2/filters/keywords/:id`.
pub async fn keywords_update(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::keyword_update(
        store,
        viewer,
        id,
        forms::get(&body, "keyword"),
        forms::get(&body, "whole_word").map(bool_cast),
        state.context.now_ms(),
    ) {
        Ok(keyword) => axum::Json(keyword_json(&keyword)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `DELETE /api/v2/filters/keywords/:id` — `200` with the deleted keyword.
pub async fn keywords_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::keyword_destroy(store, viewer, id) {
        Ok(keyword) => axum::Json(keyword_json(&keyword)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Status rule sub-resource.
// ---------------------------------------------------------------------------

/// `GET /api/v2/filters/:filter_id/statuses`.
pub async fn statuses_index(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(filter_id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::status_index(store, viewer, filter_id) {
        Ok(rules) => {
            let body: Vec<Value> = rules.iter().map(status_rule_json).collect();
            axum::Json(body).into_response()
        }
        Err(err) => filter_error(err).into_response(),
    }
}

/// `POST /api/v2/filters/:filter_id/statuses` — `status_id` required; the
/// status must exist and be fetchable by the viewer.
pub async fn statuses_create(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
    LenientForm(body): LenientForm,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(filter_id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    // `params.require(:status_id)`.
    let Some(status_id) = forms::get(&body, "status_id") else {
        return ApiError::bad_request("param is missing or the value is empty: status_id")
            .into_response();
    };
    let Ok(status_id) = status_id.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::status_create(
        store,
        viewer,
        filter_id,
        status_id,
        state.context.now_ms(),
    ) {
        Ok(rule) => axum::Json(status_rule_json(&rule)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `GET /api/v2/filters/statuses/:id`.
pub async fn statuses_show(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(READ_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::status_show(store, viewer, id) {
        Ok(rule) => axum::Json(status_rule_json(&rule)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

/// `DELETE /api/v2/filters/statuses/:id` — `200` with the deleted rule.
pub async fn statuses_destroy(
    State(state): State<ApiState>,
    auth: Auth,
    Path(raw): Path<String>,
) -> Response {
    if let Err(err) = auth.0.require_scopes(WRITE_SCOPES) {
        return err.into_response();
    }
    let viewer = match auth.0.require_user() {
        Ok(id) => id,
        Err(err) => return err.into_response(),
    };
    let Ok(id) = raw.parse::<Id>() else {
        return ApiError::not_found().into_response();
    };
    let store: &dyn Store = &*state.context.store;
    match filters::status_destroy(store, viewer, id) {
        Ok(rule) => axum::Json(status_rule_json(&rule)).into_response(),
        Err(err) => filter_error(err).into_response(),
    }
}

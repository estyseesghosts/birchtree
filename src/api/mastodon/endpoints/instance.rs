//! Instance endpoints (`GET /api/v1/instance`, `GET /api/v2/instance`,
//! and the v1 sub-resources).
//!
//! The document endpoints are public. They expose static, config-derived facts
//! plus live counts and an optional contact account.
//!
//! Sub-resources (fix-4, P3 operator decision: build). Upstream routes at
//! `8ff11c2b` (`config/routes/api.rb:144-157`): `peers`, `rules`,
//! `domain_blocks`, `terms_of_service` (index + show), `privacy_policy`,
//! `extended_description`, `translation_languages`, `languages`, `activity`
//! (all v1-only; the v2 `resource :instance` at `api.rb:351` has no
//! sub-resources). Each handler below cites its controller + serializer.
//! Where the backing data does not exist here (privacy policy text, terms of
//! service rows), the handler answers 404 — the same status upstream serves
//! when unconfigured — never a fabricated document.

use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{Value, json};

use crate::api::mastodon::errors::ApiError;
use crate::api::mastodon::ApiState;
use crate::api::mastodon::dto::{self, InstanceStats};
use crate::application::ports::Store;
use crate::domain::{Account, AdminDomainBlock, AdminDomainSeverity, Id, Rule};

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

// ---------------------------------------------------------------------------
// Sub-resources (fix-4; all public, like upstream's `BaseController`, which
// skips authentication outside limited-federation mode).
// ---------------------------------------------------------------------------

/// `GET /api/v1/instance/peers` — known remote domains.
///
/// Upstream (`app/controllers/api/v1/instances/peers_controller.rb:13-15` at
/// `8ff11c2b`): `Instance.searchable.pluck(:domain)`. Upstream answers 404
/// unless `Setting.peers_api_enabled && !limited_federation_mode`
/// (`peers_controller.rb:24-26`); both default to serving
/// (`config/settings.yml:29`: `peers_api_enabled: true`). Wired to
/// `Config::peers_api_enabled` (default true). No login is required upstream
/// and none is required here.
pub async fn peers_index(State(state): State<ApiState>) -> Response {
    if !state.server.peers_api_enabled {
        return ApiError::not_found().into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let domains = match store.admin_accounts_overview(INSTANCE_SCAN_CAP) {
        Ok(accounts) => {
            let mut set: HashSet<&str> = HashSet::new();
            for account in &accounts {
                if !account.local {
                    set.insert(account.domain.as_str());
                }
            }
            let mut domains: Vec<String> = set.into_iter().map(str::to_string).collect();
            domains.sort();
            domains
        }
        Err(err) => return ApiError::from(err).into_response(),
    };
    Json(domains).into_response()
}

/// `GET /api/v1/instance/rules` — the server rules.
///
/// Upstream (`app/controllers/api/v1/instances/rules_controller.rb:11-14`):
/// `Rule.ordered` rendered with `REST::RuleSerializer` (`id`, `text`,
/// `hint`, `translations`; `app/serializers/rest/rule_serializer.rb:3-12`).
/// Upstream order is `(priority, id)` (`app/models/rule.rb:27`); local rules
/// carry no priority (all-zero upstream default), so id order matches.
/// `translations` is `{locale: {text, hint}}` (4.4+); a rule with no
/// translation rows renders `{}` and clients fall back to the base
/// `text`/`hint`.
pub async fn rules_index(State(state): State<ApiState>) -> Response {
    let store: &dyn Store = &*state.context.store;
    let mut rules = match store.admin_rules() {
        Ok(rules) => rules,
        Err(err) => return ApiError::from(err).into_response(),
    };
    rules.sort_by_key(|rule| rule.id);
    let translations = match store.admin_rule_translations() {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut by_rule: HashMap<Id, BTreeMap<&str, &crate::domain::RuleTranslation>> = HashMap::new();
    for row in &translations {
        by_rule.entry(row.rule_id).or_default().insert(row.locale.as_str(), row);
    }
    let body: Vec<Value> = rules.iter().map(|rule| rule_json(rule, &by_rule)).collect();
    Json(body).into_response()
}

/// Render one rule with its translations (`REST::RuleSerializer` shape).
/// Locales sort ascending (BTreeMap); an untranslated rule renders
/// `translations: {}` exactly like upstream's `translations.to_h` over zero
/// rows.
fn rule_json(
    rule: &Rule,
    by_rule: &HashMap<Id, BTreeMap<&str, &crate::domain::RuleTranslation>>,
) -> Value {
    let translations: serde_json::Map<String, Value> = by_rule
        .get(&rule.id)
        .map(|rows| {
            rows.iter()
                .map(|(locale, row)| {
                    (
                        locale.to_string(),
                        json!({ "text": row.text, "hint": row.hint }),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": rule.id.to_string(),
        "text": rule.text,
        "hint": rule.hint,
        "translations": Value::Object(translations),
    })
}

/// Whether the public domain-blocks list is served. Upstream hides it by
/// default (`config/settings.yml:30`: `show_domain_blocks: 'disabled'` →
/// `head 404`), so this answers `false` unless the operator opts in via
/// `domain_blocks_public` (file config; upstream `users`/`all` granularity
/// is not modeled — `true` serves silence/suspend rows with public digests).
fn domain_blocks_public(server: &crate::api::mastodon::ServerInfo) -> bool {
    server.domain_blocks_public
}

fn domain_block_severity_order(severity: AdminDomainSeverity) -> u8 {
    match severity {
        AdminDomainSeverity::Silence => 0,
        AdminDomainSeverity::Suspend => 1,
        AdminDomainSeverity::Noop => 2,
    }
}

/// Upstream `DomainBlock#public_domain` (`app/models/domain_block.rb:87-101`
/// at `8ff11c2b`): the bare domain unless `obfuscate`, in which case the
/// middle characters (past the first and last quarters, dots kept) render
/// as `*`.
fn public_domain(domain: &str, obfuscate: bool) -> String {
    if !obfuscate {
        return domain.to_string();
    }
    let chars: Vec<char> = domain.chars().collect();
    let length = chars.len();
    let visible_ratio = length / 4;
    chars
        .into_iter()
        .enumerate()
        .map(|(i, chr)| {
            if i > visible_ratio && i < length.saturating_sub(visible_ratio) && chr != '.' {
                '*'
            } else {
                chr
            }
        })
        .collect()
}

/// The public domain-block row (`REST::DomainBlockSerializer`:
/// `app/serializers/rest/domain_block_serializer.rb:3-11` — `domain`
/// (obfuscated form), `digest` (SHA-256 hex, `domain_block.rb:103-105`),
/// `severity`, `comment` (only `with_comment`, i.e. when the operator shows
/// rationales; without the rationale setting the comment stays `null`).
pub fn public_domain_block_json(row: &AdminDomainBlock) -> Value {
    json!({
        "domain": public_domain(&row.domain, row.obfuscate),
        "digest": crate::application::crypto::sha256_hex(row.domain.as_bytes()),
        "severity": row.severity.as_str(),
        "comment": row
            .public_comment
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    })
}

/// `GET /api/v1/instance/domain_blocks` — user-facing domain blocks.
///
/// Upstream (`domain_blocks_controller.rb:9-18`): `DomainBlock` rows
/// `with_user_facing_limitations` (`silence`/`suspend` only,
/// `app/models/domain_block.rb:31`) ordered `by_severity`
/// (`noop < silence < suspend`, then domain; `domain_block.rb:33`).
/// Currently 404 (see [`domain_blocks_public`]).
pub async fn domain_blocks_index(State(state): State<ApiState>) -> Response {
    if !domain_blocks_public(&state.server) {
        return ApiError::not_found().into_response();
    }
    let store: &dyn Store = &*state.context.store;
    let rows = match store.admin_domain_blocks_page(None, None, None, INSTANCE_SCAN_CAP) {
        Ok(rows) => rows,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let mut rows: Vec<AdminDomainBlock> = rows
        .into_iter()
        .filter(|row| !matches!(row.severity, AdminDomainSeverity::Noop))
        .collect();
    rows.sort_by(|a, b| {
        domain_block_severity_order(a.severity)
            .cmp(&domain_block_severity_order(b.severity))
            .then_with(|| a.domain.cmp(&b.domain))
    });
    Json(rows.iter().map(public_domain_block_json).collect::<Vec<_>>()).into_response()
}

/// `GET /api/v1/instance/languages` — the locale catalog.
///
/// Upstream (`app/controllers/api/v1/instances/languages_controller.rb:9-12`)
/// renders every key of `LanguagesHelper::SUPPORTED_LOCALES` (the ISO-639
/// catalog, `app/helpers/languages_helper.rb:236`) with its localized name
/// (`LanguagePresenter`, `language_presenter.rb`; `REST::LanguageSerializer`
/// `code`/`name`). This serves the same static shape from the bundled
/// [`LOCALE_CATALOG`] below: every supported code with its autonym, sorted
/// by code. It is a snapshot, not a live CLDR lookup — unknown codes can
/// never appear because the set is fixed (no `name` = code fallback rows).
pub async fn languages_show() -> Response {
    let body: Vec<Value> = LOCALE_CATALOG
        .iter()
        .map(|(code, name)| json!({ "code": code, "name": name }))
        .collect();
    Json(body).into_response()
}

/// Static ISO-639 locale catalog: `(code, localized autonym)` pairs sorted
/// by code (gaps batch G-A).
///
/// Snapshot of upstream's `SUPPORTED_LOCALES` autonyms (rails-i18n): clients
/// use this for language pickers, so the names are the languages' own names
/// (`Français`, `Deutsch`, `日本語`), never the bare code.
const LOCALE_CATALOG: &[(&str, &str)] = &[
    ("ar", "العربية"),
    ("ast", "Asturianu"),
    ("bg", "Български"),
    ("bn", "বাংলা"),
    ("br", "Brezhoneg"),
    ("ca", "Català"),
    ("ckb", "کوردی"),
    ("cs", "Čeština"),
    ("cy", "Cymraeg"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("el", "Ελληνικά"),
    ("en", "English"),
    ("eo", "Esperanto"),
    ("es", "Español"),
    ("et", "Eesti"),
    ("eu", "Euskara"),
    ("fa", "فارسی"),
    ("fi", "Suomi"),
    ("fr", "Français"),
    ("gl", "Galego"),
    ("he", "עברית"),
    ("hi", "हिन्दी"),
    ("hr", "Hrvatski"),
    ("hu", "Magyar"),
    ("hy", "Հայերեն"),
    ("id", "Bahasa Indonesia"),
    ("ie", "Interlingue"),
    ("io", "Ido"),
    ("is", "Íslenska"),
    ("it", "Italiano"),
    ("ja", "日本語"),
    ("ka", "ქართული"),
    ("kab", "Taqbaylit"),
    ("kk", "Қазақша"),
    ("kn", "ಕನ್ನಡ"),
    ("ko", "한국어"),
    ("ku", "Kurdî"),
    ("la", "Latina"),
    ("lt", "Lietuvių"),
    ("lv", "Latviešu"),
    ("ml", "മലയാളം"),
    ("mr", "मराठी"),
    ("ms", "Bahasa Melayu"),
    ("nl", "Nederlands"),
    ("nn", "Nynorsk"),
    ("no", "Norsk"),
    ("oc", "Occitan"),
    ("pl", "Polski"),
    ("pt-BR", "Português (Brasil)"),
    ("pt-PT", "Português (Portugal)"),
    ("ro", "Română"),
    ("ru", "Русский"),
    ("sc", "Sardu"),
    ("si", "සිංහල"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("sq", "Shqip"),
    ("sr", "Српски"),
    ("sr-Latn", "Srpski (latinica)"),
    ("sv", "Svenska"),
    ("ta", "தமிழ்"),
    ("te", "తెలుగు"),
    ("th", "ไทย"),
    ("tr", "Türkçe"),
    ("uk", "Українська"),
    ("ur", "اردو"),
    ("vi", "Tiếng Việt"),
    ("zh-CN", "简体中文"),
    ("zh-HK", "繁體中文（香港）"),
    ("zh-TW", "繁體中文（台灣）"),
];

/// Weeks of instance activity, like upstream (`WEEKS_OF_ACTIVITY = 12`,
/// `app/controllers/api/v1/instances/activity_controller.rb:5`).
const WEEKS_OF_ACTIVITY: u64 = 12;

/// `GET /api/v1/instance/activity` — weekly local activity.
///
/// Upstream (`activity_controller.rb:7-38`): twelve `{week, statuses, logins,
/// registrations}` rows, values as strings, `week` the window-start unix
/// time; statuses count local posts (`activity:statuses:local`), logins
/// count unique sign-ins (`activity:logins`), registrations count new local
/// accounts (`activity:accounts:local`). Upstream 404s unless
/// `Setting.activity_api_enabled` (`activity_controller.rb:44-46`), which
/// defaults to true (`config/settings.yml:28`); wired to
/// `Config::activity_api_enabled` (default true).
pub async fn activity_show(State(state): State<ApiState>) -> Response {
    if !state.server.activity_api_enabled {
        return ApiError::not_found().into_response();
    }
    let store: &dyn Store = &*state.context.store;
    const WEEK_MS: u64 = 7 * 86_400_000;
    let now = state.context.now_ms();
    let accounts = match store.admin_accounts_overview(INSTANCE_SCAN_CAP) {
        Ok(accounts) => accounts,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let statuses = match store.admin_status_metrics(INSTANCE_SCAN_CAP) {
        Ok(statuses) => statuses,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let sign_ins = match store.admin_sign_ins(INSTANCE_SCAN_CAP) {
        Ok(sign_ins) => sign_ins,
        Err(err) => return ApiError::from(err).into_response(),
    };
    let local_ids: HashSet<Id> = accounts
        .iter()
        .filter(|account| account.local)
        .map(|account| account.id)
        .collect();
    let mut out = Vec::with_capacity(WEEKS_OF_ACTIVITY as usize);
    for week in 0..WEEKS_OF_ACTIVITY {
        let end = now.saturating_sub(week * WEEK_MS);
        let start = end.saturating_sub(WEEK_MS);
        let in_window = |at: u64| at >= start && at <= end;
        let statuses_count = statuses
            .iter()
            .filter(|row| local_ids.contains(&row.account_id) && in_window(row.created_at_ms))
            .count();
        let logins_count: HashSet<Id> = sign_ins
            .iter()
            .filter(|(_, at)| in_window(*at))
            .map(|(who, _)| *who)
            .collect();
        let registrations_count = accounts
            .iter()
            .filter(|account| account.local && in_window(account.created_at_ms))
            .count();
        out.push(json!({
            "week": (start / 1_000).to_string(),
            "statuses": statuses_count.to_string(),
            "logins": logins_count.len().to_string(),
            "registrations": registrations_count.to_string(),
        }));
    }
    Json(out).into_response()
}

/// Read an operator-authored site text file at serve time (gaps batch G-A;
/// `Config::validate` deliberately checks shape only, never existence).
///
/// Returns the file content plus its modification time. Any failure (no path
/// configured, unreadable file, unreadable mtime) is `None`: the resource
/// keeps its honest-absence shape and no filesystem detail ever leaks.
fn read_site_file(path: Option<&std::path::Path>) -> Option<(String, std::time::SystemTime)> {
    let content = std::fs::read_to_string(path?);
    let mtime = std::fs::metadata(path?).and_then(|meta| meta.modified());
    match (content, mtime) {
        (Ok(text), Ok(mtime)) => Some((text, mtime)),
        _ => None,
    }
}

/// Format a `SystemTime` as Mastodon's ISO-8601 with milliseconds
/// (`2025-04-15T12:00:00.000Z`).
fn rfc3339_ms_time(time: std::time::SystemTime) -> String {
    let ms: i64 = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// Format a `SystemTime` as a `YYYY-MM-DD` date (terms `effective_date`).
fn date_only_time(time: std::time::SystemTime) -> String {
    let ms: i64 = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.date_naive().to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// `GET /api/v1/instance/extended_description`.
///
/// Upstream (`extended_descriptions_controller.rb:16-19`) renders the
/// `ExtendedDescription` row with `REST::ExtendedDescriptionSerializer`
/// (`updated_at`, `content`); when no description is set
/// (`ExtendedDescription.current`, `app/models/extended_description.rb:5-13`)
/// the row is empty: `updated_at` renders `null` (`&.` on nil) and blank
/// text renders `''`. A configured `--site-extended-file=` serves the file
/// content with the file mtime as `updated_at` (content served verbatim —
/// the operator authors the text shown on the about page); unconfigured
/// keeps the unset shape.
pub async fn extended_description_show(State(state): State<ApiState>) -> Response {
    match read_site_file(state.server.site_extended_file.as_deref()) {
        Some((content, mtime)) => Json(json!({
            "updated_at": rfc3339_ms_time(mtime),
            "content": content,
        }))
        .into_response(),
        None => Json(json!({ "updated_at": Value::Null, "content": "" })).into_response(),
    }
}

/// `GET /api/v1/instance/translation_languages`.
///
/// Upstream (`translation_languages_controller.rb:9-16`) renders `{}` when
/// no translation service is configured. No translation provider is
/// configured here, so `{}` is served — the exact upstream unconfigured
/// shape, not a stub.
pub async fn translation_languages_show() -> Response {
    Json(json!({})).into_response()
}

/// `GET /api/v1/instance/privacy_policy`.
///
/// Upstream (`privacy_policies_controller.rb:7-10`) serves `PrivacyPolicy`
/// with `REST::PrivacyPolicySerializer` (`updated_at`, `content`);
/// `PrivacyPolicy.current` falls back to the bundled default template
/// (`app/models/privacy_policy.rb:3-15`). A configured
/// `--site-privacy-file=` serves the file content with the file mtime as
/// `updated_at` (content verbatim); unconfigured answers 404 rather than
/// fabricating a policy.
pub async fn privacy_policy_show(State(state): State<ApiState>) -> Response {
    match read_site_file(state.server.site_privacy_file.as_deref()) {
        Some((content, mtime)) => Json(json!({
            "updated_at": rfc3339_ms_time(mtime),
            "content": content,
        }))
        .into_response(),
        None => ApiError::not_found().into_response(),
    }
}

/// Render the file-backed terms of service (`REST::TermsOfServiceSerializer`:
/// `effective_date`, `effective`, `content`, `succeeded_by`). The single
/// configured file is the terms in force: `effective_date` is the file
/// mtime's date, `effective` is always true, `succeeded_by` is null (one
/// file holds no version chain). Content is served verbatim.
fn terms_of_service_json(content: &str, mtime: std::time::SystemTime) -> Value {
    json!({
        "effective_date": date_only_time(mtime),
        "effective": true,
        "content": content,
        "succeeded_by": Value::Null,
    })
}

/// `GET /api/v1/instance/terms_of_service`.
///
/// Upstream (`terms_of_service_controller.rb:7-10`) raises
/// `ActiveRecord::RecordNotFound` (404) when no terms are in force. A
/// configured `--site-terms-file=` serves the current terms; unconfigured
/// answers 404 — the upstream unconfigured behavior.
pub async fn terms_of_service_index(State(state): State<ApiState>) -> Response {
    match read_site_file(state.server.site_terms_file.as_deref()) {
        Some((content, mtime)) => Json(terms_of_service_json(&content, mtime)).into_response(),
        None => ApiError::not_found().into_response(),
    }
}

/// `GET /api/v1/instance/terms_of_service/:date`.
///
/// Upstream (`terms_of_service_controller.rb:12-15`) 404s on an unknown
/// effective date. The file-backed store holds one version, so only the
/// file mtime's date matches; every other date (and the unconfigured case)
/// 404s.
pub async fn terms_of_service_show(
    State(state): State<ApiState>,
    Path(date): Path<String>,
) -> Response {
    match read_site_file(state.server.site_terms_file.as_deref()) {
        Some((content, mtime)) if date_only_time(mtime) == date => {
            Json(terms_of_service_json(&content, mtime)).into_response()
        }
        _ => ApiError::not_found().into_response(),
    }
}

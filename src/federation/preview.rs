//! Link-preview crawler (D40): fills a trend link's preview-card metadata
//! from the linked page's OpenGraph tags.
//!
//! The fetch goes through the same transport policy as every other remote
//! fetch: URL shape + SSRF checks per hop
//! ([`crate::federation::policy::check_url`]), at most
//! [`MAX_REDIRECTS`] redirects (each re-validated), the connect + total
//! timeouts of the [`crate::federation::net::SafeHttpConfig`] client, and a
//! [`MAX_PREVIEW_BYTES`] body cap. Only `text/html` pages are parsed.
//!
//! Outcome mapping: another try could help (transport errors, 5xx, 429) →
//! retry with the queue's backoff; a stable answer (4xx, refused URL, too
//! large, not HTML) → quarantine, terminal. A row that already carries a
//! title is a no-op, so retried jobs never refetch. Nothing is fabricated:
//! a page without tags keeps `null` card fields.
//!
//! Cap note: [`MAX_PREVIEW_BYTES`] matches the remote-document fetch cap
//! (200 KB); the safe-client limits are shared with the federation edge
//! work (Subagent E) and tighten together.

use futures_util::StreamExt;
use url::Url;

use crate::application::ports::Store;
use crate::domain::{Id, Job};
use crate::federation::delivery::DeliveryError;
use crate::federation::net::SafeHttpConfig;
use crate::federation::policy;

/// Largest preview page accepted (matches the remote-document fetch cap).
pub const MAX_PREVIEW_BYTES: u64 = 200 * 1024;
/// Redirect hops followed per fetch (each re-validated against the policy).
pub const MAX_REDIRECTS: usize = 3;
/// Stored-field length bounds (a hostile page cannot grow a row without bound).
pub const MAX_TITLE_CHARS: usize = 512;
pub const MAX_DESCRIPTION_CHARS: usize = 2048;
pub const MAX_NAME_CHARS: usize = 256;
pub const MAX_URL_CHARS: usize = 2048;

/// Card fields parsed from one page (`None` = the page did not carry it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub provider_name: Option<String>,
    pub provider_url: Option<String>,
    pub image_url: Option<String>,
}

fn truncate(value: String, max_chars: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(max_chars).collect())
}

/// The `content` of the first `<meta>` tag whose `property` (or `name`) is
/// one of `keys` (ASCII case-insensitive on tag, attribute names and the
/// key match; values keep their case).
fn meta_content(html: &str, keys: &[&str]) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0usize;
    while let Some(start) = lower[cursor..].find("<meta") {
        let tag_start = cursor + start;
        let rest = &lower[tag_start..];
        let Some(end) = rest.find('>') else {
            break;
        };
        let tag = &html[tag_start..tag_start + end];
        cursor = tag_start + end + 1;
        let attrs = parse_attributes(tag);
        let key = attrs
            .iter()
            .find(|(name, _)| name == "property" || name == "name")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        if keys.iter().any(|want| key.eq_ignore_ascii_case(want)) {
            let content = attrs
                .iter()
                .find(|(name, _)| name == "content")
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            if !content.trim().is_empty() {
                return Some(content);
            }
        }
    }
    None
}

/// `(lowercased-name, value)` pairs of one HTML tag's attributes.
/// Handles quoted (`"`/`'`) and bare values; malformed tags yield what
/// parses.
fn parse_attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = tag.as_bytes();
    let mut i = 0usize;
    // Skip the tag name.
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/' || bytes[i] == b'>') {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b':' || bytes[i] == b'_') {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name: String = tag[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let value: String;
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let value_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                value = tag[value_start..i].to_string();
                i += 1;
            } else {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = tag[value_start..i].to_string();
            }
            out.push((name, value));
        } else {
            out.push((name, String::new()));
        }
    }
    out
}

/// The page's `<title>` text, if any.
fn title_tag(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let content_start = lower[start..].find('>')? + start + 1;
    let end = lower[content_start..].find("</title>")? + content_start;
    let text = html[content_start..end].trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Parse preview-card fields from a page body. `page_url` resolves a
/// relative `og:image` and supplies the provider origin fallback.
pub fn parse_card(html: &str, page_url: &Url) -> CardFields {
    let title = meta_content(html, &["og:title", "twitter:title"])
        .or_else(|| title_tag(html))
        .and_then(|v| truncate(v, MAX_TITLE_CHARS));
    let description = meta_content(html, &["og:description", "twitter:description", "description"])
        .and_then(|v| truncate(v, MAX_DESCRIPTION_CHARS));
    let provider_name = meta_content(html, &["og:site_name"])
        .and_then(|v| truncate(v, MAX_NAME_CHARS));
    let image_url = meta_content(html, &["og:image", "og:image:url", "twitter:image"])
        .and_then(|src| {
            let resolved = page_url.join(src.trim()).ok()?.to_string();
            let parsed = Url::parse(&resolved).ok()?;
            if parsed.scheme() == "http" || parsed.scheme() == "https" {
                truncate(resolved, MAX_URL_CHARS)
            } else {
                None
            }
        });
    // The page origin travels as the provider fallback, but only when the
    // page carried at least one real tag: a tag-less page keeps the
    // all-`None` shape (never fabricated).
    let has_signal = title.is_some()
        || description.is_some()
        || provider_name.is_some()
        || image_url.is_some();
    let provider_url = if !has_signal {
        None
    } else {
        // `ascii_serialization` renders tuple origins without a trailing
        // slash (`https://example.com`) and opaque origins as `null`.
        let origin = page_url.origin().ascii_serialization();
        if origin.is_empty() || origin == "null" {
            None
        } else {
            truncate(origin, MAX_URL_CHARS)
        }
    };
    CardFields {
        title,
        description,
        provider_name,
        provider_url,
        image_url,
    }
}

/// Fetch one trend link's preview metadata and store it. Designed for the
/// async job worker; tests call it directly with their own client.
pub async fn fetch(
    store: &dyn Store,
    client: &reqwest::Client,
    http: &SafeHttpConfig,
    job: &Job,
) -> std::result::Result<(), DeliveryError> {
    let link_id = job
        .payload
        .get("link_id")
        .and_then(|v| v.as_u64())
        .map(Id::new)
        .ok_or_else(|| DeliveryError::Quarantine("preview job has no link".into()))?;
    let url_text = job
        .payload
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DeliveryError::Quarantine("preview job has no url".into()))?;
    let retry = |e: crate::domain::Error| DeliveryError::Retry(e.to_string());
    let row = store.trend_link_by_id(link_id).map_err(retry)?.ok_or_else(|| {
        DeliveryError::Quarantine(format!("preview link {link_id} is gone"))
    })?;
    if row.title.is_some() {
        // Metadata already arrived (a retried job): nothing to do.
        return Ok(());
    }
    if row.url != url_text {
        return Err(DeliveryError::Quarantine("preview job url no longer matches".into()));
    }
    let mut url = Url::parse(&row.url).map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
    if let Err(e) = policy::check_url(&url, http.allow_http, &http.allowlist) {
        return Err(DeliveryError::Quarantine(format!("preview url refused: {e}")));
    }

    // Manual redirect chain (the safe client never follows redirects
    // itself): each hop is re-validated against the policy.
    let mut hops = 0usize;
    let body: Vec<u8> = loop {
        let response = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|e| DeliveryError::Retry(e.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            hops += 1;
            if hops > MAX_REDIRECTS {
                return Err(DeliveryError::Quarantine("preview redirect limit reached".into()));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| DeliveryError::Quarantine("preview redirect without a location".into()))?
                .to_string();
            url = url
                .join(&location)
                .map_err(|e| DeliveryError::Quarantine(e.to_string()))?;
            if let Err(e) = policy::check_url(&url, http.allow_http, &http.allowlist) {
                return Err(DeliveryError::Quarantine(format!(
                    "preview redirect refused: {e}"
                )));
            }
            continue;
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(DeliveryError::Retry(format!("preview server answered {status}")));
        }
        if !status.is_success() {
            return Err(DeliveryError::Quarantine(format!(
                "preview server answered {status}"
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !content_type.contains("html") {
            return Err(DeliveryError::Quarantine(format!(
                "preview is not an HTML page ({content_type})"
            )));
        }
        if response.content_length().is_some_and(|len| len > MAX_PREVIEW_BYTES) {
            return Err(DeliveryError::Quarantine("preview page is larger than the limit".into()));
        }
        let mut collected: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        let mut too_large = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| DeliveryError::Retry(e.to_string()))?;
            if collected.len() as u64 + chunk.len() as u64 > MAX_PREVIEW_BYTES {
                too_large = true;
                break;
            }
            collected.extend_from_slice(&chunk);
        }
        if too_large {
            return Err(DeliveryError::Quarantine("preview page is larger than the limit".into()));
        }
        break collected;
    };

    let html = String::from_utf8_lossy(&body);
    let fields = parse_card(&html, &url);
    // `now_ms` is unused by the store update (card fields carry no clock);
    // the job's own timestamp travels instead.
    store
        .update_trend_link_card(
            link_id,
            fields.title,
            fields.description,
            fields.provider_name,
            fields.provider_url,
            fields.image_url,
            job.created_at_ms,
        )
        .map_err(retry)?;
    Ok(())
}

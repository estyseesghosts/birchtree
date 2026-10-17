//! JSON request bodies (Phase 4.1, register D11).
//!
//! Upstream reads a JSON body with Rails' default parameter parser, so a
//! handler sees the same params whether the client sent JSON or a form. This
//! module flattens a JSON body into the ordered key/value pairs that
//! [`crate::api::forms`] already produces:
//!
//! - nested objects become `a[b]` keys;
//! - arrays of scalars repeat `a[]`;
//! - arrays of objects use indexed keys `a[N][b]`, the same encoding as the
//!   `keywords_attributes[N][...]` form params that filters read;
//! - numbers and booleans become their decimal and `true`/`false` text;
//! - `null` is absent, as in Rails' form encoding.
//!
//! Malformed JSON is rejected by [`guard`] before any handler runs, as Rails
//! rejects it before any action (its `ParseError` is raised while params are
//! read). The answer matches `ActionDispatch::PublicExceptions` at v4.7.3.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

/// Largest JSON body read for parameters. Matches the form cap in
/// [`crate::api::forms`].
pub const MAX_JSON_BODY: usize = 1024 * 1024;

/// Whether the request declares a JSON body (`application/json`, with or
/// without parameters such as `charset`).
pub fn is_json(headers: &HeaderMap) -> bool {
    media_type(headers).as_deref() == Some("application/json")
}

/// Parameters from a request body, JSON or form-encoded. A zero-length body
/// yields none, as in Rails.
pub fn body_pairs(headers: &HeaderMap, bytes: &[u8]) -> Vec<(String, String)> {
    if is_json(headers) {
        // The router's guard has already rejected malformed JSON, so an
        // unparsable body here yields no parameters.
        parse(bytes).unwrap_or_default()
    } else {
        crate::api::forms::parse(bytes)
    }
}

/// Flatten a JSON body into ordered key/value pairs. `None` when the bytes are
/// not JSON. A zero-length body yields no pairs.
pub fn parse(bytes: &[u8]) -> Option<Vec<(String, String)>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut out = Vec::new();
    match value {
        Value::Object(map) => {
            for (key, value) in &map {
                push(&mut out, key.clone(), value);
            }
        }
        // Rails wraps a non-object top-level value under `_json`.
        other => push(&mut out, "_json".to_string(), &other),
    }
    Some(out)
}

fn push(out: &mut Vec<(String, String)>, key: String, value: &Value) {
    match value {
        Value::Null => {}
        Value::Bool(flag) => out.push((key, flag.to_string())),
        Value::Number(number) => out.push((key, number.to_string())),
        Value::String(text) => out.push((key, text.clone())),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                match item {
                    Value::Object(map) => {
                        for (field, value) in map {
                            push(out, format!("{key}[{index}][{field}]"), value);
                        }
                    }
                    _ => push(out, format!("{key}[]"), item),
                }
            }
        }
        Value::Object(map) => {
            for (field, value) in map {
                push(out, format!("{key}[{field}]"), value);
            }
        }
    }
}

/// Reject a malformed JSON body with upstream's response, for every route.
///
/// Rails parses params before the action runs, so a malformed body fails even
/// when the endpoint ignores its body. The guard reads the body (bounded by
/// [`MAX_JSON_BODY`]), validates it, and passes the same bytes on.
pub async fn guard(request: Request, next: Next) -> Response {
    if !is_json(request.headers()) {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_JSON_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    if parse(&bytes).is_none() {
        return malformed(&parts.headers);
    }
    next.run(Request::from_parts(parts, Body::from(bytes))).await
}

/// Upstream's answer to Rails' `ParseError`, which `ShowExceptions` hands to
/// `ActionDispatch::PublicExceptions`:
///
/// - when the first negotiated format is JSON, `400` with the body
///   `{"status":400,"error":"Bad Request"}`;
/// - otherwise `400` with an empty `text/html` body. v4.7.3 ships no
///   `public/400.html`, so the exceptions app answers `404` with
///   `X-Cascade: pass`, and `ShowExceptions` turns that into this empty `400`.
fn malformed(headers: &HeaderMap) -> Response {
    if negotiated_json(headers) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            r#"{"status":400,"error":"Bad Request"}"#,
        )
            .into_response();
    }
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
    )
        .into_response()
}

/// `request.formats.first` for a request whose params cannot be parsed
/// (`ActionDispatch::Http::MimeNegotiation#formats`). Path extensions are not
/// considered: the routes in this crate have none.
fn negotiated_json(headers: &HeaderMap) -> bool {
    let accept = header_text(headers, header::ACCEPT.as_str()).trim();
    let xhr = header_text(headers, "x-requested-with") == "XMLHttpRequest";
    let content = media_type(headers);
    let valid_accept = (xhr && (!accept.is_empty() || content.is_some()))
        || (!accept.is_empty() && !browser_like(accept));
    let first = if !valid_accept {
        None
    } else if accept.is_empty() {
        content
    } else {
        first_accepted(accept)
    };
    first.as_deref() == Some("application/json")
}

/// Upstream's `BROWSER_LIKE_ACCEPTS`: a `*/*` listed alongside other types.
fn browser_like(accept: &str) -> bool {
    let parts: Vec<&str> = accept.split(',').collect();
    parts.iter().enumerate().any(|(index, part)| {
        (index > 0 && part.trim_start().starts_with("*/*"))
            || (index + 1 < parts.len() && part.trim_end().ends_with("*/*"))
    })
}

/// The highest-q media type in an `Accept` header; the first listed wins a tie.
fn first_accepted(accept: &str) -> Option<String> {
    let mut best: Option<(f32, String)> = None;
    for entry in accept.split(',') {
        let mut fields = entry.split(';');
        let media = fields.next().unwrap_or("").trim().to_ascii_lowercase();
        if media.is_empty() {
            continue;
        }
        let quality = fields
            .find_map(|field| field.trim().strip_prefix("q="))
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(1.0);
        if best.as_ref().is_none_or(|(known, _)| quality > *known) {
            best = Some((quality, media));
        }
    }
    best.map(|(_, media)| media)
}

/// The media type of `Content-Type`, lowercased, without parameters.
fn media_type(headers: &HeaderMap) -> Option<String> {
    let raw = header_text(headers, header::CONTENT_TYPE.as_str());
    let media = raw.split(';').next()?.trim().to_ascii_lowercase();
    (!media.is_empty()).then_some(media)
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(json: &str) -> Vec<(String, String)> {
        parse(json.as_bytes()).expect("valid json")
    }

    fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Vec<&'a str> {
        pairs.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str()).collect()
    }

    #[test]
    fn flattens_scalars_objects_and_arrays() {
        let out = pairs(
            r#"{"status":"hi","sensitive":true,"n":3,"gone":null,
                "poll":{"options":["a","b"],"expires_in":300},
                "keywords_attributes":[{"keyword":"x"},{"keyword":"y"}]}"#,
        );
        assert_eq!(get(&out, "status"), vec!["hi"]);
        assert_eq!(get(&out, "sensitive"), vec!["true"]);
        assert_eq!(get(&out, "n"), vec!["3"]);
        assert!(get(&out, "gone").is_empty());
        assert_eq!(get(&out, "poll[options][]"), vec!["a", "b"]);
        assert_eq!(get(&out, "poll[expires_in]"), vec!["300"]);
        assert_eq!(get(&out, "keywords_attributes[0][keyword]"), vec!["x"]);
        assert_eq!(get(&out, "keywords_attributes[1][keyword]"), vec!["y"]);
    }

    #[test]
    fn false_is_kept_and_empty_bodies_have_no_pairs() {
        assert_eq!(get(&pairs(r#"{"notifications":false}"#), "notifications"), vec!["false"]);
        assert!(parse(b"").expect("empty ok").is_empty());
    }

    #[test]
    fn malformed_json_is_none() {
        assert!(parse(b"{\"status\":").is_none());
        assert!(parse(b"   ").is_none());
    }

    #[test]
    fn negotiation_follows_accept() {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, "application/json".parse().unwrap());
        assert!(negotiated_json(&h));
        h.insert(header::ACCEPT, "*/*".parse().unwrap());
        assert!(!negotiated_json(&h));
        h.insert(header::ACCEPT, "text/html, application/json".parse().unwrap());
        assert!(!negotiated_json(&h));
        h.insert(header::ACCEPT, "application/json;q=0.9, text/html;q=0.5".parse().unwrap());
        assert!(negotiated_json(&h));
        h.insert(header::ACCEPT, "text/html, */*".parse().unwrap());
        assert!(!negotiated_json(&h));
        // No Accept header: Rails falls back to HTML, whatever the body type.
        assert!(!negotiated_json(&HeaderMap::new()));
    }
}

//! Pagination and `Link` headers.
//!
//! Cursors are numeric `Id`s compared by value (never lexicographically).
//! `max_id`/`since_id` page descending; `min_id` pages ascending. The `Link`
//! header preserves the original query and swaps only the cursor parameter,
//! matching Mastodon's `Link` output shape.

use axum::http::Uri;

use crate::api::mastodon::errors::{ApiError, ApiResult};
use crate::domain::Id;

/// Parsed pagination parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub limit: usize,
    pub max_id: Option<Id>,
    pub since_id: Option<Id>,
    pub min_id: Option<Id>,
}

/// Parse and bound pagination parameters.
///
/// Negative values are rejected with Mastodon's message; the limit is clamped
/// to `[1, max_limit]` (Mastodon clamps to twice the endpoint default unless a
/// per-endpoint maximum is given).
pub fn parse(
    limit: Option<&str>,
    max_id: Option<&str>,
    since_id: Option<&str>,
    min_id: Option<&str>,
    default_limit: usize,
    max_limit: usize,
) -> ApiResult<Page> {
    let limit = match limit {
        None => default_limit,
        Some(raw) => {
            let value: i64 = raw
                .parse()
                .map_err(|_| ApiError::bad_request("Invalid `limit` value"))?;
            if value < 0 {
                return Err(ApiError::bad_request(
                    "Pagination values for `offset` and `limit` must be positive",
                ));
            }
            if value == 0 {
                default_limit
            } else {
                (value as usize).min(max_limit)
            }
        }
    };
    Ok(Page {
        limit,
        max_id: parse_id(max_id)?,
        since_id: parse_id(since_id)?,
        min_id: parse_id(min_id)?,
    })
}

fn parse_id(raw: Option<&str>) -> ApiResult<Option<Id>> {
    match raw {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<Id>()
            .map(Some)
            .map_err(|_| ApiError::bad_request("Invalid cursor value")),
    }
}

/// Build a `Link` header pointing at `next` (via `max_id`) and `prev` (via
/// `min_id`). Either may be `None`; an empty result omits the header entirely.
pub fn link_header(
    base: &Uri,
    next_max_id: Option<Id>,
    prev_min_id: Option<Id>,
) -> Option<String> {
    link_header_with(base, &["max_id", "min_id"], next_max_id, "min_id", prev_min_id)
}

/// `Link` header for descending `max_id`/`since_id` lists (Mastodon's
/// `paginate_by_max_id`, used by `GET /api/v1/domain_blocks`): `next` pages
/// with `max_id`, `prev` with `since_id`. Either may be `None`.
pub fn link_header_desc(
    base: &Uri,
    next_max_id: Option<Id>,
    prev_since_id: Option<Id>,
) -> Option<String> {
    link_header_with(base, &["max_id", "since_id"], next_max_id, "since_id", prev_since_id)
}

fn link_header_with(
    base: &Uri,
    drop: &[&str],
    next_max_id: Option<Id>,
    prev_key: &str,
    prev_id: Option<Id>,
) -> Option<String> {
    let mut links = Vec::new();
    if let Some(id) = next_max_id {
        links.push(format!(
            "<{}>; rel=\"next\"",
            with_query_param(base, drop, "max_id", &id.to_string())
        ));
    }
    if let Some(id) = prev_id {
        links.push(format!(
            "<{}>; rel=\"prev\"",
            with_query_param(base, drop, prev_key, &id.to_string())
        ));
    }
    if links.is_empty() {
        None
    } else {
        Some(links.join(", "))
    }
}

/// Return `base` with the `drop` parameters removed and `key=value` appended.
fn with_query_param(base: &Uri, drop: &[&str], key: &str, value: &str) -> String {
    let path = base.path();
    let kept: Vec<(String, String)> = base
        .query()
        .map(|q| {
            q.split('&')
                .filter(|pair| !pair.is_empty())
                .filter_map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    if drop.contains(&k) {
                        None
                    } else {
                        Some((k.to_string(), v.to_string()))
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut query = kept
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>();
    query.push(format!("{key}={value}"));
    format!("{path}?{}", query.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    #[test]
    fn limit_is_clamped_to_max() {
        assert_eq!(parse(None, None, None, None, 20, 40).unwrap().limit, 20);
        assert_eq!(parse(Some("5"), None, None, None, 20, 40).unwrap().limit, 5);
        assert_eq!(parse(Some("999"), None, None, None, 20, 40).unwrap().limit, 40);
        assert_eq!(parse(Some("0"), None, None, None, 20, 40).unwrap().limit, 20);
    }

    #[test]
    fn negative_limit_is_rejected() {
        let err = parse(Some("-1"), None, None, None, 20, 40).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn cursors_parse_numerically() {
        let page = parse(None, Some("1000"), Some("999"), None, 20, 40).unwrap();
        assert_eq!(page.max_id, Some(Id::new(1000)));
        assert_eq!(page.since_id, Some(Id::new(999)));
    }

    #[test]
    fn link_header_swaps_cursor_and_preserves_query() {
        let base = uri("/api/v1/accounts/1/statuses?limit=20&max_id=500");
        let header = link_header(&base, Some(Id::new(499)), Some(Id::new(2))).unwrap();
        assert_eq!(
            header,
            "</api/v1/accounts/1/statuses?limit=20&max_id=499>; rel=\"next\", </api/v1/accounts/1/statuses?limit=20&min_id=2>; rel=\"prev\""
        );
    }
}

//! Minimal `application/x-www-form-urlencoded` parsing.
//!
//! Mastodon clients send form-encoded bodies (and the OAuth endpoints use
//! them exclusively). The std/`axum` form extractors lose repeated keys and do
//! not handle both `key` and `key[]` conventions, so we parse the raw body into
//! an ordered list of key/value pairs.

/// Parse a form body into ordered key/value pairs (percent- and `+`-decoded).
pub fn parse(body: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(body);
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(pair), String::new()),
        })
        .collect()
}

/// Percent-decode a form component (`+` becomes a space).
pub fn decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]);
                let lo = hex_val(bytes[i + 2]);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi << 4) | lo);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// First value for `key`.
pub fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// First value for any of `keys`.
pub fn first<'a>(pairs: &'a [(String, String)], keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| get(pairs, key))
}

/// All values for `key`, accepting both `key` and `key[]`.
pub fn all<'a>(pairs: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    let bracket = format!("{key}[]");
    pairs
        .iter()
        .filter(|(k, _)| k == key || k == &bracket)
        .map(|(_, v)| v.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_and_plus() {
        let pairs = parse(b"a=hello+world&b=%2Fpath");
        assert_eq!(get(&pairs, "a"), Some("hello world"));
        assert_eq!(get(&pairs, "b"), Some("/path"));
    }

    #[test]
    fn collects_repeated_and_bracketed_keys() {
        let pairs = parse(b"redirect_uris[]=a&redirect_uris[]=b&x=1");
        assert_eq!(all(&pairs, "redirect_uris"), vec!["a", "b"]);
        assert_eq!(first(&pairs, &["x", "y"]), Some("1"));
    }
}

// ---------------------------------------------------------------------------
// Axum extractor.
// ---------------------------------------------------------------------------

/// A form body extractor that tolerates absent or empty bodies.
///
/// Mastodon endpoints like `POST /api/v1/statuses/:id/reblog` or
/// `POST /api/v1/accounts/:id/follow` accept requests with no body at all;
/// axum's [`axum::extract::RawForm`] rejects those with `415`. This extractor
/// parses whatever bytes are present as `application/x-www-form-urlencoded`
/// (an empty body yields no pairs). JSON request bodies are not accepted —
/// recorded as a compatibility gap in `docs/endpoint-manifest.md`.
pub struct LenientForm(pub Vec<(String, String)>);

/// Cap for a manually read body (the router's `DefaultBodyLimit` applies to
/// the request as a whole; this is a second, explicit bound).
const MAX_FORM_BODY: usize = 1024 * 1024;

impl<S> axum::extract::FromRequest<S> for LenientForm
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request(
        req: axum::extract::Request,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let (_parts, body) = req.into_parts();
        let bytes = axum::body::to_bytes(body, MAX_FORM_BODY)
            .await
            .unwrap_or_default();
        Ok(LenientForm(parse(&bytes)))
    }
}

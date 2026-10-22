//! Rules of the public media route (`/system/media_attachments/files/…`).
//!
//! The route serves bytes straight from the blob stores, so it must be
//! impossible to reach anything that is not a published media object:
//! staging siblings, in-flight downloads, transcode scratch blobs, other
//! namespaces (custom emoji have their own route) and traversal tricks are
//! all refused *by key shape* before any file is opened. The accepted shapes
//! are exactly the ones this server writes:
//!
//! - `{aa}/{bb}/{16 hex}/{style}.{ext}` — attachments and profile images
//!   (`style` is `original`, `small`, `thumbnail`, `avatar` or `header`);
//! - `{ddd}/{ddd}/{ddd}/{original|small}/{name}.{ext}` — pre-overhaul
//!   Mastodon-shaped keys that older databases still reference;
//! - `remote/{id}` — cached remote attachment bytes.
//!
//! Also here: the single-range `Range` parser used for audio/video seeking,
//! the in-process single-flight gate for cache-miss refetches, and the
//! key-shape gate for the custom-emoji image route
//! ([`emoji_blob_key`], used by the emoji endpoint the same way
//! [`classify_key`] guards the attachment route).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::media::remote_cache;

/// Styles a published attachment or profile image may carry.
const PUBLISHED_STYLES: [&str; 5] = ["original", "small", "thumbnail", "avatar", "header"];

/// Styles an emoji image may carry.
const EMOJI_STYLES: [&str; 2] = ["original", "static"];

/// What a requested public path addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicMediaKind {
    /// Durable local bytes (uploads, profile images).
    Local,
    /// Cached remote attachment bytes for the media row `media_id`.
    RemoteCache { media_id: u64 },
    /// A derived preview or cover image of cached remote media.
    RemotePreview { media_id: u64 },
}

/// A sibling key of `file_key` for another style (`small`, `thumbnail`,
/// `derived`): same shard directory for sharded keys, and
/// `remote/<id>.<style>.<ext>` for cache keys (which have no shard
/// directory of their own, so a plain sibling would be shared by every
/// remote attachment).
pub fn sibling_key(file_key: &str, style: &str, ext: &str) -> Option<String> {
    if let Some(id) = file_key.strip_prefix("remote/") {
        if !id.is_empty() && !id.contains(['/', '.']) {
            return Some(format!("remote/{id}.{style}.{ext}"));
        }
        return None;
    }
    let (dir, _) = file_key.rsplit_once('/')?;
    if dir.is_empty() {
        return None;
    }
    Some(format!("{dir}/{style}.{ext}"))
}

/// Classify a requested path, or `None` when it is not a published media
/// key. `None` must be answered with 404 without touching the filesystem.
pub fn classify_key(path: &str) -> Option<PublicMediaKind> {
    if path.is_empty() || path.len() > 255 || !path.is_ascii() {
        return None;
    }
    if crate::media::blob_store::validate_key(path).is_err() {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["remote", id] => {
            if let Some((digits, rest)) = id.split_once('.') {
                let media_id: u64 = parse_digits(digits)?;
                let (style, ext) = rest.split_once('.')?;
                if matches!(style, "small" | "thumbnail") && is_extension(ext) {
                    return Some(PublicMediaKind::RemotePreview { media_id });
                }
                return None;
            }
            let media_id: u64 = parse_digits(id)?;
            Some(PublicMediaKind::RemoteCache { media_id })
        }
        [first, second, shard, file] => {
            let sharded =
                is_lower_hex(first, 2) && is_lower_hex(second, 2) && is_lower_hex(shard, 16);
            if sharded && is_published_file(file) {
                return Some(PublicMediaKind::Local);
            }
            None
        }
        [first, second, third, style, file] => {
            let legacy = [first, second, third]
                .iter()
                .all(|part| part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()));
            let styled = matches!(*style, "original" | "small");
            if legacy && styled && is_plain_file_name(file) {
                return Some(PublicMediaKind::Local);
            }
            None
        }
        _ => None,
    }
}

/// The full blob key for a requested emoji path, or `None` when the path is
/// not a published emoji image. `None` must be answered with 404 without
/// touching the filesystem.
///
/// Accepted shapes are exactly the ones the server writes
/// (`application::emoji::add_emoji`):
///
/// - `{p0}/{p1}/{shortcode}/original/{shortcode}.png` — local emoji;
/// - `{p0}/{p1}/{shortcode}/static/{shortcode}.png` — local static variant;
/// - `.../{shortcode}/original/{shortcode}@{domain}.png` (and `static`) —
///   namespaced generations: remote-origin rows staged through this route
///   before the `remote/emojis/` split, and local replacement generations
///   (`@restage-{rand}`).
///
/// Remote-origin emoji now live under `remote/emojis/` (see
/// [`remote_emoji_blob_key`]); this gate keeps serving their legacy
/// `custom_emojis/` rows until refetch rewrites them.
///
/// `{p0}/{p1}` must be the hex of the shortcode's first two bytes (the
/// writer's partition), so staging siblings, scratch files, other namespaces,
/// traversal tricks and guessed cross-emoji paths are all refused by shape.
pub fn emoji_blob_key(path: &str) -> Option<String> {
    if path.is_empty() || path.len() > 255 || !path.is_ascii() {
        return None;
    }
    let key: String = format!("custom_emojis/images/{path}");
    if crate::media::blob_store::validate_key(&key).is_err() {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let [p0, p1, dir, style, file] = parts.as_slice() else {
        return None;
    };
    if !is_lower_hex(p0, 2) || !is_lower_hex(p1, 2) {
        return None;
    }
    if !crate::domain::CustomEmoji::shortcode_valid(dir) {
        return None;
    }
    let bytes: &[u8] = dir.as_bytes();
    if *p0 != format!("{:02x}", bytes[0]) || *p1 != format!("{:02x}", bytes[1]) {
        return None;
    }
    if !EMOJI_STYLES.contains(style) {
        return None;
    }
    let stem: &str = file.strip_suffix(".png")?;
    if stem == *dir {
        // Local emoji: the file name repeats the partition directory.
    } else if let Some((short, domain)) = stem.split_once('@') {
        if short != *dir || domain.contains('@') || !is_emoji_domain(domain) {
            return None;
        }
    } else {
        return None;
    }
    Some(key)
}

/// The full blob key for a requested remote-emoji image, or `None` when the
/// path is not a published remote-emoji image. `None` must be answered with
/// 404 without touching the filesystem.
///
/// Accepted shapes are exactly the ones the fetch path writes (see
/// [`remote_cache::remote_emoji_keys`]): `remote/emojis/{domain}/{shortcode}/`
/// `{original|static}.png`, with a strictly validated lowercase domain and
/// shortcode. Guessed names, traversal, backslashes, staging siblings and
/// other namespaces are all refused by shape — exactly like the attachment
/// route ([`classify_key`]) and the local-emoji gate ([`emoji_blob_key`]).
///
/// Remote-emoji bytes live in the evictable cache namespace, so a missing
/// blob whose ledger shell survives must enqueue a refetch (same miss path
/// as cached attachments) rather than answer a bare 404; the route owner
/// wires that (the attachment route's `recover_missing` is the pattern).
pub fn remote_emoji_blob_key(path: &str) -> Option<String> {
    if path.is_empty() || path.len() > 255 || !path.is_ascii() {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let ["remote", "emojis", domain, shortcode, file] = parts.as_slice() else {
        return None;
    };
    let style: &str = file.strip_suffix(".png")?;
    if !matches!(style, "original" | "static") {
        return None;
    }
    let (image_key, static_image_key) = remote_cache::remote_emoji_keys(domain, shortcode)?;
    let key: String = format!("remote/emojis/{domain}/{shortcode}/{style}.png");
    if key != image_key && key != static_image_key {
        return None;
    }
    if crate::media::blob_store::validate_key(&key).is_err() {
        return None;
    }
    Some(key)
}

/// An origin domain in a remote-emoji file name: dot/dash separated labels,
/// never empty, never leading/trailing punctuation.
fn is_emoji_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }
    if domain.starts_with(['.', '-']) || domain.ends_with(['.', '-']) {
        return false;
    }
    if domain.contains("..") {
        return false;
    }
    domain
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

fn parse_digits(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 20 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `{style}.{ext}` with a published style and a short alphanumeric extension.
fn is_published_file(file: &str) -> bool {
    let Some((style, ext)) = file.split_once('.') else {
        return false;
    };
    PUBLISHED_STYLES.contains(&style) && is_extension(ext)
}

/// `{name}.{ext}` of a legacy key.
fn is_plain_file_name(file: &str) -> bool {
    let Some((name, ext)) = file.rsplit_once('.') else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && is_extension(ext)
}

fn is_extension(ext: &str) -> bool {
    (1..=8).contains(&ext.len())
        && ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Outcome of evaluating a `Range` header against a representation length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// No (usable) range: answer `200` with the whole body.
    Full,
    /// Answer `206` with the inclusive byte span.
    Partial { start: u64, end: u64 },
    /// Answer `416`.
    Unsatisfiable,
}

/// Evaluate a `Range` header value for a body of `len` bytes.
///
/// Only a single `bytes=` range is honored; multi-range requests and
/// anything unparseable are answered with the full body, which RFC 9110
/// permits. A range that starts at or beyond the end is unsatisfiable.
pub fn evaluate_range(header: Option<&str>, len: u64) -> RangeOutcome {
    let Some(value) = header else {
        return RangeOutcome::Full;
    };
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeOutcome::Full;
    };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return RangeOutcome::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // Suffix range: the final `n` bytes.
        let Some(suffix) = parse_digits(last) else {
            return RangeOutcome::Full;
        };
        if suffix == 0 || len == 0 {
            return RangeOutcome::Unsatisfiable;
        }
        let start: u64 = len.saturating_sub(suffix);
        return RangeOutcome::Partial {
            start,
            end: len - 1,
        };
    }
    let Some(start) = parse_digits(first) else {
        return RangeOutcome::Full;
    };
    if start >= len {
        return RangeOutcome::Unsatisfiable;
    }
    let end: u64 = if last.is_empty() {
        len - 1
    } else {
        match parse_digits(last) {
            Some(end) if end >= start => end.min(len - 1),
            Some(_) | None => return RangeOutcome::Full,
        }
    };
    RangeOutcome::Partial { start, end }
}

/// How long a refetch request for one media row suppresses duplicates.
const REFETCH_SUPPRESS: Duration = Duration::from_secs(60);

/// Most rows remembered by the gate (older entries are evicted first).
const REFETCH_MEMORY: usize = 4096;

/// In-process single-flight gate for cache-miss refetches: many requests for
/// the same missing cached attachment enqueue one `FetchRemoteMedia` job per
/// suppression window instead of one per request.
#[derive(Debug, Default)]
pub struct RefetchGate {
    recent: Mutex<HashMap<u64, Instant>>,
}

impl RefetchGate {
    /// An empty gate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the caller should enqueue a refetch for `media_id` now.
    /// `true` at most once per suppression window.
    pub fn should_enqueue(&self, media_id: u64) -> bool {
        let Ok(mut recent) = self.recent.lock() else {
            return false;
        };
        let now: Instant = Instant::now();
        if recent.len() >= REFETCH_MEMORY {
            recent.retain(|_, at| now.duration_since(*at) < REFETCH_SUPPRESS);
            if recent.len() >= REFETCH_MEMORY {
                return false;
            }
        }
        match recent.get(&media_id) {
            Some(at) if now.duration_since(*at) < REFETCH_SUPPRESS => false,
            _ => {
                recent.insert(media_id, now);
                true
            }
        }
    }
}

/// Process-wide gate used by the public route.
pub fn refetch_gate() -> &'static RefetchGate {
    static GATE: std::sync::OnceLock<RefetchGate> = std::sync::OnceLock::new();
    GATE.get_or_init(RefetchGate::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_published_shapes_are_served() {
        for ok in [
            "ab/cd/0123456789abcdef/original.jpg",
            "ab/cd/0123456789abcdef/small.png",
            "ab/cd/0123456789abcdef/avatar.webp",
            "000/000/042/original/ab12cd.jpg",
        ] {
            assert_eq!(classify_key(ok), Some(PublicMediaKind::Local), "{ok}");
        }
        assert_eq!(
            classify_key("remote/42"),
            Some(PublicMediaKind::RemoteCache { media_id: 42 })
        );
        assert_eq!(
            classify_key("remote/42.small.png"),
            Some(PublicMediaKind::RemotePreview { media_id: 42 })
        );
        assert_eq!(classify_key("remote/42.derived.webm"), None);
        assert_eq!(classify_key("remote/42.small"), None);
        assert_eq!(
            sibling_key("remote/42", "small", "png").as_deref(),
            Some("remote/42.small.png")
        );
        assert_eq!(
            sibling_key("ab/cd/0123456789abcdef/original.png", "small", "png").as_deref(),
            Some("ab/cd/0123456789abcdef/small.png")
        );
        for bad in [
            "",
            "ab/cd/0123456789abcdef/derived.webm",
            "ab/cd/0123456789abcdef/.original.jpg.part-1-2",
            "ab/cd/0123456789abcdef/original.jpg.part",
            "tmp/123-4-abcd.part",
            "../etc/passwd",
            "ab/cd/0123456789abcdef/../../x",
            "ab\\cd\\0123456789abcdef\\original.jpg",
            "AB/cd/0123456789abcdef/original.jpg",
            "ab/cd/0123456789abcdef/original.JPG",
            "custom_emojis/images/6b/65/key/original/key.png",
            "remote/42/extra",
            "remote/abc",
            "remote/",
            "ab/cd/0123456789abcdef",
            "ab/cd/0123456789abcdef/original.jp\0g",
        ] {
            assert_eq!(classify_key(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn range_header_cases() {
        assert_eq!(evaluate_range(None, 100), RangeOutcome::Full);
        assert_eq!(
            evaluate_range(Some("bytes=0-9"), 100),
            RangeOutcome::Partial { start: 0, end: 9 }
        );
        assert_eq!(
            evaluate_range(Some("bytes=90-"), 100),
            RangeOutcome::Partial { start: 90, end: 99 }
        );
        assert_eq!(
            evaluate_range(Some("bytes=-10"), 100),
            RangeOutcome::Partial { start: 90, end: 99 }
        );
        assert_eq!(
            evaluate_range(Some("bytes=50-500"), 100),
            RangeOutcome::Partial { start: 50, end: 99 }
        );
        assert_eq!(
            evaluate_range(Some("bytes=100-"), 100),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            evaluate_range(Some("bytes=-0"), 100),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            evaluate_range(Some("bytes=0-1,5-6"), 100),
            RangeOutcome::Full
        );
        assert_eq!(evaluate_range(Some("items=0-1"), 100), RangeOutcome::Full);
        assert_eq!(evaluate_range(Some("bytes=9-3"), 100), RangeOutcome::Full);
        assert_eq!(
            evaluate_range(Some("bytes=-5"), 3),
            RangeOutcome::Partial { start: 0, end: 2 }
        );
    }

    #[test]
    fn only_published_remote_emoji_shapes_have_keys() {
        for ok in [
            "remote/emojis/remote.example/party/original.png",
            "remote/emojis/remote.example/party/static.png",
            "remote/emojis/a.b-c.example/p4rty_2/original.png",
        ] {
            assert_eq!(remote_emoji_blob_key(ok).as_deref(), Some(ok), "{ok}");
        }
        for bad in [
            "",
            "remote/emojis/remote.example/party/derived.png",
            "remote/emojis/remote.example/party/original.jpg",
            "remote/emojis/remote.example/party/original.png.part",
            "remote/emojis/REMOTE.EXAMPLE/party/original.png",
            "remote/emojis/remote.example/../party/original.png",
            "remote/emojis/remote..example/party/original.png",
            "remote/emojis/.lead/party/original.png",
            "remote/emojis/ok/x/original.png",
            "custom_emojis/images/70/61/party/original/party.png",
            "remote/42",
            "remote/emojis/remote.example/party",
            "remote/emojis/remote.example",
            "tmp/remote/emojis/remote.example/party/original.png",
            "remote/emojis/remote.example/party/.original.png.part-1-2",
        ] {
            assert_eq!(remote_emoji_blob_key(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn refetch_gate_admits_one_request_per_window() {
        let gate = RefetchGate::new();
        assert!(gate.should_enqueue(7));
        assert!(!gate.should_enqueue(7));
        assert!(gate.should_enqueue(8));
    }

    #[test]
    fn only_published_emoji_shapes_have_keys() {
        // `party` = 0x70 0x61 → partition `70/61`.
        for ok in [
            "70/61/party/original/party.png",
            "70/61/party/static/party.png",
            "70/61/party/original/party@remote.example.png",
            "70/61/party/static/party@remote.example.png",
        ] {
            assert_eq!(
                emoji_blob_key(ok).as_deref(),
                Some(format!("custom_emojis/images/{ok}").as_str()),
                "{ok}",
            );
        }
        for bad in [
            "",
            "70/61/party/original/party.jpg",
            "70/61/party/original/party.png.part",
            "70/61/party/original/.party.png.part-1-2",
            "70/61/party/derived/party.png",
            "70/61/party/original/other.png",
            "70/61/other/original/party.png",
            "00/00/party/original/party.png",
            "70/61/party/original/party@.png",
            "70/61/party/original/party@bad..example.png",
            "70/61/party/original/party@evil@example.png",
            "70/61/party/original/party",
            "70/61/party/original",
            "70/61/party",
            "tmp/1-2-3.part",
            "../outside.png",
            "70/61/party/../../../outside.png",
            "70\\61\\party\\original\\party.png",
            "70/61/party/original/party.png/",
            "70/61/party//party.png",
        ] {
            assert_eq!(emoji_blob_key(bad), None, "{bad:?}");
        }
    }
}

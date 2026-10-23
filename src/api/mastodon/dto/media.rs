//! Mastodon media DTO packers (S10 split of `api::mastodon::dto`).
//!
//! `media_file_url` is the shared asset-path helper: `account.rs` imports it
//! for avatar/header URLs, and `status.rs` reaches the attachment packers
//! through the functions below.

use serde_json::{Map, Value, json};

use crate::api::mastodon::ServerInfo;
use crate::application::license_policy;
use crate::domain::{MediaAttachment, MediaState};

/// Local file path of an attachment blob key under the media route.
pub fn media_file_url(server: &ServerInfo, file_key: &str) -> String {
    server.asset_url(&format!("system/media_attachments/files/{file_key}"))
}

/// `REST::MediaAttachmentSerializer`.
///
/// `url`/`preview_url` are `null` while the attachment is not processed
/// (queued, in progress or failed — Mastodon's `not_processed?`), matching
/// the wire contract exactly; the URLs point at this server's file route
/// (`/system/media_attachments/files/...`, Mastodon's storage path shape).
/// `remote_url`/`preview_remote_url`/`text_url` stay `null` for local media
/// (Mastodon assigns no `shortcode` to modern uploads, so `text_url` is
/// `null` there too).
///
/// Phase 7.1 packer rules: a transcoded attachment serves its *derived* blob
/// (the derived bytes replace the original at the attachment's own
/// `file_key`, so `url` needs no indirection); pending rows keep `url: null`
/// (contract-correct — a partial derived blob is never surfaced); GIF rows
/// that transcoded report `type: video` with `meta.original` dimensions read
/// from the derived WebM (`duration` only when the probe knew it, otherwise
/// honestly absent); converted stills carry derived dimensions, a WebP
/// `small` preview and a blurhash hashed from the derived pixels.
/// `REST::MediaAttachmentSerializer` for local media (no remote source).
pub fn media_attachment_json(server: &ServerInfo, media: &MediaAttachment) -> Value {
    media_attachment_json_with_source(server, media, None)
}

/// `REST::MediaAttachmentSerializer` with the stored remote origin URL.
///
/// `remote_source` is `Some` exactly for remote attachments (threaded from
/// the batch-loaded remote-media sources): both `remote_url` and
/// `preview_remote_url` render the origin URL even while the cached bytes
/// are still pending or have failed (Mastodon's serializer reads
/// `object.remote_url` immediately; only `url` waits on `not_processed?`).
/// Local media passes `None` and keeps both keys `null`.
pub fn media_attachment_json_with_source(
    server: &ServerInfo,
    media: &MediaAttachment,
    remote_source: Option<&str>,
) -> Value {
    let processed = media.state == MediaState::Ready && !media.file_key.is_empty();
    let url = if processed {
        Value::String(media_file_url(server, &media.file_key))
    } else {
        Value::Null
    };
    let preview_url = match &media.preview_key {
        Some(key) if processed => Value::String(media_file_url(server, key)),
        _ => Value::Null,
    };
    let remote_url = match remote_source.filter(|source| !source.is_empty()) {
        Some(source) => Value::String(source.to_string()),
        None => Value::Null,
    };
    let mut value = json!({
        "id": media.id.to_string(),
        "type": media.r#type,
        "url": url,
        "preview_url": preview_url,
        "remote_url": remote_url.clone(),
        "preview_remote_url": remote_url,
        "text_url": Value::Null,
        "meta": media_meta_json(media),
        "description": if media.description.is_empty() { Value::Null } else { json!(media.description) },
        "blurhash": media.blurhash.clone().map(Value::String).unwrap_or(Value::Null),
    });
    // Core extension CORE-L: `license` renders only when the policy is
    // enabled (`off` omits the key); enabled + `None` renders `null`
    // (all rights reserved).
    if let Some(license) = license_policy::license_wire_value(&server.license_policy, media.license)
    {
        if let Some(object) = value.as_object_mut() {
            object.insert("license".into(), license);
        }
    }
    value
}

/// The `meta` object: `original` (and `small` when a preview exists) plus
/// `focus`, mirroring Mastodon's `MediaAttachment#populate_meta` shape.
fn media_meta_json(media: &MediaAttachment) -> Value {
    let mut original = Map::new();
    if let (Some(width), Some(height)) = (media.width, media.height) {
        original.insert("width".into(), json!(width));
        original.insert("height".into(), json!(height));
        original.insert("size".into(), json!(format!("{width}x{height}")));
        if height > 0 {
            original.insert("aspect".into(), json!(width as f64 / height as f64));
        }
    }
    if let Some(duration_ms) = media.duration_ms {
        original.insert("duration".into(), json!(duration_ms as f64 / 1_000.0));
    }
    if let Some(frame_rate) = &media.frame_rate {
        original.insert("frame_rate".into(), json!(frame_rate));
    }
    if let Some(bitrate) = media.bitrate {
        original.insert("bitrate".into(), json!(bitrate));
    }
    if original.is_empty() {
        return Value::Null;
    }
    let mut meta = Map::new();
    meta.insert("original".into(), Value::Object(original));
    if media.preview_key.is_some()
        && let (Some(width), Some(height)) = (media.preview_width, media.preview_height)
    {
        let mut small = Map::new();
        small.insert("width".into(), json!(width));
        small.insert("height".into(), json!(height));
        small.insert("size".into(), json!(format!("{width}x{height}")));
        if height > 0 {
            small.insert("aspect".into(), json!(width as f64 / height as f64));
        }
        meta.insert("small".into(), Value::Object(small));
    }
    if let Some(focus) = media.focus {
        meta.insert("focus".into(), json!({ "x": focus.x, "y": focus.y }));
    }
    Value::Object(meta)
}

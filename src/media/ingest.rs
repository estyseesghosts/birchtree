//! Content sniffing for uploads (magic bytes).
//!
//! The declared multipart content type is never trusted: the byte prefix
//! decides what an upload actually is, and the upload use case rejects
//! anything outside Mastodon's accepted image/video/audio MIME lists with
//! the pinned 422 message.

/// A sniffed upload classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sniffed {
    /// Detected MIME type (from content, not the request).
    pub mime_type: &'static str,
    /// Mastodon wire `type` (`image`/`video`/`audio`).
    pub r#type: &'static str,
    /// Canonical file extension.
    pub extension: &'static str,
}

fn starts_with(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && &haystack[..needle.len()] == needle
}

/// Classify an upload by its leading bytes.
///
/// Mirrors the Mastodon 4.7.3 accepted lists: images (jpeg/png/gif/webp),
/// video (mp4/webm/quicktime) and audio (ogg/mp3/wav/flac/m4a). Anything
/// else returns `None` (the use case answers the content-type 422).
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    if bytes.len() < 12 {
        return None;
    }
    // Images.
    if starts_with(bytes, &[0xFF, 0xD8, 0xFF]) {
        return Some(Sniffed {
            mime_type: "image/jpeg",
            r#type: "image",
            extension: "jpg",
        });
    }
    if starts_with(bytes, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(Sniffed {
            mime_type: "image/png",
            r#type: "image",
            extension: "png",
        });
    }
    if starts_with(bytes, b"GIF87a") || starts_with(bytes, b"GIF89a") {
        return Some(Sniffed {
            mime_type: "image/gif",
            r#type: "image",
            extension: "gif",
        });
    }
    if starts_with(bytes, b"RIFF") && starts_with(&bytes[8..], b"WEBP") {
        return Some(Sniffed {
            mime_type: "image/webp",
            r#type: "image",
            extension: "webp",
        });
    }
    if starts_with(bytes, b"BM") {
        return Some(Sniffed {
            mime_type: "image/bmp",
            r#type: "image",
            extension: "bmp",
        });
    }
    // ISO-BMFF containers: mp4/m4v/quicktime/m4a (video vs audio by major
    // brand, matching how clients treat the extensions).
    if starts_with(&bytes[4..], b"ftyp") {
        let brand = &bytes[8..12];
        if brand == b"M4A " || brand == b"M4B " {
            return Some(Sniffed {
                mime_type: "audio/mp4",
                r#type: "audio",
                extension: "m4a",
            });
        }
        return Some(Sniffed {
            mime_type: "video/mp4",
            r#type: "video",
            extension: "mp4",
        });
    }
    // Matroska/WebM (EBML header).
    if starts_with(bytes, &[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Sniffed {
            mime_type: "video/webm",
            r#type: "video",
            extension: "webm",
        });
    }
    // Ogg (audio; Theora video is rare in client uploads).
    if starts_with(bytes, b"OggS") {
        return Some(Sniffed {
            mime_type: "audio/ogg",
            r#type: "audio",
            extension: "ogg",
        });
    }
    // WAV.
    if starts_with(bytes, b"RIFF") && starts_with(&bytes[8..], b"WAVE") {
        return Some(Sniffed {
            mime_type: "audio/wav",
            r#type: "audio",
            extension: "wav",
        });
    }
    // FLAC.
    if starts_with(bytes, b"fLaC") {
        return Some(Sniffed {
            mime_type: "audio/flac",
            r#type: "audio",
            extension: "flac",
        });
    }
    // MP3: ID3 tag or a bare MPEG frame sync.
    if starts_with(bytes, b"ID3") {
        return Some(Sniffed {
            mime_type: "audio/mpeg",
            r#type: "audio",
            extension: "mp3",
        });
    }
    if bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0 {
        return Some(Sniffed {
            mime_type: "audio/mpeg",
            r#type: "audio",
            extension: "mp3",
        });
    }
    None
}

/// Custom-emoji images are restricted to PNG/GIF/WebP (Mastodon's
/// `CustomEmoji` content types), sniffed from content.
pub fn is_emoji_image(bytes: &[u8]) -> bool {
    matches!(
        sniff(bytes).map(|s| s.mime_type),
        Some("image/png") | Some("image/gif") | Some("image/webp")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&[0; 8]);
        bytes
    }

    #[test]
    fn classifies_common_formats() {
        assert_eq!(sniff(&png()).unwrap().mime_type, "image/png");
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.extend_from_slice(&[0; 8]);
        assert_eq!(sniff(&jpeg).unwrap().mime_type, "image/jpeg");
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0; 8]);
        assert_eq!(sniff(&gif).unwrap().mime_type, "image/gif");
        let mut webp = b"RIFF\x00\x00\x00\x00WEBP".to_vec();
        webp.extend_from_slice(&[0; 4]);
        assert_eq!(sniff(&webp).unwrap().mime_type, "image/webp");
        let mut mp4 = vec![0; 4];
        mp4.extend_from_slice(b"ftypisom");
        mp4.extend_from_slice(&[0; 8]);
        assert_eq!(sniff(&mp4).unwrap().r#type, "video");
        let mut m4a = vec![0; 4];
        m4a.extend_from_slice(b"ftypM4A ");
        m4a.extend_from_slice(&[0; 8]);
        assert_eq!(sniff(&m4a).unwrap().r#type, "audio");
        assert_eq!(
            sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0, 0, 0, 0, 0])
                .unwrap()
                .mime_type,
            "video/webm"
        );
    }

    #[test]
    fn rejects_unknown_and_short_input() {
        assert!(sniff(b"plain text not an upload at all").is_none());
        assert!(sniff(b"tiny").is_none());
        assert!(!is_emoji_image(b"plain text not an upload at all"));
        assert!(is_emoji_image(&png()));
    }
}

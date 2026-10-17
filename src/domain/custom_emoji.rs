//! Custom emoji.
//!
//! Mirrors Mastodon's `CustomEmoji` for the public index endpoint
//! (`GET /api/v1/custom_emojis` lists local, enabled, picker-visible emoji)
//! and for the `emojis` arrays embedded in status/account payloads. Emoji are
//! managed by CLI (no admin API in the public-client milestone).

use serde::{Deserialize, Serialize};

/// One custom emoji.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomEmoji {
    /// shortcode without colons (`party_parrot`), 2–128 chars.
    pub shortcode: String,
    /// Blob key of the animated/original image.
    pub image_key: String,
    /// Blob key of the static image.
    pub static_image_key: String,
    #[serde(default = "default_true")]
    pub visible_in_picker: bool,
    #[serde(default)]
    pub disabled: bool,
    /// Category name shown in pickers (`None` = uncategorized).
    #[serde(default)]
    pub category: Option<String>,
}

fn default_true() -> bool {
    true
}

impl CustomEmoji {
    /// Mastodon `CustomEmoji::SHORTCODE_RE_FRAGMENT` bounds.
    pub const MIN_SHORTCODE_SIZE: usize = 2;
    pub const MAX_SHORTCODE_SIZE: usize = 128;
    /// Mastodon `CustomEmoji::LIMIT` (256 KB per image).
    pub const IMAGE_LIMIT: u64 = 256 * 1024;

    /// True when the emoji belongs in the public index (Mastodon
    /// `CustomEmoji.listed` = local + enabled + visible_in_picker).
    pub fn listed(&self) -> bool {
        !self.disabled && self.visible_in_picker
    }

    /// Validate a shortcode (Mastodon: `[a-zA-Z0-9_]{2,}` within length).
    pub fn shortcode_valid(shortcode: &str) -> bool {
        let len = shortcode.chars().count();
        (Self::MIN_SHORTCODE_SIZE..=Self::MAX_SHORTCODE_SIZE).contains(&len)
            && !shortcode.is_empty()
            && shortcode
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
}

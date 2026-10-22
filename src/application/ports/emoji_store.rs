use crate::domain::{CustomEmoji, Result};

pub trait EmojiStore: Send + Sync {
    /// Create or replace an emoji by shortcode (CLI-managed).
    fn upsert_emoji(&self, emoji: CustomEmoji) -> Result<CustomEmoji>;
    fn emoji_by_shortcode(&self, shortcode: &str) -> Result<Option<CustomEmoji>>;
    fn delete_emoji(&self, shortcode: &str) -> Result<bool>;
    /// Listed emoji (index endpoint; local + enabled + picker-visible),
    /// shortcode order, bounded.
    fn listed_emojis(&self, cap: usize) -> Result<Vec<CustomEmoji>>;
    /// Every emoji row (operator listing; shortcode order, bounded).
    fn all_emojis(&self, cap: usize) -> Result<Vec<CustomEmoji>>;
    /// Emoji whose shortcode is in `shortcodes` (status/account payloads).
    fn emojis_by_shortcodes(&self, shortcodes: &[String]) -> Result<Vec<CustomEmoji>>;
    /// Upsert one remote emoji row (federation tail, G-D ingest).
    fn upsert_remote_emoji(&self, row: RemoteEmojiRow) -> Result<RemoteEmojiRow>;
    /// Remote emoji rows by shortcode (federation tail, G-D): backs the
    /// `emojis` arrays for shortcodes with no local row. Empty when none.
    fn remote_emojis_by_shortcodes(&self, shortcodes: &[String]) -> Result<Vec<RemoteEmojiRow>>;
}

/// One remote custom emoji row (federation tail, G-D): the origin domain +
/// shortcode key the render path, with the local asset path served in
/// `emojis` arrays (never picker-visible, never listed).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteEmojiRow {
    pub shortcode: String,
    pub domain: String,
    pub url: String,
    /// Origin image URL the bytes were fetched from. The serve path uses it
    /// to enqueue a `FetchRemoteEmoji` refetch on a byte miss (evicted bytes
    /// heal; they never 500). Defaults for rows written before the field
    /// existed (which never refetch).
    #[serde(default)]
    pub source_url: String,
}

//! Custom emoji use cases (CLI-managed) and the public index.
//!
//! Mirrors Mastodon's `Api::V1::CustomEmojisController` (public, listed
//! scope = local + enabled + picker-visible) and the CLI-managed admin path
//! (no admin API in the public-client milestone).

use crate::application::ports::{BlobStore, Store};
use crate::domain::{CustomEmoji, Error, Result};

/// Public index (`GET /api/v1/custom_emojis`): listed emoji in shortcode
/// order.
pub fn index(store: &dyn Store) -> Result<Vec<CustomEmoji>> {
    store.listed_emojis(1_000)
}

/// CLI: register (or replace) a local emoji from staged image bytes.
///
/// The image is staged first, then the row commits — file storage cannot
/// join the `redb` transaction (`birchtree.md` §5).
pub fn add_emoji(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    shortcode: &str,
    image: &[u8],
    category: Option<&str>,
    visible_in_picker: bool,
) -> Result<CustomEmoji> {
    if !CustomEmoji::shortcode_valid(shortcode) {
        return Err(Error::InvalidInput(
            "shortcode must be 2-128 characters of letters, digits and underscores".into(),
        ));
    }
    if image.len() as u64 > CustomEmoji::IMAGE_LIMIT {
        return Err(Error::InvalidInput(
            "Image must be less than 256 KB".into(),
        ));
    }
    // Reject anything that is not a PNG/GIF/WebP image (Mastodon's content
    // type list) by magic bytes.
    if !crate::media::ingest::is_emoji_image(image) {
        return Err(Error::InvalidInput(
            "File content type is invalid, File is invalid".into(),
        ));
    }
    let partition = emoji_partition(shortcode);
    let image_key = format!("custom_emojis/images/{partition}/original/{shortcode}.png");
    let static_key = format!("custom_emojis/images/{partition}/static/{shortcode}.png");
    blobs.put(&image_key, image)?;
    blobs.put(&static_key, image)?;
    let emoji = CustomEmoji {
        shortcode: shortcode.to_string(),
        image_key,
        static_image_key: static_key,
        visible_in_picker,
        disabled: false,
        category: category.map(str::to_string).filter(|c| !c.is_empty()),
    };
    store.upsert_emoji(emoji)
}

/// CLI: remove an emoji and its bytes.
pub fn rm_emoji(store: &dyn Store, blobs: &dyn BlobStore, shortcode: &str) -> Result<bool> {
    let Some(emoji) = store.emoji_by_shortcode(shortcode)? else {
        return Ok(false);
    };
    let removed = store.delete_emoji(shortcode)?;
    blobs.delete(&emoji.image_key)?;
    blobs.delete(&emoji.static_image_key)?;
    Ok(removed)
}

/// Stable blob partition for an emoji (`ab/cd/shortcode`).
pub fn emoji_partition(shortcode: &str) -> String {
    let bytes = shortcode.as_bytes();
    let a = bytes.first().map(|b| *b).unwrap_or(b'_');
    let b = bytes.get(1).map(|b| *b).unwrap_or(b'_');
    format!("{a:02x}/{b:02x}/{shortcode}")
}

/// Patch for [`set_emoji_flags`]: `None` leaves the field unchanged, while
/// `category`'s inner `None` clears the category.
#[derive(Debug, Default)]
pub struct EmojiFlags {
    pub category: Option<Option<String>>,
    pub visible_in_picker: Option<bool>,
    pub disabled: Option<bool>,
}

/// Admin: update an emoji's category, picker visibility, and disabled flag
/// (upstream HTML-admin batch enable/disable/list/unlist).
///
/// Unknown shortcodes are `NotFound` (the endpoint maps this to 404);
/// empty-string categories normalize to uncategorized, matching
/// [`add_emoji`].
pub fn set_emoji_flags(
    store: &dyn Store,
    shortcode: &str,
    patch: EmojiFlags,
) -> Result<CustomEmoji> {
    let Some(mut emoji) = store.emoji_by_shortcode(shortcode)? else {
        return Err(Error::NotFound(format!(
            "No emoji with shortcode {shortcode}"
        )));
    };
    if let Some(category) = patch.category {
        emoji.category = category.filter(|c| !c.is_empty());
    }
    if let Some(visible) = patch.visible_in_picker {
        emoji.visible_in_picker = visible;
    }
    if let Some(disabled) = patch.disabled {
        emoji.disabled = disabled;
    }
    store.upsert_emoji(emoji)
}

/// Admin: update an emoji's category (`None` clears it).
pub fn set_emoji_category(
    store: &dyn Store,
    shortcode: &str,
    category: Option<&str>,
) -> Result<CustomEmoji> {
    set_emoji_flags(
        store,
        shortcode,
        EmojiFlags {
            category: Some(category.map(str::to_string)),
            ..EmojiFlags::default()
        },
    )
}

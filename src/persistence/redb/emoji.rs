//! Custom emoji persistence (CLI-managed; listed rows feed the public index
//! and the `emojis` arrays of status/account payloads).

use crate::application::ports::EmojiStore;
use crate::domain::{CustomEmoji, Result};

use redb::ReadableTable;

use super::schema::CUSTOM_EMOJI;
use super::{decode, encode, RedbStore};

/// Scan cap for the public emoji index (Mastodon instances hold hundreds at
/// most; the cap bounds a pathological table).
const LISTED_CAP: usize = 1_000;

impl EmojiStore for RedbStore {
    fn upsert_emoji(&self, emoji: CustomEmoji) -> Result<CustomEmoji> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(CUSTOM_EMOJI)?;
            table.insert(emoji.shortcode.as_str(), encode(&emoji)?.as_slice())?;
        }
        write.commit()?;
        Ok(emoji)
    }

    fn emoji_by_shortcode(&self, shortcode: &str) -> Result<Option<CustomEmoji>> {
        let read = self.begin_read()?;
        let table = read.open_table(CUSTOM_EMOJI)?;
        match table.get(shortcode)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn delete_emoji(&self, shortcode: &str) -> Result<bool> {
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(CUSTOM_EMOJI)?;
            table.remove(shortcode)?.is_some()
        };
        write.commit()?;
        Ok(removed)
    }

    fn listed_emojis(&self, cap: usize) -> Result<Vec<CustomEmoji>> {
        let read = self.begin_read()?;
        let table = read.open_table(CUSTOM_EMOJI)?;
        let cap = cap.min(LISTED_CAP);
        let mut out = Vec::new();
        for entry in table.range(""..)? {
            let (_key, value) = entry?;
            let emoji: CustomEmoji = decode(value.value())?;
            if emoji.listed() {
                out.push(emoji);
            }
            if out.len() >= cap {
                break;
            }
        }
        self.note_scanned(out.len());
        Ok(out)
    }

    fn emojis_by_shortcodes(&self, shortcodes: &[String]) -> Result<Vec<CustomEmoji>> {
        if shortcodes.is_empty() {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let table = read.open_table(CUSTOM_EMOJI)?;
        let mut out = Vec::new();
        for shortcode in shortcodes {
            if let Some(guard) = table.get(shortcode.as_str())? {
                out.push(decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn upsert_remote_emoji(
        &self,
        row: crate::application::ports::RemoteEmojiRow,
    ) -> Result<crate::application::ports::RemoteEmojiRow> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(super::schema::REMOTE_EMOJI)?;
            table.insert(
                super::schema::remote_emoji_key(&row.domain, &row.shortcode).as_slice(),
                encode(&row)?.as_slice(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn remote_emojis_by_shortcodes(
        &self,
        shortcodes: &[String],
    ) -> Result<Vec<crate::application::ports::RemoteEmojiRow>> {
        use crate::application::ports::RemoteEmojiRow;
        if shortcodes.is_empty() {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let table = read.open_table(super::schema::REMOTE_EMOJI)?;
        // Rows are keyed `(domain, shortcode)`; the render path knows only
        // the shortcode, so scan bounded and match on the suffix. The table
        // holds hundreds of rows at most (one per seen remote emoji).
        let mut out = Vec::new();
        let mut scanned = 0usize;
        for entry in table.iter()? {
            let (key, value) = entry?;
            scanned += 1;
            if scanned > 5_000 {
                break;
            }
            let row: RemoteEmojiRow = decode(value.value())?;
            if shortcodes.iter().any(|s| s == &row.shortcode) {
                // Confirm the key suffix matches (never serve a row whose
                // key disagrees with its value).
                let bytes = key.value();
                if bytes
                    .split(|b| *b == 0xFF)
                    .nth(1)
                    .is_some_and(|suffix| suffix == row.shortcode.as_bytes())
                {
                    out.push(row);
                }
            }
            if out.len() >= shortcodes.len() {
                break;
            }
        }
        Ok(out)
    }
}

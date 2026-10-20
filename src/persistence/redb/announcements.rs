//! Announcement persistence (fix round P1/S2).
//!
//! Durable counterpart to the transitional `MemoryAnnouncements`: every
//! mutating method commits the record + index + mute/reaction rows in ONE
//! `redb` transaction. Announcements carry no fan-out here, so no jobs attach
//! to these writes. Scheduled publishing is manual via the admin
//! publish/unpublish surface (no `PublishScheduledAnnouncement` job yet — a
//! future-dated `scheduled_at_ms` stays a draft until published).

use redb::ReadableTable;

use crate::application::announcements::{
    AnnouncementReaction, AnnouncementRecord, AnnouncementStore,
};
use crate::domain::{Id, Result};

use super::schema::{
    self, ANNOUNCEMENTS, ANNOUNCEMENT_MUTES, ANNOUNCEMENT_REACTIONS,
    ANNOUNCEMENT_REACTIONS_BY_ANNOUNCEMENT, ANNOUNCEMENT_REACTIONS_BY_VIEWER,
};
use super::{decode, encode, RedbStore};

/// 24-byte `(announcement_id_be || created_at_be || reaction_id_be)` key.
fn by_announcement_key(announcement_id: u64, created_at_ms: u64, reaction_id: u64) -> [u8; 24] {
    let mut key = [0u8; 24];
    key[..8].copy_from_slice(&announcement_id.to_be_bytes());
    key[8..16].copy_from_slice(&created_at_ms.to_be_bytes());
    key[16..24].copy_from_slice(&reaction_id.to_be_bytes());
    key
}

/// `(announcement_id_be || account_id_be || name)` key.
fn by_viewer_key(announcement_id: u64, account_id: u64, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + name.len());
    key.extend_from_slice(&announcement_id.to_be_bytes());
    key.extend_from_slice(&account_id.to_be_bytes());
    key.extend_from_slice(name.as_bytes());
    key
}

impl AnnouncementStore for RedbStore {
    fn announcement_by_id(&self, id: Id) -> Result<Option<AnnouncementRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(ANNOUNCEMENTS)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn all_announcements(&self, cap: usize) -> Result<Vec<AnnouncementRecord>> {
        let read = self.begin_read()?;
        let table = read.open_table(ANNOUNCEMENTS)?;
        let mut out = Vec::new();
        for entry in table.range(0u64..=u64::MAX)? {
            let (_, value) = entry?;
            out.push(decode(value.value())?);
            if out.len() >= cap {
                break;
            }
        }
        Ok(out)
    }

    fn insert_announcement(&self, record: AnnouncementRecord) -> Result<AnnouncementRecord> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(ANNOUNCEMENTS)?;
            table.insert(record.id.value(), encode(&record)?.as_slice())?;
        }
        write.commit()?;
        Ok(record)
    }

    fn update_announcement(&self, record: AnnouncementRecord) -> Result<AnnouncementRecord> {
        self.insert_announcement(record)
    }

    fn delete_announcement(&self, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let existed = {
            let mut rows = write.open_table(ANNOUNCEMENTS)?;
            rows.remove(id.value())?.is_some()
        };
        if existed {
            // Cascade: mutes, then every reaction + its index rows.
            let mut prefix = [0u8; 8];
            prefix.copy_from_slice(&id.value().to_be_bytes());
            let mute_keys: Vec<Vec<u8>> = {
                let mutes = write.open_table(ANNOUNCEMENT_MUTES)?;
                mutes
                    .range(prefix.as_slice()..)?
                    .take_while(|entry| {
                        entry.as_ref().is_ok_and(|(key, _)| {
                            let bytes = key.value();
                            bytes.len() == 16 && &bytes[..8] == prefix.as_slice()
                        })
                    })
                    .filter_map(|entry| entry.ok().map(|(key, _)| key.value().to_vec()))
                    .collect()
            };
            {
                let mut mutes = write.open_table(ANNOUNCEMENT_MUTES)?;
                for key in mute_keys {
                    mutes.remove(key.as_slice())?;
                }
            }
            let reactions: Vec<AnnouncementReaction> = {
                let by_ann = write.open_table(ANNOUNCEMENT_REACTIONS_BY_ANNOUNCEMENT)?;
                let mut out = Vec::new();
                for entry in by_ann.range(prefix.as_slice()..)? {
                    let (key, _) = entry?;
                    let bytes = key.value();
                    if bytes.len() != 24 || &bytes[..8] != prefix.as_slice() {
                        break;
                    }
                    let mut raw = [0u8; 8];
                    raw.copy_from_slice(&bytes[16..24]);
                    let reaction_id = u64::from_be_bytes(raw);
                    let rows = write.open_table(ANNOUNCEMENT_REACTIONS)?;
                    if let Some(guard) = rows.get(reaction_id)? {
                        out.push(decode(guard.value())?);
                    }
                }
                out
            };
            {
                let mut rows = write.open_table(ANNOUNCEMENT_REACTIONS)?;
                let mut by_ann = write.open_table(ANNOUNCEMENT_REACTIONS_BY_ANNOUNCEMENT)?;
                let mut by_viewer = write.open_table(ANNOUNCEMENT_REACTIONS_BY_VIEWER)?;
                for reaction in &reactions {
                    rows.remove(reaction.id.value())?;
                    by_ann.remove(
                        by_announcement_key(
                            reaction.announcement_id.value(),
                            reaction.created_at_ms,
                            reaction.id.value(),
                        )
                        .as_slice(),
                    )?;
                    by_viewer.remove(
                        by_viewer_key(
                            reaction.announcement_id.value(),
                            reaction.account_id.value(),
                            &reaction.name,
                        )
                        .as_slice(),
                    )?;
                }
            }
        }
        write.commit()?;
        Ok(existed)
    }

    fn mute_announcement(&self, announcement_id: Id, account_id: Id, _now_ms: u64) -> Result<()> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(ANNOUNCEMENT_MUTES)?;
            let key = schema::pair_key(announcement_id.value(), account_id.value());
            table.insert(key.as_slice(), ())?;
        }
        write.commit()?;
        Ok(())
    }

    fn announcement_muted(&self, announcement_id: Id, account_id: Id) -> Result<bool> {
        let read = self.begin_read()?;
        let table = read.open_table(ANNOUNCEMENT_MUTES)?;
        let key = schema::pair_key(announcement_id.value(), account_id.value());
        Ok(table.get(key.as_slice())?.is_some())
    }

    fn insert_reaction(&self, reaction: AnnouncementReaction) -> Result<AnnouncementReaction> {
        let write = self.begin_write()?;
        {
            let mut rows = write.open_table(ANNOUNCEMENT_REACTIONS)?;
            rows.insert(reaction.id.value(), encode(&reaction)?.as_slice())?;
        }
        {
            let mut by_ann = write.open_table(ANNOUNCEMENT_REACTIONS_BY_ANNOUNCEMENT)?;
            by_ann.insert(
                by_announcement_key(
                    reaction.announcement_id.value(),
                    reaction.created_at_ms,
                    reaction.id.value(),
                )
                .as_slice(),
                (),
            )?;
        }
        {
            let mut by_viewer = write.open_table(ANNOUNCEMENT_REACTIONS_BY_VIEWER)?;
            by_viewer.insert(
                by_viewer_key(
                    reaction.announcement_id.value(),
                    reaction.account_id.value(),
                    &reaction.name,
                )
                .as_slice(),
                reaction.id.value(),
            )?;
        }
        write.commit()?;
        Ok(reaction)
    }

    fn viewer_reaction(
        &self,
        announcement_id: Id,
        account_id: Id,
        name: &str,
    ) -> Result<Option<AnnouncementReaction>> {
        let key = by_viewer_key(announcement_id.value(), account_id.value(), name);
        let read = self.begin_read()?;
        let by_viewer = read.open_table(ANNOUNCEMENT_REACTIONS_BY_VIEWER)?;
        match by_viewer.get(key.as_slice())? {
            Some(guard) => {
                let reaction_id = guard.value();
                let rows = read.open_table(ANNOUNCEMENT_REACTIONS)?;
                match rows.get(reaction_id)? {
                    Some(row) => Ok(Some(decode(row.value())?)),
                    None => Ok(None),
                }
            }
            None => Ok(None),
        }
    }

    fn reactions_of(&self, announcement_id: Id) -> Result<Vec<AnnouncementReaction>> {
        let mut prefix = [0u8; 8];
        prefix.copy_from_slice(&announcement_id.value().to_be_bytes());
        let read = self.begin_read()?;
        let by_ann = read.open_table(ANNOUNCEMENT_REACTIONS_BY_ANNOUNCEMENT)?;
        let rows = read.open_table(ANNOUNCEMENT_REACTIONS)?;
        let mut out = Vec::new();
        for entry in by_ann.range(prefix.as_slice()..)? {
            let (key, _) = entry?;
            let bytes = key.value();
            if bytes.len() != 24 || &bytes[..8] != prefix.as_slice() {
                break;
            }
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[16..24]);
            let reaction_id = u64::from_be_bytes(raw);
            if let Some(guard) = rows.get(reaction_id)? {
                out.push(decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn delete_reaction(&self, id: Id) -> Result<bool> {
        let write = self.begin_write()?;
        let reaction: Option<AnnouncementReaction> = {
            let mut rows = write.open_table(ANNOUNCEMENT_REACTIONS)?;
            match rows.remove(id.value())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        let Some(reaction) = reaction else {
            return Ok(false);
        };
        {
            let mut by_ann = write.open_table(ANNOUNCEMENT_REACTIONS_BY_ANNOUNCEMENT)?;
            by_ann.remove(
                by_announcement_key(
                    reaction.announcement_id.value(),
                    reaction.created_at_ms,
                    reaction.id.value(),
                )
                .as_slice(),
            )?;
        }
        {
            let mut by_viewer = write.open_table(ANNOUNCEMENT_REACTIONS_BY_VIEWER)?;
            by_viewer.remove(
                by_viewer_key(
                    reaction.announcement_id.value(),
                    reaction.account_id.value(),
                    &reaction.name,
                )
                .as_slice(),
            )?;
        }
        write.commit()?;
        Ok(true)
    }
}

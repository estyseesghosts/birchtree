//! Media attachment metadata persistence.
//!
//! Attachment bytes and the processing lifecycle are Phase 4; this module
//! owns the metadata rows written by the upload step plus the ownership /
//! attachability checks that run *inside* a status create or edit
//! transaction, so a status can never reference a missing or foreign
//! attachment.

use redb::{ReadableDatabase, ReadableTable, WriteTransaction};

use crate::application::ports::{MediaStore, NewMedia};
use crate::domain::{Error, Id, MediaAttachment, MediaState, Result};

use super::schema::{self, MEDIA, MEDIA_BY_OWNER};
use super::{decode, encode, RedbStore, StoreResult};

/// Read a media row inside an open write transaction.
///
/// Shared with the statuses module so attachment bookkeeping happens in the
/// same transaction as the status it belongs to.
pub(super) fn read_media_txn(
    write: &WriteTransaction,
    id: Id,
) -> StoreResult<Option<MediaAttachment>> {
    let table = write.open_table(MEDIA)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Write (upsert) a media row inside an open write transaction.
pub(super) fn write_media_txn(write: &WriteTransaction, media: &MediaAttachment) -> StoreResult<()> {
    let mut table = write.open_table(MEDIA)?;
    table.insert(media.id.value(), encode(media)?.as_slice())?;
    Ok(())
}

/// Validate that every id in `ids` exists, is owned by `owner_id`, and is
/// free to attach: unattached, or already attached to `attach_to` (the status
/// being written — `None` while creating, so the row must be unattached).
///
/// Returns the domain `InvalidInput` error the create/update paths surface,
/// aborting their whole transaction.
pub(super) fn check_attachable_txn(
    write: &WriteTransaction,
    owner_id: Id,
    ids: &[Id],
    attach_to: Option<Id>,
) -> Result<()> {
    for id in ids {
        let attachable = match read_media_txn(write, *id)? {
            Some(media) => {
                media.account_id == owner_id
                    && (media.status_id.is_none() || media.status_id == attach_to)
            }
            None => false,
        };
        if !attachable {
            return Err(Error::InvalidInput(format!(
                "Media {id} not found or already attached to another post"
            )));
        }
    }
    Ok(())
}

impl MediaStore for RedbStore {
    fn create_media(&self, input: NewMedia) -> Result<MediaAttachment> {
        // ONE transaction: row + owner index commit together.
        let write = self.db().begin_write()?;

        let media = MediaAttachment {
            id: self.next_id(),
            account_id: input.account_id,
            r#type: input.r#type,
            url: None,
            state: MediaState::Pending,
            description: input.description,
            blurhash: None,
            status_id: None,
        };

        {
            let mut table = write.open_table(MEDIA)?;
            table.insert(media.id.value(), encode(&media)?.as_slice())?;
        }
        {
            let mut table = write.open_table(MEDIA_BY_OWNER)?;
            table.insert(
                schema::pair_key(media.account_id.value(), media.id.value()).as_slice(),
                (),
            )?;
        }

        write.commit()?;
        Ok(media)
    }

    fn attachable_media(
        &self,
        owner_id: Id,
        ids: &[Id],
        status_id: Option<Id>,
    ) -> Result<Vec<MediaAttachment>> {
        // Read-only: return the subset the caller may attach, in the
        // requested order. The status create/edit paths additionally validate
        // *inside* their transaction (see `check_attachable_txn`) so a racing
        // attach can never slip through.
        let read = self.db().begin_read()?;
        let table = read.open_table(MEDIA)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(guard) = table.get(id.value())? else {
                continue;
            };
            let media: MediaAttachment = decode(guard.value())?;
            let owned = media.account_id == owner_id;
            let attachable = media.status_id.is_none() || media.status_id == status_id;
            if owned && attachable {
                out.push(media);
            }
        }
        Ok(out)
    }

    fn media_by_id(&self, id: Id) -> Result<Option<MediaAttachment>> {
        let read = self.db().begin_read()?;
        let table = read.open_table(MEDIA)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }
}

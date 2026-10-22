//! Media attachment metadata persistence.
//!
//! Attachment bytes and the processing lifecycle are Phase 4; this module
//! owns the metadata rows written by the upload step plus the ownership /
//! attachability checks that run *inside* a status create or edit
//! transaction, so a status can never reference a missing or foreign
//! attachment.

use std::collections::HashMap;

use redb::{ReadableTable, WriteTransaction};

use crate::application::ports::{MediaPatch, MediaProcessingResult, MediaStore, NewMedia, NewMediaUpload};
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
        let write = self.begin_write()?;

        let media = MediaAttachment {
            id: self.next_id(),
            account_id: input.account_id,
            r#type: input.r#type,
            url: None,
            state: MediaState::Pending,
            description: input.description,
            blurhash: None,
            status_id: None,
            file_key: String::new(),
            preview_key: None,
            preview_width: None,
            preview_height: None,
            thumbnail_key: None,
            mime_type: String::new(),
            file_size: 0,
            width: None,
            height: None,
            duration_ms: None,
            frame_rate: None,
            bitrate: None,
            focus: None,
            created_at_ms: self.now_ms(),
            updated_at_ms: self.now_ms(),
            error: None,
            license: input.license,
        };

        super::statuses_read::ensure_id_unused(&write, MEDIA, media.id)?;
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
        let read = self.begin_read()?;
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
        let read = self.begin_read()?;
        let table = read.open_table(MEDIA)?;
        match table.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn media_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, MediaAttachment>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // ONE read transaction for the whole page's attachments.
        let read = self.begin_read()?;
        let table = read.open_table(MEDIA)?;
        let mut out = HashMap::with_capacity(ids.len());
        for id in ids {
            if out.contains_key(id) {
                continue;
            }
            if let Some(guard) = table.get(id.value())? {
                out.insert(*id, decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn create_media_upload(&self, input: NewMediaUpload, now_ms: u64) -> Result<MediaAttachment> {
        // ONE transaction: metadata row + owner index + ProcessMedia job
        // commit together; the staged bytes are already on disk.
        let write = self.begin_write()?;
        let media = MediaAttachment {
            id: self.next_id(),
            account_id: input.account_id,
            r#type: input.r#type,
            url: None,
            state: MediaState::Pending,
            description: input.description,
            blurhash: None,
            status_id: None,
            file_key: input.file_key,
            preview_key: None,
            preview_width: None,
            preview_height: None,
            thumbnail_key: input.thumbnail_key,
            mime_type: input.mime_type,
            file_size: input.file_size,
            width: input.width,
            height: input.height,
            duration_ms: input.duration_ms,
            frame_rate: input.frame_rate,
            bitrate: input.bitrate,
            focus: input.focus,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            error: None,
            license: input.license,
        };
        write_media_txn(&write, &media)?;
        {
            let mut table = write.open_table(MEDIA_BY_OWNER)?;
            table.insert(
                schema::pair_key(media.account_id.value(), media.id.value()).as_slice(),
                (),
            )?;
        }
        // The job id doubles as the media id (globally unique), so the job
        // and its attachment can never diverge.
        let job = crate::domain::Job::new(
            media.id,
            crate::domain::JobKind::ProcessMedia,
            serde_json::json!({ "media_id": media.id.value(), "delay": input.delay }),
            now_ms,
            now_ms,
        );
        super::jobs::insert_job(&write, &job)?;
        write.commit()?;
        Ok(media)
    }

    fn owner_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>> {
        let read = self.begin_read()?;
        let table = read.open_table(MEDIA)?;
        match table.get(id.value())? {
            Some(guard) => {
                let media: MediaAttachment = decode(guard.value())?;
                if media.account_id == account_id {
                    Ok(Some(media))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    fn update_media(
        &self,
        account_id: Id,
        id: Id,
        patch: MediaPatch,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>> {
        let write = self.begin_write()?;
        let Some(mut media) = read_media_txn(&write, id)? else {
            return Ok(None);
        };
        if media.account_id != account_id {
            return Ok(None);
        }
        if let Some(description) = patch.description {
            media.description = description;
        }
        if let Some(focus) = patch.focus {
            media.focus = focus;
        }
        if let Some(thumbnail) = patch.thumbnail_key {
            media.thumbnail_key = thumbnail;
        }
        if let Some(license) = patch.license {
            media.license = license;
        }
        media.updated_at_ms = now_ms;
        write_media_txn(&write, &media)?;
        write.commit()?;
        Ok(Some(media))
    }

    fn delete_media(&self, account_id: Id, id: Id) -> Result<Option<MediaAttachment>> {
        let write = self.begin_write()?;
        let Some(media) = read_media_txn(&write, id)? else {
            return Ok(None);
        };
        if media.account_id != account_id {
            return Ok(None);
        }
        {
            let mut table = write.open_table(MEDIA)?;
            table.remove(id.value())?;
        }
        {
            let mut table = write.open_table(MEDIA_BY_OWNER)?;
            table.remove(
                schema::pair_key(media.account_id.value(), media.id.value()).as_slice(),
            )?;
        }
        write.commit()?;
        Ok(Some(media))
    }

    fn mark_media_processed(
        &self,
        id: Id,
        result: MediaProcessingResult,
        now_ms: u64,
    ) -> Result<Option<MediaAttachment>> {
        let write = self.begin_write()?;
        let Some(mut media) = read_media_txn(&write, id)? else {
            return Ok(None);
        };
        media.state = result.state;
        media.r#type = result.r#type;
        media.blurhash = result.blurhash.or(media.blurhash);
        media.preview_key = result.preview_key.or(media.preview_key);
        media.preview_width = result.preview_width.or(media.preview_width);
        media.preview_height = result.preview_height.or(media.preview_height);
        media.width = result.width.or(media.width);
        media.height = result.height.or(media.height);
        media.duration_ms = result.duration_ms.or(media.duration_ms);
        media.frame_rate = result.frame_rate.or(media.frame_rate);
        media.bitrate = result.bitrate.or(media.bitrate);
        media.error = result.error;
        media.updated_at_ms = now_ms;
        write_media_txn(&write, &media)?;
        write.commit()?;
        Ok(Some(media))
    }
}

//! Phase 7 web/tag rows: settings sync, web push subscriptions, tag
//! follows and featured tags. One write transaction per mutation; reads are
//! bounded scans.

use redb::ReadableTable;

use crate::application::crypto;
use crate::application::ports::{FeaturedTag, TagFollow, WebPushSubscription, WebStore};
use crate::domain::{Id, Result};

use super::schema::{self, FEATURED_TAGS, TAG_FOLLOWS, WEB_PUSH_SUBSCRIPTIONS, WEB_SETTINGS};
use super::{decode, encode, RedbStore};

/// Upper bound for one account's tag-follow scan.
const TAG_SCAN_CAP: usize = 2_000;

/// `(account_id_be || tag_utf8)` range for prefix scans (`0xFF` never
/// appears in UTF-8, so `prefix || 0xFF` closes the range).
fn tag_range(account_id: Id) -> (Vec<u8>, Vec<u8>) {
    let mut lower = Vec::with_capacity(9);
    lower.extend_from_slice(&account_id.value().to_be_bytes());
    let mut upper = lower.clone();
    upper.push(0xFF);
    (lower, upper)
}

fn tag_key(account_id: Id, tag: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(8 + tag.len());
    key.extend_from_slice(&account_id.value().to_be_bytes());
    key.extend_from_slice(tag.as_bytes());
    key
}

impl WebStore for RedbStore {
    fn web_settings(&self, user_id: Id) -> Result<Option<String>> {
        let read = self.begin_read()?;
        let table = read.open_table(WEB_SETTINGS)?;
        Ok(table
            .get(user_id.value())?
            .map(|guard| String::from_utf8_lossy(guard.value()).into_owned()))
    }

    fn set_web_settings(&self, user_id: Id, data: &str) -> Result<String> {
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(WEB_SETTINGS)?;
            table.insert(user_id.value(), data.as_bytes())?;
        }
        write.commit()?;
        Ok(data.to_string())
    }

    fn web_push_subscriptions(&self, user_id: Id) -> Result<Vec<WebPushSubscription>> {
        let read = self.begin_read()?;
        let table = read.open_table(WEB_PUSH_SUBSCRIPTIONS)?;
        let (lower, upper) = (
            schema::pair_key(user_id.value(), 0),
            schema::pair_key(user_id.value(), u64::MAX),
        );
        let mut rows: Vec<WebPushSubscription> = Vec::new();
        for entry in table.range(lower.as_slice()..=upper.as_slice())? {
            let (_key, value) = entry?;
            rows.push(decode(value.value())?);
            if rows.len() >= TAG_SCAN_CAP {
                break;
            }
        }
        rows.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(rows)
    }

    fn create_web_push_subscription(
        &self,
        user_id: Id,
        endpoint: &str,
        key_p256dh: &str,
        key_auth: &str,
        standard: bool,
        data: &str,
        now_ms: u64,
    ) -> Result<WebPushSubscription> {
        let row = WebPushSubscription {
            id: self.next_id(),
            user_id,
            endpoint: endpoint.to_string(),
            key_auth: key_auth.to_string(),
            key_p256dh: key_p256dh.to_string(),
            standard,
            data: data.to_string(),
            unsubscribe_token: crypto::random_token_hex(24),
            created_at_ms: now_ms,
        };
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(WEB_PUSH_SUBSCRIPTIONS)?;
            table.insert(
                schema::pair_key(user_id.value(), row.id.value()).as_slice(),
                encode(&row)?.as_slice(),
            )?;
        }
        write.commit()?;
        Ok(row)
    }

    fn update_web_push_subscription(
        &self,
        user_id: Id,
        id: Id,
        data: &str,
        _now_ms: u64,
    ) -> Result<Option<WebPushSubscription>> {
        let write = self.begin_write()?;
        let updated = {
            let mut table = write.open_table(WEB_PUSH_SUBSCRIPTIONS)?;
            let key = schema::pair_key(user_id.value(), id.value());
            let Some(guard) = table.get(key.as_slice())? else {
                return Ok(None);
            };
            let mut row: WebPushSubscription = decode(guard.value())?;
            drop(guard);
            row.data = data.to_string();
            table.insert(key.as_slice(), encode(&row)?.as_slice())?;
            row
        };
        write.commit()?;
        Ok(Some(updated))
    }

    fn destroy_web_push_subscription(
        &self,
        user_id: Id,
        id: Id,
    ) -> Result<Option<WebPushSubscription>> {
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(WEB_PUSH_SUBSCRIPTIONS)?;
            let key = schema::pair_key(user_id.value(), id.value());
            let Some(guard) = table.get(key.as_slice())? else {
                return Ok(None);
            };
            let row: WebPushSubscription = decode(guard.value())?;
            drop(guard);
            table.remove(key.as_slice())?;
            row
        };
        write.commit()?;
        Ok(Some(removed))
    }

    fn destroy_web_push_subscription_by_token(
        &self,
        token: &str,
    ) -> Result<Option<WebPushSubscription>> {
        // Token lookup scans every subscription row (bounded): rows only
        // exist for users who created them, and the scan cap bounds abuse.
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(WEB_PUSH_SUBSCRIPTIONS)?;
            let mut found: Option<(Vec<u8>, WebPushSubscription)> = None;
            for entry in table.iter()? {
                let (key, value) = entry?;
                let row: WebPushSubscription = decode(value.value())?;
                if row.unsubscribe_token == token {
                    found = Some((key.value().to_vec(), row));
                    break;
                }
            }
            match found {
                Some((key, row)) => {
                    table.remove(key.as_slice())?;
                    Some(row)
                }
                None => None,
            }
        };
        write.commit()?;
        Ok(removed)
    }

    fn follow_tag(&self, account_id: Id, tag: &str, now_ms: u64) -> Result<TagFollow> {
        if let Some(existing) = self.tag_follow(account_id, tag)? {
            return Ok(existing);
        }
        let row = TagFollow {
            id: self.next_id(),
            account_id,
            tag: tag.to_string(),
            created_at_ms: now_ms,
        };
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(TAG_FOLLOWS)?;
            table.insert(tag_key(account_id, tag).as_slice(), encode(&row)?.as_slice())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn unfollow_tag(&self, account_id: Id, tag: &str) -> Result<Option<TagFollow>> {
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(TAG_FOLLOWS)?;
            let key = tag_key(account_id, tag);
            let Some(guard) = table.get(key.as_slice())? else {
                return Ok(None);
            };
            let row: TagFollow = decode(guard.value())?;
            drop(guard);
            table.remove(key.as_slice())?;
            row
        };
        write.commit()?;
        Ok(Some(removed))
    }

    fn followed_tags(
        &self,
        account_id: Id,
        max_id: Option<Id>,
        since_id: Option<Id>,
        limit: usize,
    ) -> Result<Vec<TagFollow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let read = self.begin_read()?;
        let (lower, upper) = tag_range(account_id);
        let table = read.open_table(TAG_FOLLOWS)?;
        let mut rows: Vec<TagFollow> = Vec::new();
        for entry in table.range(lower.as_slice()..=upper.as_slice())? {
            let (_key, value) = entry?;
            rows.push(decode(value.value())?);
            if rows.len() >= TAG_SCAN_CAP {
                break;
            }
        }
        rows.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(rows
            .into_iter()
            .filter(|row| max_id.is_none_or(|m| row.id < m))
            .filter(|row| since_id.is_none_or(|m| row.id > m))
            .take(limit)
            .collect())
    }

    fn tag_follow(&self, account_id: Id, tag: &str) -> Result<Option<TagFollow>> {
        let read = self.begin_read()?;
        let table = read.open_table(TAG_FOLLOWS)?;
        Ok(table
            .get(tag_key(account_id, tag).as_slice())?
            .map(|guard| decode(guard.value()))
            .transpose()?)
    }

    fn feature_tag(&self, account_id: Id, tag: &str, now_ms: u64) -> Result<FeaturedTag> {
        if let Some(existing) = self.featured_tag(account_id, tag)? {
            return Ok(existing);
        }
        let row = FeaturedTag {
            id: self.next_id(),
            account_id,
            tag: tag.to_string(),
            created_at_ms: now_ms,
        };
        let write = self.begin_write()?;
        {
            let mut table = write.open_table(FEATURED_TAGS)?;
            table.insert(tag_key(account_id, tag).as_slice(), encode(&row)?.as_slice())?;
        }
        write.commit()?;
        Ok(row)
    }

    fn unfeature_tag(&self, account_id: Id, id: Id) -> Result<Option<FeaturedTag>> {
        let write = self.begin_write()?;
        let removed = {
            let mut table = write.open_table(FEATURED_TAGS)?;
            let (lower, upper) = tag_range(account_id);
            let mut found: Option<(Vec<u8>, FeaturedTag)> = None;
            for entry in table.range(lower.as_slice()..=upper.as_slice())? {
                let (key, value) = entry?;
                let row: FeaturedTag = decode(value.value())?;
                if row.id == id {
                    found = Some((key.value().to_vec(), row));
                    break;
                }
            }
            match found {
                Some((key, row)) => {
                    table.remove(key.as_slice())?;
                    Some(row)
                }
                None => None,
            }
        };
        write.commit()?;
        Ok(removed)
    }

    fn featured_tags_of(&self, account_id: Id) -> Result<Vec<FeaturedTag>> {
        let read = self.begin_read()?;
        let (lower, upper) = tag_range(account_id);
        let table = read.open_table(FEATURED_TAGS)?;
        let mut rows: Vec<FeaturedTag> = Vec::new();
        for entry in table.range(lower.as_slice()..=upper.as_slice())? {
            let (_key, value) = entry?;
            rows.push(decode(value.value())?);
            if rows.len() >= TAG_SCAN_CAP {
                break;
            }
        }
        Ok(rows)
    }

    fn featured_tag(&self, account_id: Id, tag: &str) -> Result<Option<FeaturedTag>> {
        let read = self.begin_read()?;
        let table = read.open_table(FEATURED_TAGS)?;
        Ok(table
            .get(tag_key(account_id, tag).as_slice())?
            .map(|guard| decode(guard.value()))
            .transpose()?)
    }
}

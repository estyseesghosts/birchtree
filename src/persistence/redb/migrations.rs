//! Versioned schema migration runner.
//!
//! Migrations are applied once, in order, recorded in [`schema::META`], and are
//! idempotent/restartable: opening every table in a write transaction creates
//! any missing table, so re-running a step is harmless. The runner refuses to
//! open a database written by a newer build.

use redb::{Database, ReadableDatabase};

use super::schema::{
    self, ACCESS_TOKENS, ACCOUNTS, ACCOUNT_NOTES, ACCOUNT_STATS, AUTHORIZATION_CODES, BLOCKS,
    BLOCKS_BY_BLOCKED, BOOKMARKS, BOOKMARKS_BY_STATUS, CUSTOM_EMOJI, DOMAIN_BLOCKS,
    DOMAIN_BLOCKS_BY_ID, ENDORSEMENTS,
    ENDORSEMENTS_BY_ENDORSED, FAVOURITES, FAVOURITES_BY_STATUS, FILTERS, FILTER_KEYWORDS,
    FILTER_KEYWORDS_BY_FILTER, FILTER_STATUSES, FILTER_STATUSES_BY_FILTER, FILTERS_BY_ACCOUNT,
    FOLLOWS, FOLLOWS_BY_FOLLOWED, HANDLE_INDEX, HASHTAG_INDEX, IDEMPOTENCY, JOBS, JOBS_INDEX,
    LISTS, LISTS_BY_ACCOUNT, LIST_MEMBERS, LOCAL_USERS, MARKERS, MEDIA, MEDIA_BY_OWNER,
    MEMBERSHIPS_BY_ACCOUNT, MENTIONS_BY_ACCOUNT, META, MUTES, MUTES_BY_MUTED, NOTIFICATIONS,
    NOTIFICATIONS_BY_RECIPIENT, NOTIFICATION_DEDUP, OAUTH_APPS, POLL_BY_STATUS, POLLS, POLL_VOTES,
    QUOTES, QUOTES_BY_QUOTED, QUOTES_BY_QUOTING, REBLOGS_BY_ACCOUNT, SCHEDULED_BY_ACCOUNT,
    SCHEDULED_STATUSES, SESSIONS, STATUSES, STATUS_AUTHOR_INDEX, STATUS_PARENT_INDEX,
    STATUS_PINS, STATUS_REBLOG_INDEX, STATUS_VERSIONS, TIMELINE_LOCAL, TIMELINE_PUBLIC,
};
use super::StoreError;

/// Desired schema version for this build.
pub fn target_version() -> u64 {
    schema::SCHEMA_VERSION
}

/// Read the applied schema version, or `0` for a fresh database.
pub fn applied_version(db: &Database) -> Result<u64, StoreError> {
    let read = db.begin_read()?;
    let table = match read.open_table(META) {
        Ok(table) => table,
        // A brand-new database has no `meta` table yet.
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(err) => return Err(err.into()),
    };
    match table.get(schema::SCHEMA_VERSION_KEY)? {
        Some(value) => {
            let bytes = value.value();
            let arr: [u8; 8] = bytes
                .try_into()
                .map_err(|_| StoreError::Migration("corrupt schema_version record".into()))?;
            Ok(u64::from_be_bytes(arr))
        }
        None => Ok(0),
    }
}

/// Apply pending migrations. Idempotent and safe to call on every open.
pub fn apply(db: &Database) -> Result<u64, StoreError> {
    let current = applied_version(db)?;
    let target = target_version();

    if current > target {
        return Err(StoreError::Migration(format!(
            "database schema v{current} is newer than this build's v{target}; upgrade the server"
        )));
    }
    if current == target {
        return Ok(current);
    }

    let write = db.begin_write()?;
    {
        // Step v1: create every table this build knows about. Opening a table
        // in a write transaction creates it; re-running is idempotent.
        if current < 1 {
            let _ = write.open_table(META)?;
            let _ = write.open_table(ACCOUNTS)?;
            let _ = write.open_table(HANDLE_INDEX)?;
            let _ = write.open_table(LOCAL_USERS)?;
            let _ = write.open_table(OAUTH_APPS)?;
            let _ = write.open_table(ACCESS_TOKENS)?;
            let _ = write.open_table(SESSIONS)?;
            let _ = write.open_table(JOBS)?;
            let _ = write.open_table(JOBS_INDEX)?;
        }

        // Step v2: authorization codes for the authorization-code + PKCE grant.
        if current < 2 {
            let _ = write.open_table(AUTHORIZATION_CODES)?;
        }

        // Step v3: statuses, relationships, interactions, polls, quotes and
        // their indexes (Phase 3).
        if current < 3 {
            let _ = write.open_table(STATUSES)?;
            let _ = write.open_table(STATUS_AUTHOR_INDEX)?;
            let _ = write.open_table(STATUS_PARENT_INDEX)?;
            let _ = write.open_table(STATUS_REBLOG_INDEX)?;
            let _ = write.open_table(REBLOGS_BY_ACCOUNT)?;
            let _ = write.open_table(STATUS_VERSIONS)?;
            let _ = write.open_table(STATUS_PINS)?;
            let _ = write.open_table(FAVOURITES)?;
            let _ = write.open_table(FAVOURITES_BY_STATUS)?;
            let _ = write.open_table(BOOKMARKS)?;
            let _ = write.open_table(BOOKMARKS_BY_STATUS)?;
            let _ = write.open_table(FOLLOWS)?;
            let _ = write.open_table(FOLLOWS_BY_FOLLOWED)?;
            let _ = write.open_table(BLOCKS)?;
            let _ = write.open_table(BLOCKS_BY_BLOCKED)?;
            let _ = write.open_table(DOMAIN_BLOCKS)?;
            let _ = write.open_table(MUTES)?;
            let _ = write.open_table(MUTES_BY_MUTED)?;
            let _ = write.open_table(ACCOUNT_NOTES)?;
            let _ = write.open_table(ENDORSEMENTS)?;
            let _ = write.open_table(ENDORSEMENTS_BY_ENDORSED)?;
            let _ = write.open_table(POLLS)?;
            let _ = write.open_table(POLL_BY_STATUS)?;
            let _ = write.open_table(POLL_VOTES)?;
            let _ = write.open_table(QUOTES)?;
            let _ = write.open_table(QUOTES_BY_QUOTED)?;
            let _ = write.open_table(QUOTES_BY_QUOTING)?;
            let _ = write.open_table(NOTIFICATIONS)?;
            let _ = write.open_table(NOTIFICATIONS_BY_RECIPIENT)?;
            let _ = write.open_table(NOTIFICATION_DEDUP)?;
            let _ = write.open_table(IDEMPOTENCY)?;
            let _ = write.open_table(ACCOUNT_STATS)?;
            let _ = write.open_table(MEDIA)?;
            let _ = write.open_table(MEDIA_BY_OWNER)?;
            let _ = write.open_table(MENTIONS_BY_ACCOUNT)?;
            let _ = write.open_table(HASHTAG_INDEX)?;
        }

        // Step v4: timeline ranges, lists, markers, filters, scheduled
        // statuses and custom emoji (Phase 4), plus a backfill of the
        // timeline indexes from existing statuses.
        if current < 4 {
            let _ = write.open_table(TIMELINE_PUBLIC)?;
            let _ = write.open_table(TIMELINE_LOCAL)?;
            let _ = write.open_table(LISTS)?;
            let _ = write.open_table(LISTS_BY_ACCOUNT)?;
            let _ = write.open_table(LIST_MEMBERS)?;
            let _ = write.open_table(MEMBERSHIPS_BY_ACCOUNT)?;
            let _ = write.open_table(MARKERS)?;
            let _ = write.open_table(FILTERS)?;
            let _ = write.open_table(FILTERS_BY_ACCOUNT)?;
            let _ = write.open_table(FILTER_KEYWORDS)?;
            let _ = write.open_table(FILTER_KEYWORDS_BY_FILTER)?;
            let _ = write.open_table(FILTER_STATUSES)?;
            let _ = write.open_table(FILTER_STATUSES_BY_FILTER)?;
            let _ = write.open_table(SCHEDULED_STATUSES)?;
            let _ = write.open_table(SCHEDULED_BY_ACCOUNT)?;
            let _ = write.open_table(CUSTOM_EMOJI)?;

            // Backfill the timeline indexes. A status joins the public range
            // when it is public and not deleted; it joins the local range
            // when its author's account row says the account is local. The
            // scan is bounded by the status count of the pre-v4 database
            // (federation is Phase 6, so this is one node's worth of rows)
            // and idempotent: re-inserting the same keys is a no-op.
            use redb::{ReadableTable, ReadableTableMetadata};
            use crate::domain::{Account, Status, Visibility};
            let backfill: Vec<(u64, bool)> = {
                let statuses = write.open_table(STATUSES)?;
                let accounts = write.open_table(ACCOUNTS)?;
                let status_count = statuses.len()? as usize;
                let mut backfill: Vec<(u64, bool)> = Vec::new();
                for entry in statuses.range(0u64..=u64::MAX)? {
                    let (key, value) = entry?;
                    let id = key.value();
                    let status: Status = super::decode(value.value())?;
                    if status.is_deleted() || status.visibility != Visibility::Public {
                        continue;
                    }
                    let author_local = match accounts.get(status.account_id.value())? {
                        Some(guard) => {
                            let account: Account = super::decode(guard.value())?;
                            account.local
                        }
                        None => false,
                    };
                    backfill.push((id, author_local));
                    if backfill.len() >= status_count {
                        break;
                    }
                }
                backfill
            };
            let mut public = write.open_table(TIMELINE_PUBLIC)?;
            let mut local = write.open_table(TIMELINE_LOCAL)?;
            for (id, author_local) in backfill {
                public.insert(schema::desc_id(id), ())?;
                if author_local {
                    local.insert(schema::desc_id(id), ())?;
                }
            }
        }

        // Step v5: domain blocks get a numeric row id (Mastodon's paging
        // cursor) and an id index. A pre-v5 row has no id, so it is assigned
        // `created_at_ms << 16 | r`, where `r` numbers the account's rows that
        // share a creation millisecond, in domain order. Ids stay increasing
        // with creation time, and rows that already carry an id are kept, so
        // the step is idempotent. The scan is bounded by the pre-v5 block count.
        if current < 5 {
            use redb::ReadableTable;
            use crate::domain::{DomainBlock, Id};

            #[derive(serde::Deserialize)]
            struct LegacyDomainBlock {
                account_id: Id,
                domain: String,
                created_at_ms: u64,
            }

            // Same layout as the timestamp id generator (16 sequence bits).
            const SEQUENCE_BITS: u32 = 16;

            let raw: Vec<(Vec<u8>, Vec<u8>)> = {
                let blocks = write.open_table(DOMAIN_BLOCKS)?;
                let mut raw = Vec::new();
                for entry in blocks.iter()? {
                    let (key, value) = entry?;
                    raw.push((key.value().to_vec(), value.value().to_vec()));
                }
                raw
            };

            // (key, row, needs_id) for every row.
            let mut rows: Vec<(Vec<u8>, DomainBlock, bool)> = Vec::with_capacity(raw.len());
            for (key, value) in raw {
                match super::decode::<DomainBlock>(&value) {
                    Ok(block) => rows.push((key, block, false)),
                    Err(_) => {
                        let legacy: LegacyDomainBlock = super::decode(&value)?;
                        rows.push((
                            key,
                            DomainBlock {
                                id: Id::new(0),
                                account_id: legacy.account_id,
                                domain: legacy.domain,
                                created_at_ms: legacy.created_at_ms,
                            },
                            true,
                        ));
                    }
                }
            }

            let mut order: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].2).collect();
            order.sort_by(|&a, &b| {
                let (x, y) = (&rows[a].1, &rows[b].1);
                (x.account_id.value(), x.created_at_ms, &x.domain)
                    .cmp(&(y.account_id.value(), y.created_at_ms, &y.domain))
            });
            let mut last: Option<(u64, u64)> = None;
            let mut rank = 0u64;
            for i in order {
                let account = rows[i].1.account_id.value();
                let created = rows[i].1.created_at_ms;
                rank = if last == Some((account, created)) { rank + 1 } else { 0 };
                last = Some((account, created));
                rows[i].1.id = Id::new((created << SEQUENCE_BITS) | rank);
            }

            let mut blocks = write.open_table(DOMAIN_BLOCKS)?;
            let mut by_id = write.open_table(DOMAIN_BLOCKS_BY_ID)?;
            for (key, block, assigned) in &rows {
                let encoded = super::encode(block)?;
                if *assigned {
                    blocks.insert(key.as_slice(), encoded.as_slice())?;
                }
                by_id.insert(
                    schema::pair_key(block.account_id.value(), schema::desc_id(block.id.value()))
                        .as_slice(),
                    encoded.as_slice(),
                )?;
            }
        }

        // Step v6: federation keys, actor-URI index, inbound activity dedup and
        // outbound follow-by-URI index (Phase 6). The actor-URI index is backfilled
        // from the accounts table: local accounts were not indexed before v6.
        if current < 6 {
            let _ = write.open_table(schema::ACTOR_KEYS)?;
            let _ = write.open_table(schema::ACTIVITY_DEDUP)?;
            let _ = write.open_table(schema::FOLLOW_BY_URI)?;
            use redb::ReadableTable;
            use crate::domain::Account;
            let rows: Vec<(u64, String)> = {
                let accounts = write.open_table(ACCOUNTS)?;
                let mut rows = Vec::new();
                for entry in accounts.iter()? {
                    let (key, value) = entry?;
                    let account: Account = super::decode(value.value())?;
                    rows.push((key.value(), account.actor_uri));
                }
                rows
            };
            let mut by_actor = write.open_table(schema::ACCOUNT_BY_ACTOR_URI)?;
            for (id, actor_uri) in rows {
                by_actor.insert(actor_uri.as_str(), id)?;
            }
        }

        // Step v7: status identity by URI (backfilled from every existing status),
        // the remote-thread refresh table, and reports.
        if current < 7 {
            let _ = write.open_table(schema::REMOTE_REFRESH)?;
            let _ = write.open_table(schema::REPORTS)?;
            let _ = write.open_table(schema::REMOTE_MEDIA_SOURCE)?;
            let _ = write.open_table(schema::QUOTE_BY_APPROVAL)?;
            let _ = write.open_table(schema::UNRESOLVED_PARENT)?;
            use redb::ReadableTable;
            use crate::domain::Status;
            let rows: Vec<(String, u64)> = {
                let statuses = write.open_table(STATUSES)?;
                let mut rows = Vec::new();
                for entry in statuses.iter()? {
                    let (key, value) = entry?;
                    let status: Status = super::decode(value.value())?;
                    rows.push((status.uri, key.value()));
                }
                rows
            };
            let mut by_uri = write.open_table(schema::STATUS_BY_URI)?;
            for (uri, id) in rows {
                by_uri.insert(uri.as_str(), id)?;
            }
        }

        // Step v8: admin infrastructure rows (Phase 7.3, subagent C). Tables
        // only; no backfill (the domain allow/block lists start empty, and
        // tag rows materialize from hashtag usage on first admin read).
        if current < 8 {
            let _ = write.open_table(schema::ADMIN_DOMAIN_ALLOWS)?;
            let _ = write.open_table(schema::ADMIN_DOMAIN_ALLOW_BY_DOMAIN)?;
            let _ = write.open_table(schema::ADMIN_DOMAIN_BLOCKS)?;
            let _ = write.open_table(schema::ADMIN_DOMAIN_BLOCK_BY_DOMAIN)?;
            let _ = write.open_table(schema::ADMIN_EMAIL_DOMAIN_BLOCKS)?;
            let _ = write.open_table(schema::ADMIN_EMAIL_DOMAIN_BLOCK_BY_DOMAIN)?;
            let _ = write.open_table(schema::ADMIN_IP_BLOCKS)?;
            let _ = write.open_table(schema::ADMIN_IP_BLOCK_BY_IP)?;
            let _ = write.open_table(schema::ADMIN_CANONICAL_EMAIL_BLOCKS)?;
            let _ = write.open_table(schema::ADMIN_CANONICAL_EMAIL_BLOCK_BY_HASH)?;
            let _ = write.open_table(schema::ADMIN_TAGS)?;
            let _ = write.open_table(schema::ADMIN_TAG_BY_NAME)?;
        }

        // Step v8: admin audit trail (Phase 7.3). REPORTS already exists
        // (v7); the audit table is new. No backfill: pre-7.3 databases have
        // no admin writes to record (the admin routes did not exist).
        if current < 8 {
            let _ = write.open_table(schema::ADMIN_AUDIT)?;
        }

        // Step v8: trend approval flags/candidates and annual reports (Phase
        // 7.3, trends workstream). Tables only; candidate rows materialize
        // from real usage on read (hashtag index, recent public statuses,
        // URLs in status text), so there is nothing to backfill.
        if current < 8 {
            let _ = write.open_table(schema::TREND_TAG_FLAGS)?;
            let _ = write.open_table(schema::TREND_STATUS_FLAGS)?;
            let _ = write.open_table(schema::TREND_LINKS)?;
            let _ = write.open_table(schema::TREND_LINKS_BY_URL)?;
            let _ = write.open_table(schema::TREND_PUBLISHERS)?;
            let _ = write.open_table(schema::TREND_PUBLISHERS_BY_DOMAIN)?;
            let _ = write.open_table(schema::ANNUAL_REPORTS)?;
            let _ = write.open_table(schema::ANNUAL_REPORTS_BY_ACCOUNT)?;
            let _ = write.open_table(schema::ADMIN_AUDIT_BY_TARGET)?;
        }

        // Step v9: Phase 7 conversation state/mutes, push subscriptions, and
        // web/tag rows. Tables only; per-viewer state, subscriptions, tag
        // follows and featured tags materialize from real client writes, so
        // there is nothing to backfill.
        if current < 9 {
            let _ = write.open_table(schema::CONVERSATION_STATE)?;
            let _ = write.open_table(schema::CONVERSATION_MUTES)?;
            let _ = write.open_table(schema::PUSH_SUBSCRIPTIONS)?;
            let _ = write.open_table(schema::PUSH_SUBSCRIPTION_BY_TOKEN)?;
            let _ = write.open_table(schema::PUSH_SUBSCRIPTIONS_BY_USER)?;
            let _ = write.open_table(schema::WEB_SETTINGS)?;
            let _ = write.open_table(schema::WEB_PUSH_SUBSCRIPTIONS)?;
            let _ = write.open_table(schema::TAG_FOLLOWS)?;
            let _ = write.open_table(schema::FEATURED_TAGS)?;
        }

        // Step v10: remote tombstones (Delete-beats-Create) and per-destination
        // delivery state. Tables only; both materialize from live federation
        // traffic, so there is nothing to backfill.
        if current < 10 {
            let _ = write.open_table(schema::REMOTE_TOMBSTONE)?;
            let _ = write.open_table(schema::DELIVERY_DEST)?;
        }

        // Step v11: server rules, account warnings, sign-in telemetry (D37,
        // D39). Tables only; rules are operator-managed and the event tables
        // materialize from live traffic, so there is nothing to backfill.
        if current < 11 {
            let _ = write.open_table(schema::RULES)?;
            let _ = write.open_table(schema::ACCOUNT_WARNINGS)?;
            let _ = write.open_table(schema::SIGN_INS)?;
        }

        // Step v12: durable invites, email index + tokens, interaction events
        // (D12/D39 remainders). The event tables materialize from live
        // traffic; the email index is backfilled from existing credential
        // rows (bounded by the pre-v12 user count; skips rows without an
        // address; re-inserting the same keys is a no-op).
        if current < 12 {
            let _ = write.open_table(schema::INVITES)?;
            let _ = write.open_table(schema::EMAIL_INDEX)?;
            let _ = write.open_table(schema::EMAIL_TOKENS)?;
            let _ = write.open_table(schema::INTERACTION_EVENTS)?;
            use redb::{ReadableTable, ReadableTableMetadata};
            let backfill: Vec<(String, u64)> = {
                let users = write.open_table(schema::LOCAL_USERS)?;
                let user_count = users.len()? as usize;
                let mut backfill = Vec::new();
                for entry in users.range(0u64..=u64::MAX)? {
                    let (_, value) = entry?;
                    let user: serde_json::Value = serde_json::from_slice(value.value())?;
                    let email = user
                        .get("email")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|e| !e.is_empty());
                    let account_id = user.get("account_id").and_then(|v| v.as_u64());
                    if let (Some(email), Some(account_id)) = (email, account_id) {
                        backfill.push((email.to_ascii_lowercase(), account_id));
                    }
                    if backfill.len() >= user_count {
                        break;
                    }
                }
                backfill
            };
            let mut index = write.open_table(schema::EMAIL_INDEX)?;
            for (email, account_id) in backfill {
                index.insert(email.as_str(), account_id)?;
            }
        }

        {
            let mut meta = write.open_table(META)?;
            meta.insert(
                schema::SCHEMA_VERSION_KEY,
                target.to_be_bytes().as_slice(),
            )?;
        }
    }
    write.commit()?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::create(dir.path().join("test.redb")).unwrap();
        (dir, db)
    }

    #[test]
    fn fresh_database_migrates_to_target() {
        let (_dir, db) = temp_db();
        assert_eq!(applied_version(&db).unwrap(), 0);
        assert_eq!(apply(&db).unwrap(), target_version());
        assert_eq!(applied_version(&db).unwrap(), target_version());
    }

    #[test]
    fn migration_is_idempotent_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        {
            let db = Database::create(&path).unwrap();
            apply(&db).unwrap();
        }
        {
            let db = Database::create(&path).unwrap();
            // Re-applying is a no-op returning the current version.
            assert_eq!(apply(&db).unwrap(), target_version());
        }
    }

    #[test]
    fn upgrades_v2_database_to_v3() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");
        {
            // Simulate a Phase 2 database: v1+v2 tables present, version 2.
            let db = Database::create(&path).unwrap();
            let write = db.begin_write().unwrap();
            {
                let _ = write.open_table(META).unwrap();
                let _ = write.open_table(ACCOUNTS).unwrap();
                let _ = write.open_table(HANDLE_INDEX).unwrap();
                let _ = write.open_table(LOCAL_USERS).unwrap();
                let _ = write.open_table(OAUTH_APPS).unwrap();
                let _ = write.open_table(ACCESS_TOKENS).unwrap();
                let _ = write.open_table(SESSIONS).unwrap();
                let _ = write.open_table(JOBS).unwrap();
                let _ = write.open_table(JOBS_INDEX).unwrap();
                let _ = write.open_table(schema::AUTHORIZATION_CODES).unwrap();
                let mut meta = write.open_table(META).unwrap();
                meta.insert(schema::SCHEMA_VERSION_KEY, 2u64.to_be_bytes().as_slice())
                    .unwrap();
            }
            write.commit().unwrap();
        }
        {
            let db = Database::create(&path).unwrap();
            assert_eq!(applied_version(&db).unwrap(), 2);
            assert_eq!(apply(&db).unwrap(), target_version());
            // The Phase 3 tables exist after the upgrade.
            let read = db.begin_read().unwrap();
            assert!(read.open_table(schema::STATUSES).is_ok());
            assert!(read.open_table(schema::FOLLOWS).is_ok());
            assert!(read.open_table(schema::QUOTES).is_ok());
        }
    }

    #[test]
    fn upgrades_v4_database_backfills_domain_block_ids() {
        let (_dir, db) = temp_db();
        // A pre-v5 database: legacy domain-block rows (no id), stamped v4.
        {
            let write = db.begin_write().unwrap();
            {
                let mut blocks = write.open_table(schema::DOMAIN_BLOCKS).unwrap();
                let rows = [
                    (7u64, "b.example", 1_000u64),
                    (7, "a.example", 1_000),
                    (7, "c.example", 2_000),
                    (8, "x.example", 1_000),
                ];
                for (account, domain, created) in rows {
                    let key = schema::domain_block_key(account, domain);
                    let legacy = serde_json::json!({
                        "account_id": account,
                        "domain": domain,
                        "created_at_ms": created,
                    });
                    blocks
                        .insert(key.as_slice(), serde_json::to_vec(&legacy).unwrap().as_slice())
                        .unwrap();
                }
            }
            {
                let mut meta = write.open_table(schema::META).unwrap();
                meta.insert(schema::SCHEMA_VERSION_KEY, 4u64.to_be_bytes().as_slice())
                    .unwrap();
            }
            write.commit().unwrap();
        }

        assert_eq!(apply(&db).unwrap(), target_version());

        let read = db.begin_read().unwrap();
        let blocks = read.open_table(schema::DOMAIN_BLOCKS).unwrap();
        let by_id = read.open_table(schema::DOMAIN_BLOCKS_BY_ID).unwrap();
        let block_at = |account: u64, domain: &str| -> crate::domain::DomainBlock {
            let key = schema::domain_block_key(account, domain);
            let guard = blocks.get(key.as_slice()).unwrap().expect("row kept");
            serde_json::from_slice(guard.value()).unwrap()
        };
        // Within one millisecond, the rank follows domain order; a later
        // millisecond always has the larger id.
        assert_eq!(block_at(7, "a.example").id.value(), 1_000u64 << 16);
        assert_eq!(block_at(7, "b.example").id.value(), (1_000u64 << 16) | 1);
        assert_eq!(block_at(7, "c.example").id.value(), 2_000u64 << 16);
        assert_eq!(block_at(8, "x.example").id.value(), 1_000u64 << 16);

        // The id index is newest first, and per account.
        let mut domains = Vec::new();
        for entry in by_id
            .range(schema::pair_key(7, 0).as_slice()..schema::pair_key(7, u64::MAX).as_slice())
            .unwrap()
        {
            let (_key, value) = entry.unwrap();
            let block: crate::domain::DomainBlock =
                serde_json::from_slice(value.value()).unwrap();
            domains.push(block.domain);
        }
        assert_eq!(domains, ["c.example", "b.example", "a.example"]);
    }

    #[test]
    fn refuses_newer_schema() {
        let (_dir, db) = temp_db();
        apply(&db).unwrap();
        // Stamp a future version and ensure the runner refuses it.
        let write = db.begin_write().unwrap();
        {
            let mut meta = write.open_table(META).unwrap();
            meta.insert(
                schema::SCHEMA_VERSION_KEY,
                (target_version() + 1).to_be_bytes().as_slice(),
            )
            .unwrap();
        }
        write.commit().unwrap();
        assert!(matches!(apply(&db), Err(StoreError::Migration(_))));
    }
}

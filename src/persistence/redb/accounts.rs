//! Account and local-credential persistence.

use redb::{ReadableDatabase, ReadableTable};

use crate::application::ports::{AccountStore, NewLocalAccount};
use crate::domain::{Account, Error, Id, LocalUser, Result};

use super::schema::{ACCOUNTS, HANDLE_INDEX, LOCAL_USERS};
use super::{decode, encode, RedbStore, StoreResult};

fn read_account(store: &RedbStore, id: Id) -> StoreResult<Option<Account>> {
    let read = store.db().begin_read()?;
    let table = read.open_table(ACCOUNTS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn read_local_user(store: &RedbStore, account_id: Id) -> StoreResult<Option<LocalUser>> {
    let read = store.db().begin_read()?;
    let table = read.open_table(LOCAL_USERS)?;
    match table.get(account_id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

impl AccountStore for RedbStore {
    fn create_local_account(&self, input: NewLocalAccount) -> Result<Account> {
        let key = crate::domain::account::handle_key(&input.normalized_username, &input.domain);

        // Single writer: concurrent creates serialize here and the handle check
        // inside the transaction is authoritative.
        let write = self.db().begin_write()?;

        {
            let handles = write.open_table(HANDLE_INDEX)?;
            if handles.get(key.as_str())?.is_some() {
                return Err(Error::AlreadyExists(format!(
                    "handle already registered: {key}"
                )));
            }
        }

        let account_id = self.next_id();
        let user_id = self.next_id();

        let account = Account {
            id: account_id,
            username: input.username,
            normalized_username: input.normalized_username,
            domain: input.domain,
            display_name: input.display_name,
            note: String::new(),
            inbox_uri: Some(format!("{}/inbox", input.actor_uri)),
            shared_inbox_uri: None,
            actor_uri: input.actor_uri,
            local: true,
            locked: false,
            suspended: false,
            moved_to: None,
            created_at_ms: input.now_ms,
        };
        let user = LocalUser {
            id: user_id,
            account_id,
            password_hash: input.password_hash,
            email: input.email,
            email_verified: false,
            created_at_ms: input.now_ms,
            disabled: false,
        };

        {
            let mut accounts = write.open_table(ACCOUNTS)?;
            accounts.insert(account_id.value(), encode(&account)?.as_slice())?;
        }
        {
            let mut handles = write.open_table(HANDLE_INDEX)?;
            handles.insert(key.as_str(), account_id.value())?;
        }
        {
            let mut users = write.open_table(LOCAL_USERS)?;
            users.insert(account_id.value(), encode(&user)?.as_slice())?;
        }

        // The bounded follow-up job commits atomically with the account.
        let job = crate::domain::Job::new(
            self.next_id(),
            crate::domain::JobKind::UserCreated,
            serde_json::json!({ "account_id": account_id.value() }),
            input.now_ms,
            input.now_ms,
        );
        super::jobs::insert_job(&write, &job)?;

        write.commit()?;
        Ok(account)
    }

    fn account_by_id(&self, id: Id) -> Result<Option<Account>> {
        Ok(read_account(self, id)?)
    }

    fn account_by_handle(
        &self,
        normalized_username: &str,
        domain: &str,
    ) -> Result<Option<Account>> {
        let key = crate::domain::account::handle_key(normalized_username, domain);
        let read = self.db().begin_read()?;
        let id = {
            let handles = read.open_table(HANDLE_INDEX)?;
            match handles.get(key.as_str())? {
                Some(guard) => Id::new(guard.value()),
                None => return Ok(None),
            }
        };
        let accounts = read.open_table(ACCOUNTS)?;
        match accounts.get(id.value())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn user_by_account_id(&self, account_id: Id) -> Result<Option<LocalUser>> {
        Ok(read_local_user(self, account_id)?)
    }

    fn local_user_by_handle(
        &self,
        normalized_username: &str,
        domain: &str,
    ) -> Result<Option<LocalUser>> {
        let account = self.account_by_handle(normalized_username, domain)?;
        match account {
            Some(account) => Ok(read_local_user(self, account.id)?),
            None => Ok(None),
        }
    }

    fn account_count(&self) -> Result<u64> {
        use redb::ReadableTableMetadata;
        let read = self.db().begin_read()?;
        let accounts = read.open_table(ACCOUNTS)?;
        Ok(accounts.len()?)
    }
}

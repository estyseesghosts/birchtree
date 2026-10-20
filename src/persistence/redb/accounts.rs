//! Account and local-credential persistence.

use std::collections::HashMap;

use redb::ReadableTable;

use crate::application::accounts::InviteStore;
use crate::application::ports::{
    AccountProfileUpdate, AccountStore, NewLocalAccount, UserSettingsUpdate,
};
use crate::domain::user::{EmailToken, Invite};
use crate::domain::{Account, Error, Id, LocalUser, Result, SignInEvent, UserRole};

use super::schema::{ACCOUNTS, EMAIL_INDEX, EMAIL_TOKENS, HANDLE_INDEX, INVITES, LOCAL_USERS};
use super::{decode, encode, RedbStore, StoreResult};

/// Normalize an email address for the [`EMAIL_INDEX`](super::schema::EMAIL_INDEX)
/// key: trim + ASCII-lowercase (matches the v12 backfill normalization, so
/// pre-existing rows and new rows share one key space).
fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn read_account(store: &RedbStore, id: Id) -> StoreResult<Option<Account>> {
    let read = store.begin_read()?;
    let table = read.open_table(ACCOUNTS)?;
    match table.get(id.value())? {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

fn read_local_user(store: &RedbStore, account_id: Id) -> StoreResult<Option<LocalUser>> {
    let read = store.begin_read()?;
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
        let write = self.begin_write()?;

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
            silenced: false,
            sensitized: false,
            moved_to: None,
            created_at_ms: input.now_ms,
        };
        let user = LocalUser {
            id: user_id,
            account_id,
            password_hash: input.password_hash,
            email: input.email.clone(),
            email_verified: false,
            created_at_ms: input.now_ms,
            disabled: false,
            approved: input.approved,
            default_privacy: None,
            default_sensitive: None,
            default_language: None,
            default_quote_policy: None,
            home_mention_policy: None,
            chosen_languages: None,
            // New accounts start role-less; operators grant roles via the CLI.
            role: UserRole::None,
            // Signup-app attribution (D12 remainder; None for CLI/bootstrap).
            created_by_application: input.created_by_application,
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
            // Local accounts are found by actor URI too (Phase 6 federation).
            let mut by_actor = write.open_table(super::schema::ACCOUNT_BY_ACTOR_URI)?;
            by_actor.insert(account.actor_uri.as_str(), account_id.value())?;
        }
        {
            let mut users = write.open_table(LOCAL_USERS)?;
            users.insert(account_id.value(), encode(&user)?.as_slice())?;
        }
        // The email index shares the account-creation transaction, so a
        // committed account is always resolvable by email (D12 public
        // forgot-password). Duplicate addresses are refused like upstream
        // (`User` email uniqueness): the index read inside this transaction
        // is authoritative under the single writer. This is the only
        // email-write path in this file: `update_user_settings` never
        // touches the address.
        if let Some(email) = input.email.as_deref() {
            let normalized = normalize_email(email);
            if !normalized.is_empty() {
                let mut index = write.open_table(EMAIL_INDEX)?;
                if index.get(normalized.as_str())?.is_some() {
                    return Err(Error::Conflict(format!(
                        "email address is already registered: {email}"
                    )));
                }
                index.insert(normalized.as_str(), account_id.value())?;
            }
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

        // Public registration also commits its confirmation-mail job here, so a
        // committed account can never lose its verification message.
        if input.send_confirmation_email {
            if let Some(email) = input.email.as_deref() {
                let mail_job = crate::domain::Job::new(
                    self.next_id(),
                    crate::domain::JobKind::SendConfirmationEmail,
                    serde_json::json!({
                        "account_id": account_id.value(),
                        "user_id": user_id.value(),
                        "email": email,
                    }),
                    input.now_ms,
                    input.now_ms,
                );
                super::jobs::insert_job(&write, &mail_job)?;
            }
        }

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
        let read = self.begin_read()?;
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
        let read = self.begin_read()?;
        let accounts = read.open_table(ACCOUNTS)?;
        Ok(accounts.len()?)
    }

    fn accounts_by_ids(&self, ids: &[Id]) -> Result<HashMap<Id, Account>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let read = self.begin_read()?;
        let accounts = read.open_table(ACCOUNTS)?;
        let mut out: HashMap<Id, Account> = HashMap::with_capacity(ids.len());
        for id in ids {
            if out.contains_key(id) {
                continue;
            }
            if let Some(guard) = accounts.get(id.value())? {
                out.insert(*id, decode(guard.value())?);
            }
        }
        Ok(out)
    }

    fn update_account_profile(
        &self,
        account_id: Id,
        update: AccountProfileUpdate,
    ) -> Result<Account> {
        let write = self.begin_write()?;
        let mut account: Account = {
            let accounts = write.open_table(ACCOUNTS)?;
            match accounts.get(account_id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("account {account_id}"))),
            }
        };
        if let Some(v) = update.display_name {
            account.display_name = v;
        }
        if let Some(v) = update.note {
            account.note = v;
        }
        if let Some(v) = update.locked {
            account.locked = v;
        }
        // `discoverable`/`indexable`/`hide_collections` are accepted but not yet
        // persisted (no column in Phase 2); recorded as a gap.
        {
            let mut accounts = write.open_table(ACCOUNTS)?;
            accounts.insert(account_id.value(), encode(&account)?.as_slice())?;
        }
        write.commit()?;
        Ok(account)
    }

    fn update_user_settings(
        &self,
        account_id: Id,
        update: UserSettingsUpdate,
    ) -> Result<LocalUser> {
        let write = self.begin_write()?;
        let mut user: LocalUser = {
            let users = write.open_table(LOCAL_USERS)?;
            match users.get(account_id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("local user for {account_id}"))),
            }
        };
        if let Some(v) = update.default_privacy {
            user.default_privacy = Some(v);
        }
        if let Some(v) = update.default_sensitive {
            user.default_sensitive = Some(v);
        }
        if let Some(v) = update.default_language {
            user.default_language = Some(v);
        }
        if let Some(v) = update.default_quote_policy {
            user.default_quote_policy = Some(v);
        }
        if let Some(v) = update.home_mention_policy {
            user.home_mention_policy = Some(v);
        }
        if let Some(v) = update.chosen_languages {
            // An empty list clears the preference (upstream `compact_blank`
            // `.presence` normalization); otherwise store the normalized tags.
            let normalized =
                crate::application::policy::normalize_chosen_languages(&v);
            user.chosen_languages = if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            };
        }
        {
            let mut users = write.open_table(LOCAL_USERS)?;
            users.insert(account_id.value(), encode(&user)?.as_slice())?;
        }
        write.commit()?;
        Ok(user)
    }

    fn set_user_role(&self, account_id: Id, role: UserRole) -> Result<LocalUser> {
        let write = self.begin_write()?;
        let mut user: LocalUser = {
            let users = write.open_table(LOCAL_USERS)?;
            match users.get(account_id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("local user for {account_id}"))),
            }
        };
        user.role = role;
        {
            let mut users = write.open_table(LOCAL_USERS)?;
            users.insert(account_id.value(), encode(&user)?.as_slice())?;
        }
        write.commit()?;
        Ok(user)
    }

    fn record_sign_in(&self, account_id: Id, now_ms: u64) -> Result<()> {
        let write = self.begin_write()?;
        // Only real local users leave sign-in telemetry; a missing
        // credential row (or a remote id) records nothing, never an error.
        let users = write.open_table(LOCAL_USERS)?;
        if users.get(account_id.value())?.is_none() {
            return Ok(());
        }
        drop(users);
        let event = SignInEvent {
            id: self.next_id(),
            account_id,
            created_at_ms: now_ms,
        };
        {
            let mut table = write.open_table(super::schema::SIGN_INS)?;
            table.insert(event.id.value(), encode(&event)?.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn account_by_email(&self, normalized_email: &str) -> Result<Option<Account>> {
        let key = normalize_email(normalized_email);
        if key.is_empty() {
            return Ok(None);
        }
        let read = self.begin_read()?;
        let id = {
            let index = read.open_table(EMAIL_INDEX)?;
            match index.get(key.as_str())? {
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

    fn set_email_verified(&self, account_id: Id, verified: bool) -> Result<LocalUser> {
        let write = self.begin_write()?;
        let mut user: LocalUser = {
            let users = write.open_table(LOCAL_USERS)?;
            match users.get(account_id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("local user for {account_id}"))),
            }
        };
        user.email_verified = verified;
        {
            let mut users = write.open_table(LOCAL_USERS)?;
            users.insert(account_id.value(), encode(&user)?.as_slice())?;
        }
        write.commit()?;
        Ok(user)
    }

    fn store_email_token(&self, token: &str, record: EmailToken) -> Result<()> {
        if token.is_empty() {
            return Err(Error::InvalidInput("email token is empty".into()));
        }
        let write = self.begin_write()?;
        {
            let mut tokens = write.open_table(EMAIL_TOKENS)?;
            // Overwrite = idempotent: a retried registration job re-stores
            // the same deterministic token without error.
            tokens.insert(token, encode(&record)?.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn redeem_email_token(&self, token: &str) -> Result<Option<EmailToken>> {
        let write = self.begin_write()?;
        let record: Option<EmailToken> = {
            let tokens = write.open_table(EMAIL_TOKENS)?;
            match tokens.get(token)? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            }
        };
        if record.is_some() {
            let mut tokens = write.open_table(EMAIL_TOKENS)?;
            tokens.remove(token)?;
        }
        write.commit()?;
        Ok(record)
    }
}

/// Durable [`InviteStore`] on the `INVITES` table (D12 remainder; replaces
/// the in-memory backend for production).
///
/// Creation and redemption each commit in a single `redb` transaction:
/// redemption checks usability at `now_ms` and increments `uses` atomically,
/// so two concurrent redemptions serialize on the single writer instead of
/// double-counting one use.
impl InviteStore for RedbStore {
    fn create_invite(&self, invite: Invite) -> Result<Invite> {
        let write = self.begin_write()?;
        {
            let mut invites = write.open_table(INVITES)?;
            if invites.get(invite.code.as_str())?.is_some() {
                return Err(Error::Conflict("invite code is already taken".into()));
            }
            invites.insert(invite.code.as_str(), encode(&invite)?.as_slice())?;
        }
        write.commit()?;
        Ok(invite)
    }

    fn invite_by_code(&self, code: &str) -> Result<Option<Invite>> {
        let read = self.begin_read()?;
        let invites = read.open_table(INVITES)?;
        match invites.get(code)? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn redeem_invite(
        &self,
        code: &str,
        now_ms: u64,
    ) -> Result<Invite> {
        let write = self.begin_write()?;
        let mut invite: Invite = {
            let invites = write.open_table(INVITES)?;
            match invites.get(code)? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound("invite not found".into())),
            }
        };
        // Same mapping as the in-memory backend: a known-but-unusable code
        // is 401 `Invalid invitation code`, never a counter/expiry oracle.
        if invite.check_usable(now_ms).is_err() {
            return Err(Error::Unauthorized("Invalid invitation code".into()));
        }
        invite.uses = invite.uses.saturating_add(1);
        {
            let mut invites = write.open_table(INVITES)?;
            invites.insert(invite.code.as_str(), encode(&invite)?.as_slice())?;
        }
        write.commit()?;
        Ok(invite)
    }
}

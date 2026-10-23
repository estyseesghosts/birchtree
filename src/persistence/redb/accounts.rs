//! Account and local-credential persistence.

use std::collections::HashMap;

use redb::ReadableTable;

use crate::application::accounts::InviteStore;
use crate::application::ports::{
    AccountProfileUpdate, AccountStore, AdminAccountFilter, AdminStore, NewLocalAccount,
    UserSettingsUpdate,
};
use crate::domain::user::{EmailToken, EmailTokenKind, Invite};
use crate::domain::{Account, AccountStats, Error, Id, LocalUser, Result, SignInEvent, UserRole};

use super::schema::{
    self, ACCOUNTS, ACCOUNT_STATS, EMAIL_INDEX, EMAIL_TOKENS, HANDLE_INDEX, INVITES, LOCAL_USERS,
    META,
};
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


/// Re-key legacy `EMAIL_TOKENS` rows (keyed by the plaintext bearer) by
/// digest, once. Idempotent: the marker row short-circuits later calls, and
/// digest keys carry [`schema::EMAIL_TOKEN_KEY_PREFIX`], which no hex bearer
/// can match, so a half-applied pass can never be mistaken for legacy data.
pub(super) fn ensure_email_tokens_keyed(write: &redb::WriteTransaction) -> StoreResult<()> {
    {
        let meta = write.open_table(META)?;
        if meta.get(schema::EMAIL_TOKEN_DIGEST_MARKER_KEY)?.is_some() {
            return Ok(());
        }
    }
    let mut legacy: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let tokens = write.open_table(EMAIL_TOKENS)?;
        for row in tokens.iter()? {
            let (key, value) = row?;
            if !key.value().starts_with(schema::EMAIL_TOKEN_KEY_PREFIX) {
                legacy.push((key.value().to_string(), value.value().to_vec()));
            }
        }
    }
    {
        let mut tokens = write.open_table(EMAIL_TOKENS)?;
        for (plaintext, value) in &legacy {
            tokens.remove(plaintext.as_str())?;
            tokens.insert(schema::email_token_key(plaintext).as_str(), value.as_slice())?;
        }
    }
    let mut meta = write.open_table(META)?;
    meta.insert(schema::EMAIL_TOKEN_DIGEST_MARKER_KEY, [1u8].as_slice())?;
    Ok(())
}

/// Persist a bearer's digest (never the bearer) inside `write`.
pub(super) fn insert_email_token(
    write: &redb::WriteTransaction,
    token: &str,
    record: &EmailToken,
) -> StoreResult<()> {
    let mut tokens = write.open_table(EMAIL_TOKENS)?;
    tokens.insert(schema::email_token_key(token).as_str(), encode(record)?.as_slice())?;
    Ok(())
}

/// Remove and return a bearer's record inside `write` (single use).
pub(super) fn take_email_token(
    write: &redb::WriteTransaction,
    token: &str,
) -> StoreResult<Option<EmailToken>> {
    let mut tokens = write.open_table(EMAIL_TOKENS)?;
    let removed = tokens.remove(schema::email_token_key(token).as_str())?;
    match removed {
        Some(guard) => Ok(Some(decode(guard.value())?)),
        None => Ok(None),
    }
}

/// Which mail job a bearer kind queues.
pub(super) fn mail_job_kind(kind: EmailTokenKind) -> crate::domain::JobKind {
    match kind {
        EmailTokenKind::Confirmation => crate::domain::JobKind::SendConfirmationEmail,
        EmailTokenKind::PasswordReset => crate::domain::JobKind::SendPasswordResetEmail,
    }
}

impl RedbStore {
    /// Context string binding a sealed mail bearer to its account.
    fn mail_token_context(account_id: Id) -> Vec<u8> {
        format!("mail-token:{}", account_id.value()).into_bytes()
    }

    /// The value stored in a mail job's `token` field: sealed under the
    /// installed key, or the bare bearer when no key is configured.
    pub(super) fn protect_mail_token(&self, account_id: Id, token: &str) -> Result<String> {
        Ok(self
            .secret_box()
            .seal_text_if_enabled(&Self::mail_token_context(account_id), token)?)
    }

    /// Inverse of [`Self::protect_mail_token`]; fails closed when the value
    /// is sealed and the key is missing or wrong.
    pub(super) fn recover_mail_token(&self, account_id: Id, stored: &str) -> Result<String> {
        Ok(self
            .secret_box()
            .open_text(&Self::mail_token_context(account_id), stored)?
            .text)
    }
}

impl AccountStore for RedbStore {
    // WHY single-txn: one begin_write owns the account row + credential rows + counters atomically
    // (architecture rule); splitting would move work across the commit boundary.
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
            bot: false,
            discoverable: None,
            indexable: false,
            hide_collections: None,
            fields: Vec::new(),
            attribution_domains: Vec::new(),
            avatar_key: None,
            avatar_content_type: None,
            avatar_description: String::new(),
            header_key: None,
            header_content_type: None,
            header_description: String::new(),
            default_license: None,
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
            // Registration captures (fix round P1/P3): locale, invite-request
            // text and consumed invite code (INVITE_USES, same transaction).
            locale: input.locale,
            invite_request: input.invite_request,
            // G-E registration captures: IANA time zone + date of birth
            // (validated at the application layer; `None` for pre-batch rows).
            time_zone: input.time_zone,
            date_of_birth: input.date_of_birth,
            // Reading preferences start unset (upstream defaults apply).
            web_display_media: None,
            web_expand_spoilers: None,
            web_auto_play_gifs: None,
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

        // Invite attribution shares the account-creation transaction: the
        // consumed invite code is recorded under the new account id (backs
        // `invited_by_account_id` + the v2 `invited_by` filter). Only a
        // usable invite's code reaches `NewLocalAccount::invite_code` (the
        // application layer clears unusable codes), so every row here is a
        // genuine redemption.
        if let Some(code) = input.invite_code.as_deref() {
            if !code.trim().is_empty() {
                let mut uses = write.open_table(super::schema::INVITE_USES)?;
                uses.insert(account_id.value(), code)?;
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
                // Random bearer minted before the job exists: only its digest
                // is persisted; the job carries it sealed for delivery retries.
                let token: String = crate::application::crypto::random_token_hex(32);
                insert_email_token(
                    &write,
                    &token,
                    &EmailToken {
                        account_id,
                        kind: EmailTokenKind::Confirmation,
                        created_at_ms: input.now_ms,
                    },
                )?;
                let mail_job = crate::domain::Job::new(
                    self.next_id(),
                    crate::domain::JobKind::SendConfirmationEmail,
                    serde_json::json!({
                        "account_id": account_id.value(),
                        "user_id": user_id.value(),
                        "email": email,
                        "token": self.protect_mail_token(account_id, &token)?,
                    }),
                    input.now_ms,
                    input.now_ms,
                );
                super::jobs::insert_job(&write, &mail_job)?;
            }
        }

        // D51 `admin.sign_up` to admins, same transaction (G-B producer).
        // Fires for every created account incl. operator-created ones (an
        // account WAS created; honest).
        {
            use crate::application::notifications::emit_admin_sign_up;
            let staff = self.admin_accounts_page(
                &AdminAccountFilter {
                    staff_only: true,
                    ..Default::default()
                },
                None,
                None,
                None,
                false,
                5000,
            )?;
            let admins: Vec<Id> = staff
                .into_iter()
                .filter(|(_, user)| {
                    user.as_ref()
                        .is_some_and(|u| u.role == crate::domain::UserRole::Admin)
                })
                .map(|(account, _)| account.id)
                .collect();
            for job in emit_admin_sign_up(account_id, &admins, input.now_ms) {
                super::jobs::insert_job(
                    &write,
                    &crate::domain::Job::new(
                        self.next_id(),
                        job.kind,
                        job.payload,
                        input.now_ms,
                        input.now_ms,
                    ),
                )?;
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
        now_ms: u64,
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
        if let Some(v) = update.discoverable {
            account.discoverable = Some(v);
        }
        if let Some(v) = update.indexable {
            account.indexable = v;
        }
        if let Some(v) = update.hide_collections {
            account.hide_collections = Some(v);
        }
        if let Some(v) = update.fields {
            account.fields = v;
        }
        if let Some(v) = update.attribution_domains {
            account.attribution_domains = v;
        }
        if let Some(v) = update.bot {
            account.bot = v;
        }
        // Avatar/header slots share the rest's transaction: the bytes were
        // staged before this call (blob storage cannot join `redb`), so only
        // the references commit here.
        if let Some(slot) = update.avatar {
            match slot {
                Some(data) => {
                    account.avatar_key = Some(data.key);
                    account.avatar_content_type = Some(data.content_type);
                }
                None => {
                    account.avatar_key = None;
                    account.avatar_content_type = None;
                }
            }
        }
        if let Some(description) = update.avatar_description {
            account.avatar_description = description;
        }
        if let Some(slot) = update.header {
            match slot {
                Some(data) => {
                    account.header_key = Some(data.key);
                    account.header_content_type = Some(data.content_type);
                }
                None => {
                    account.header_key = None;
                    account.header_content_type = None;
                }
            }
        }
        if let Some(description) = update.header_description {
            account.header_description = description;
        }
        if let Some(license) = update.default_license {
            account.default_license = license;
        }
        {
            let mut accounts = write.open_table(ACCOUNTS)?;
            accounts.insert(account_id.value(), encode(&account)?.as_slice())?;
        }
        // Every profile write fans a profile `Update` to the account's
        // followers (upstream `UpdateDistributionWorker`, debounced there;
        // immediate here — documented in the federation contract). The no-op
        // guard inside keeps remote rows silent.
        super::federation_content::federation_event_txn(
            &write,
            self.next_id(),
            account_id,
            serde_json::json!({"event": "profile_update"}),
            now_ms,
        )?;
        write.commit()?;
        Ok(account)
    }

    /// Record the account this one moved to (federation `Move`, G-D parent
    /// merge of S4). Remote rows included. ONE transaction; `None` clears.
    /// No fan-out: the move itself already fanned out through the migrated
    /// follows.
    fn set_moved_to(&self, account_id: Id, moved_to: Option<Id>, _now_ms: u64) -> Result<Account> {
        let write = self.begin_write()?;
        let mut account: Account = {
            let accounts = write.open_table(ACCOUNTS)?;
            match accounts.get(account_id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("account {account_id}"))),
            }
        };
        account.moved_to = moved_to;
        {
            let mut accounts = write.open_table(ACCOUNTS)?;
            accounts.insert(account_id.value(), encode(&account)?.as_slice())?;
        }
        write.commit()?;
        Ok(account)
    }

    fn set_remote_stats(
        &self,
        account_id: Id,
        followers: Option<u64>,
        following: Option<u64>,
        statuses: Option<u64>,
    ) -> Result<AccountStats> {
        let write = self.begin_write()?;
        let mut stats = {
            let table = write.open_table(ACCOUNT_STATS)?;
            match table.get(account_id.value())? {
                Some(guard) => decode(guard.value())?,
                None => AccountStats::default(),
            }
        };
        if let Some(v) = followers {
            stats.followers_count = v;
        }
        if let Some(v) = following {
            stats.following_count = v;
        }
        if let Some(v) = statuses {
            stats.statuses_count = v;
        }
        {
            let mut table = write.open_table(ACCOUNT_STATS)?;
            table.insert(account_id.value(), encode(&stats)?.as_slice())?;
        }
        write.commit()?;
        Ok(stats)
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
        if let Some(v) = update.web_display_media {
            // Upstream `user_settings.rb:38` enum; unknown values are
            // refused at the endpoint, so anything reaching here stores.
            user.web_display_media = Some(v);
        }
        if let Some(v) = update.web_expand_spoilers {
            user.web_expand_spoilers = Some(v);
        }
        if let Some(v) = update.web_auto_play_gifs {
            user.web_auto_play_gifs = Some(v);
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

    fn set_user_password(&self, account_id: Id, password_hash: String) -> Result<LocalUser> {
        if password_hash.is_empty() {
            return Err(Error::InvalidInput("password hash is empty".into()));
        }
        let write = self.begin_write()?;
        let mut user: LocalUser = {
            let users = write.open_table(LOCAL_USERS)?;
            match users.get(account_id.value())? {
                Some(guard) => decode(guard.value())?,
                None => return Err(Error::NotFound(format!("local user for {account_id}"))),
            }
        };
        user.password_hash = password_hash;
        {
            let mut users = write.open_table(LOCAL_USERS)?;
            users.insert(account_id.value(), encode(&user)?.as_slice())?;
        }
        write.commit()?;
        Ok(user)
    }

    fn record_sign_in(
        &self,
        account_id: Id,
        now_ms: u64,
        ip: Option<std::net::IpAddr>,
    ) -> Result<()> {
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
            ip,
        };
        {
            let mut table = write.open_table(super::schema::SIGN_INS)?;
            table.insert(event.id.value(), encode(&event)?.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    fn invite_code_for_account(&self, account_id: Id) -> Result<Option<String>> {
        let read = self.begin_read()?;
        let table = read.open_table(super::schema::INVITE_USES)?;
        match table.get(account_id.value())? {
            Some(guard) => Ok(Some(guard.value().to_string())),
            None => Ok(None),
        }
    }

    fn accounts_for_inviter(&self, inviter_id: Id, cap: usize) -> Result<Vec<Id>> {
        // INVITE_USES is keyed by created account; the scan is bounded and
        // the invite lookup resolves the issuing account per row.
        let read = self.begin_read()?;
        let uses = read.open_table(super::schema::INVITE_USES)?;
        let invites = read.open_table(super::schema::INVITES)?;
        let mut out = Vec::new();
        for entry in uses.range(0u64..=u64::MAX)? {
            let (account_key, code_guard) = entry?;
            let code = code_guard.value();
            let matched = match invites.get(code)? {
                Some(guard) => {
                    let invite: Invite = decode(guard.value())?;
                    invite.created_by_account_id == inviter_id
                }
                None => false,
            };
            if matched {
                out.push(Id::new(account_key.value()));
                if out.len() >= cap {
                    break;
                }
            }
        }
        Ok(out)
    }

    fn login_ips_for(
        &self,
        account_id: Id,
        cap: usize,
    ) -> Result<Vec<(std::net::IpAddr, u64)>> {
        // SIGN_INS is keyed by event id (timestamp-ordered); the scan is
        // bounded and newest-first so the latest login wins `ip`.
        let read = self.begin_read()?;
        let table = read.open_table(super::schema::SIGN_INS)?;
        let mut out = Vec::new();
        let mut events: Vec<SignInEvent> = Vec::new();
        for entry in table.range(0u64..=u64::MAX)? {
            let (_, value) = entry?;
            let event: SignInEvent = decode(value.value())?;
            if event.account_id == account_id {
                events.push(event);
            }
            if events.len() >= cap {
                break;
            }
        }
        events.sort_by_key(|event| std::cmp::Reverse(event.created_at_ms));
        for event in events {
            if let Some(ip) = event.ip {
                out.push((ip, event.created_at_ms));
                if out.len() >= cap {
                    break;
                }
            }
        }
        Ok(out)
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
        ensure_email_tokens_keyed(&write)?;
        // Overwrite = idempotent for a re-stored bearer.
        insert_email_token(&write, token, &record)?;
        write.commit()?;
        Ok(())
    }

    fn redeem_email_token(&self, token: &str) -> Result<Option<EmailToken>> {
        let write = self.begin_write()?;
        ensure_email_tokens_keyed(&write)?;
        let record: Option<EmailToken> = take_email_token(&write, token)?;
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

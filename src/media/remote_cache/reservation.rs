//! The fetch reservation guard: holds class budget across one download (S12
//! file-size split of `super`). The guard borrows the process-wide ledger;
//! dropping it releases whatever was not committed, so every return path in
//! the fetch frees its holding without manual releases.

use std::sync::{Mutex, OnceLock};

use super::{CacheError, CacheLedger, MediaClass, RemoteCacheConfig, RemoteCacheEntry};

/// Holds `class` budget across one fetch: worst-case reservation up front,
/// shrink once `Content-Length` is known, settle into stored bytes on
/// success. Dropping frees whatever was not committed.
#[must_use]
#[derive(Debug)]
pub struct Reservation<'a> {
    ledger: &'a Mutex<CacheLedger>,
    class: MediaClass,
    remaining: u64,
}

impl<'a> Reservation<'a> {
    /// Hold `bytes` of `class` budget (same refusals as
    /// [`CacheLedger::reserve`], plus `Store` when the ledger is poisoned).
    pub fn reserve(
        ledger: &'a Mutex<CacheLedger>,
        class: MediaClass,
        bytes: u64,
    ) -> Result<Self, CacheError> {
        ledger
            .lock()
            .map_err(|_| CacheError::Store("cache ledger poisoned".into()))?
            .reserve(class, bytes)?;
        Ok(Self {
            ledger,
            class,
            remaining: bytes,
        })
    }

    /// The class this reservation holds budget for.
    pub fn class(&self) -> MediaClass {
        self.class
    }

    /// Budget still held (the live stream cap; shrinks with `Content-Length`).
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Release `amount` of the held budget (clamped to what is held), e.g.
    /// shrinking a worst-case reservation once `Content-Length` is known. A
    /// poisoned ledger keeps the holding: the drop retries the release.
    pub fn shrink(&mut self, amount: u64) {
        let take = amount.min(self.remaining);
        if take == 0 {
            return;
        }
        if let Ok(mut ledger) = self.ledger.lock() {
            ledger.release(self.class, take);
            self.remaining -= take;
        }
    }

    /// Hold `extra` more budget (the N2 top-up when staged bytes exceed the
    /// shrunk reservation). On refusal nothing is held and the guard is
    /// unchanged.
    pub fn grow(&mut self, extra: u64) -> Result<(), CacheError> {
        if extra == 0 {
            return Ok(());
        }
        self.ledger
            .lock()
            .map_err(|_| CacheError::Store("cache ledger poisoned".into()))?
            .reserve(self.class, extra)?;
        self.remaining = self.remaining.saturating_add(extra);
        Ok(())
    }

    /// Settle `reserved` of the held budget into `entry`'s stored bytes. The
    /// held budget is consumed whether the commit lands or is refused (the
    /// ledger releases the reservation on every commit path), so the guard
    /// never releases twice.
    pub fn commit_entry(
        &mut self,
        reserved: u64,
        entry: RemoteCacheEntry,
    ) -> Result<(), CacheError> {
        let take = reserved.min(self.remaining);
        let outcome = self
            .ledger
            .lock()
            .map_err(|_| CacheError::Store("cache ledger poisoned".into()))?
            .commit(self.class, take, entry);
        self.remaining -= take;
        outcome
    }

    /// Drop the ledger mapping for `key` without touching the budget (the
    /// row-gone unwind after a settled commit).
    pub fn evict_key(&self, key: &[u8]) {
        if let Ok(mut ledger) = self.ledger.lock() {
            ledger.evict_one(key);
        }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.remaining == 0 {
            return;
        }
        if let Ok(mut ledger) = self.ledger.lock() {
            ledger.release(self.class, self.remaining);
        }
    }
}

/// Process-wide admission ledger for the fetch path. Budgets come from
/// operator config via [configure_process_ledger] (parent wires it in
/// main.rs); until then the plan defaults apply.
static PROCESS_LEDGER: OnceLock<Mutex<CacheLedger>> = OnceLock::new();

/// The process-wide cache ledger.
pub fn process_ledger() -> &'static Mutex<CacheLedger> {
    PROCESS_LEDGER.get_or_init(|| Mutex::new(CacheLedger::new(RemoteCacheConfig::default())))
}

/// Install operator budgets into the process ledger (idempotent).
pub fn configure_process_ledger(cfg: RemoteCacheConfig) {
    if let Ok(mut ledger) = process_ledger().lock() {
        ledger.set_caps(cfg);
    }
}

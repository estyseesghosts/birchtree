//! Core extension CORE-L: media/account license ids (Stage 1).
//!
//! The license enum mirrors Pixelfed's attachment license ids
//! (`1, 5, 6, 11-16`); absent/`NULL` means all rights reserved.
//! `MediaAttachment.license` and `Account.default_license` ride inline in
//! the existing `media`/`accounts` JSON blobs (schema v17, serde default
//! `None`), so pre-v17 rows decode as `NULL` with no backfill.
//!
//! Policy gating (`off`/`unenforced`/`enforced`) lives in
//! `application::license_policy`; this module only validates raw values.
//! [`parse_license`] is policy-independent: an out-of-enum value is always
//! a 422 (`InvalidInput`), while an in-enum value under `off` is accepted
//! and then discarded (stored `NULL`) by the caller.

use crate::domain::{Error, Result};

/// Valid license ids (Pixelfed ids; `None`/absent = all rights reserved).
pub const VALID_LICENSES: [u8; 9] = [1, 5, 6, 11, 12, 13, 14, 15, 16];

/// 422 body for an out-of-enum license value (Mastodon `RecordInvalid` shape).
pub const ERR_LICENSE_INVALID: &str = "Validation failed: License is invalid";

/// A validated license id (one of [`VALID_LICENSES`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct LicenseId(pub u8);

impl LicenseId {
    /// Validate a raw id; out-of-enum values are [`Error::InvalidInput`].
    pub fn new(id: u8) -> Result<Self> {
        if is_valid_license(id) {
            Ok(Self(id))
        } else {
            Err(Error::InvalidInput(ERR_LICENSE_INVALID.into()))
        }
    }

    /// The raw id.
    pub fn value(self) -> u8 {
        self.0
    }
}

/// True when `id` is one of [`VALID_LICENSES`].
pub fn is_valid_license(id: u8) -> bool {
    VALID_LICENSES.contains(&id)
}

/// Parse a raw wire/store license value: `None` (absent/`NULL`) stays
/// `None` (all rights reserved); an in-enum integer validates; anything
/// else (out-of-enum, negative, overflow) is [`Error::InvalidInput`].
pub fn parse_license(raw: Option<i64>) -> Result<Option<u8>> {
    match raw {
        None => Ok(None),
        Some(value) => {
            if value < 1 || value > u8::MAX as i64 {
                return Err(Error::InvalidInput(ERR_LICENSE_INVALID.into()));
            }
            let id = value as u8;
            if is_valid_license(id) {
                Ok(Some(id))
            } else {
                Err(Error::InvalidInput(ERR_LICENSE_INVALID.into()))
            }
        }
    }
}

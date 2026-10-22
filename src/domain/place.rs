//! Places (core extension, Stage 1 CORE-P).
//!
//! A `Place` is a named geographic point an author may attach to a status
//! via `place_id`. Places ride inline in the status JSON blob as
//! `Status.place_id` (proposed shared-file field); the `PLACES` /
//! `PLACES_BY_NAME` tables (schema v18) are the durable registry.
//!
//! This module is pure domain: no HTTP DTOs, no ActivityPub JSON, no `redb`.
//! Wire-up is proposed (not applied): add `pub mod place;` to
//! `src/domain/mod.rs` plus `pub use place::{Place, ...};`.

use serde::{Deserialize, Serialize};

use crate::domain::{Error, Id, Result};

/// Maximum place-name length in Unicode scalar values.
pub const MAX_PLACE_NAME_CHARS: usize = 256;

/// A named geographic point attachable to a status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: Id,
    pub name: String,
    /// Longitude in degrees (-180..=180).
    pub lon: f64,
    /// Latitude in degrees (-90..=90).
    pub lat: f64,
    /// ISO / free-text country label, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

impl Place {
    /// Validate every field (name + coordinates).
    pub fn validate(&self) -> Result<()> {
        validate_place_name(&self.name)?;
        validate_place_coords(self.lon, self.lat)?;
        Ok(())
    }
}

/// A place name must be non-empty (after trimming) and at most
/// [`MAX_PLACE_NAME_CHARS`] characters.
pub fn validate_place_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::InvalidInput(
            "Validation failed: Place name can't be blank".into(),
        ));
    }
    if name.chars().count() > MAX_PLACE_NAME_CHARS {
        return Err(Error::InvalidInput(format!(
            "Validation failed: Place name is too long (maximum is {MAX_PLACE_NAME_CHARS} characters)"
        )));
    }
    Ok(())
}

/// Longitude must lie in -180..=180 and latitude in -90..=90; both must be
/// finite (no NaN/infinities, which JSON cannot round-trip).
pub fn validate_place_coords(lon: f64, lat: f64) -> Result<()> {
    if !lon.is_finite() || !lat.is_finite() {
        return Err(Error::InvalidInput(
            "Validation failed: Place coordinates must be finite numbers".into(),
        ));
    }
    if !(lon >= -180.0 && lon <= 180.0) {
        return Err(Error::InvalidInput(
            "Validation failed: Longitude must be between -180 and 180".into(),
        ));
    }
    if !(lat >= -90.0 && lat <= 90.0) {
        return Err(Error::InvalidInput(
            "Validation failed: Latitude must be between -90 and 90".into(),
        ));
    }
    Ok(())
}

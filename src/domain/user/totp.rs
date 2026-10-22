use crate::domain::Id;

// ---------------------------------------------------------------------------
// TOTP second factor (+ single-use hashed backup codes).
// ---------------------------------------------------------------------------

/// TOTP time-step in seconds (RFC 6238 §4, upstream `rotp` default).
pub const TOTP_STEP_SECS: u64 = 30;
/// TOTP code length in digits (RFC 4226 `Truncate`, upstream default 6).
pub const TOTP_DIGITS: usize = 6;
/// Verification window in steps on either side of the current counter
/// (upstream `verify_with_drift_and_prior`, ±30 s clock skew).
pub const TOTP_WINDOW_STEPS: u64 = 1;
/// Raw secret length in bytes (160 bits, RFC 4226 §4 minimum recommendation).
pub const TOTP_SECRET_LEN: usize = 20;

/// A TOTP second-factor enrollment (the `TOTP_SECRETS` table row, keyed by
/// account id; JSON shape).
///
/// The secret is the live credential: this struct has a hand-written
/// [`std::fmt::Debug`] that redacts the secret and the backup-code hashes,
/// and no code path may log either (see `tests/secret_handling.rs`
/// conventions). Backup codes are stored only as SHA-256 hex digests and
/// each entry is removed on first use (single-use).
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TotpRecord {
    pub account_id: Id,
    /// Base32 (RFC 4648, no padding) encoding of the raw HMAC-SHA1 secret.
    pub secret_base32: String,
    pub created_at_ms: u64,
    /// False until the owner proves possession with a live code (the
    /// `confirm` step). Unconfirmed enrollments never authenticate.
    pub confirmed: bool,
    /// SHA-256 hex digests of the unused backup codes.
    pub backup_hashes: Vec<String>,
    /// Last accepted TOTP counter (replay protection: a code's counter must
    /// strictly increase).
    pub last_counter: u64,
}

impl std::fmt::Debug for TotpRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TotpRecord")
            .field("account_id", &self.account_id)
            .field("secret_base32", &"[redacted]")
            .field("created_at_ms", &self.created_at_ms)
            .field("confirmed", &self.confirmed)
            .field("backup_hashes", &"[redacted]")
            .field("last_counter", &self.last_counter)
            .finish()
    }
}

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 encoding (uppercase, no padding).
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for &byte in bytes {
        acc = (acc << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32_ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32_ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// RFC 4648 base32 decoding (uppercase or lowercase, optional `=` padding).
/// Whitespace is rejected; unknown characters are an error, never skipped.
pub fn base32_decode(text: &str) -> crate::domain::Result<Vec<u8>> {
    use crate::domain::Error;
    let invalid = || Error::InvalidInput("TOTP secret is not valid base32".into());
    let mut stripped = text.trim().to_ascii_uppercase();
    while stripped.ends_with('=') {
        stripped.pop();
    }
    if stripped.is_empty() || stripped.len() % 8 == 1 {
        return Err(invalid());
    }
    let mut out = Vec::with_capacity(stripped.len() * 5 / 8);
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for byte in stripped.bytes() {
        let value = BASE32_ALPHABET
            .iter()
            .position(|&c| c == byte)
            .ok_or_else(invalid)? as u32;
        acc = (acc << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// TOTP counter for `now_ms` (`T - T0 / X` with `T0 = 0`, RFC 6238 §4).
pub fn totp_counter_at(now_ms: u64) -> u64 {
    now_ms / 1000 / TOTP_STEP_SECS
}

/// HOTP value (RFC 4226 §5.3–§5.4): HMAC-SHA1, dynamic truncation, mod 10⁶.
///
/// SHA-1 is mandated here by RFC 6238 (and by every authenticator app);
/// HMAC-SHA1's security does not rely on collision resistance, so the
/// legacy-use algorithm is the correct primitive — not a downgrade.
fn hotp(secret: &[u8], counter: u64) -> u32 {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = ring::hmac::sign(&key, &counter.to_be_bytes());
    let bytes = tag.as_ref();
    let offset = (bytes[bytes.len() - 1] & 0x0f) as usize;
    let code = ((bytes[offset] & 0x7f) as u32) << 24
        | (bytes[offset + 1] as u32) << 16
        | (bytes[offset + 2] as u32) << 8
        | bytes[offset + 3] as u32;
    code % 1_000_000
}

/// The 6-digit code for `secret` at `counter` (zero-padded).
pub fn totp_code_at(secret: &[u8], counter: u64) -> String {
    format!("{:06}", hotp(secret, counter))
}

/// Constant-time byte comparison for secrets (codes, hashes, challenges).
/// Lengths must match first — every compared value here is fixed-shape, so
/// the length check leaks nothing. (`ring::constant_time` is internal-only
/// in this ring version, so the loop below is the primitive; `black_box`
/// keeps the accumulator from being optimized into an early exit.)
pub(crate) fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// Constant-time code comparison (fixed 6-digit shape enforced first; the
/// length check leaks nothing — every real code is 6 chars).
fn totp_codes_equal(a: &str, b: &str) -> bool {
    if a.len() != TOTP_DIGITS || b.len() != TOTP_DIGITS {
        return false;
    }
    constant_time_equal(a.as_bytes(), b.as_bytes())
}

/// Verify a presented TOTP `code` at `now_ms`.
///
/// The code must be 6 ASCII digits matching a counter in
/// `[now - window, now + window]` that strictly exceeds `last_counter`
/// (replay protection). Returns the matched counter for the caller to persist
/// as the new `last_counter`, or `None` when the code is wrong, malformed,
/// stale (replay), or outside the window. Every candidate is evaluated in
/// constant time; unknown-vs-wrong is indistinguishable by construction.
pub fn verify_totp_at(secret: &[u8], code: &str, now_ms: u64, last_counter: u64) -> Option<u64> {
    if code.len() != TOTP_DIGITS || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = totp_counter_at(now_ms);
    let low = now.saturating_sub(TOTP_WINDOW_STEPS);
    let high = now.saturating_add(TOTP_WINDOW_STEPS);
    let mut matched: Option<u64> = None;
    for counter in low..=high {
        if counter <= last_counter {
            continue;
        }
        if totp_codes_equal(&totp_code_at(secret, counter), code) {
            matched = Some(counter);
        }
    }
    matched
}

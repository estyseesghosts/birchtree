//! Web Push `aes128gcm` content encryption (RFC 8291), `ring`-only.
//!
//! Upstream reference: Mastodon v4.7.3 (`8ff11c2b`)
//! `app/workers/web/push_notification_worker.rb` (`perform_standard_request`
//! sends `Content-Encoding: aes128gcm`) with the JSON of
//! `Web::NotificationSerializer` as the plaintext
//! (`push_notification_json`). The Ruby `webpush` gem's
//! `Webpush::Encryption.encrypt(payload, p256dh, auth)` is the exact
//! construction mirrored here.
//!
//! Construction (RFC 8291 §3–§4), using only the already-vendored `ring`
//! (no new dependency):
//!
//! - ECDH P-256 between a fresh ephemeral sender key and the subscription's
//!   `p256dh` (receiver) key → shared secret `Z`.
//! - Combined HKDF (§3.4 pseudocode): `PRK_key = HMAC(auth, Z)`,
//!   `IKM = HMAC(PRK_key, "WebPush: info" || 0x00 || rx || tx || 0x01)[..32]`,
//!   then `PRK = HMAC(header_salt, IKM)`,
//!   `CEK = HMAC(PRK, "Content-Encoding: aes128gcm" || 0x00 || 0x01)[..16]`,
//!   `NONCE = HMAC(PRK, "Content-Encoding: nonce" || 0x00 || 0x01)[..12]`.
//!   The per-message header salt IS a KDF input (second stage). This matches
//!   `mastodon/webpush@9631ac6` (`lib/webpush/encryption.rb`, the exact
//!   construction Mastodon v4.7.3 calls) and reproduces the RFC 8291
//!   Appendix A intermediates byte-for-byte (locked by
//!   `rfc8291_appendix_a_intermediates` in `tests/round3_notif_push.rs`).
//! - Record plaintext = `data || 0x02` (final-record delimiter, RFC 8188
//!   §2; no extra padding), sealed with AES-128-GCM under `nonce`.
//! - Body = `salt(16) || rs_be(4) || idlen(1)=65 || sender_pub(65) ||
//!   ciphertext`, with `rs = 4096`.
//!
//! The round-trip test in `tests/round3_notif_push.rs` re-derives the
//! receiver side independently (fresh DH from the receiver key + header
//! parse + AES-GCM open); delivery against a live push service is still
//! pending lab verification, like all network paths.
//!
//! No key material ever appears in an error string — only lengths and roles.

use ring::{aead, agreement, hkdf, rand};
use ring::rand::SecureRandom;

/// `Content-Encoding` value for the standard path.
pub const CONTENT_ENCODING_AES128GCM: &str = "aes128gcm";

/// Record size (`rs`) carried in the header. Single-record messages only:
/// the final record may be shorter than `rs` (RFC 8188 §2), so small
/// notification payloads stay small on the wire.
pub const RECORD_SIZE: u32 = 4096;

/// Max plaintext bytes: `rs - tag(16) - delimiter(1)`.
pub const MAX_PLAINTEXT_LEN: usize = RECORD_SIZE as usize - 16 - 1;

/// Lengths on the wire.
pub const SALT_LEN: usize = 16;
pub const CEK_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const UNCOMPRESSED_POINT_LEN: usize = 65;
pub const AUTH_SECRET_LEN: usize = 16;

/// One encrypted push body plus the headers the delivery needs.
#[derive(Debug, Clone)]
pub struct EncryptedPushPayload {
    /// Full request body (header + ciphertext).
    pub body: Vec<u8>,
    /// `Content-Encoding: aes128gcm`.
    pub content_encoding: &'static str,
    /// The 16-byte salt (also embedded in `body`; exposed for logging-free
    /// diagnostics by length only — never logged itself).
    pub salt_len: usize,
}

/// Encrypt `plaintext` for the subscription keys (`p256dh` / `auth` as
/// base64url-no-pad, exactly as stored on the subscription row).
///
/// Errors name only the failing role (`p256dh`, `auth`) and the expected
/// length — never key bytes. Oversize plaintext is refused with its sizes.
pub fn encrypt_aes128gcm(
    key_p256dh_b64: &str,
    key_auth_b64: &str,
    plaintext: &[u8],
) -> Result<EncryptedPushPayload, String> {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let receiver_pub = enc
        .decode(key_p256dh_b64.trim())
        .map_err(|err| format!("push p256dh key: base64url decode failed: {err}"))?;
    if receiver_pub.len() != UNCOMPRESSED_POINT_LEN || receiver_pub[0] != 0x04 {
        return Err(format!(
            "push p256dh key: must decode to a 65-byte uncompressed point, got {} bytes",
            receiver_pub.len()
        ));
    }
    let auth_secret = enc
        .decode(key_auth_b64.trim())
        .map_err(|err| format!("push auth secret: base64url decode failed: {err}"))?;
    if auth_secret.len() != AUTH_SECRET_LEN {
        return Err(format!(
            "push auth secret: must decode to 16 bytes, got {} bytes",
            auth_secret.len()
        ));
    }
    if plaintext.len() > MAX_PLAINTEXT_LEN {
        return Err(format!(
            "push payload: {} plaintext bytes exceed the {MAX_PLAINTEXT_LEN}-byte single-record limit",
            plaintext.len()
        ));
    }
    let rng = rand::SystemRandom::new();
    let sender_private =
        agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
            .map_err(|_| "push encrypt: ephemeral key generation failed".to_string())?;
    let sender_pub = sender_private
        .compute_public_key()
        .map_err(|_| "push encrypt: ephemeral public key failed".to_string())?;
    let sender_pub_bytes: &[u8] = sender_pub.as_ref();
    let mut sender_point = [0u8; UNCOMPRESSED_POINT_LEN];
    if sender_pub_bytes.len() != UNCOMPRESSED_POINT_LEN {
        return Err("push encrypt: ephemeral point has an unexpected length".to_string());
    }
    sender_point.copy_from_slice(sender_pub_bytes);
    let mut receiver_point = [0u8; UNCOMPRESSED_POINT_LEN];
    receiver_point.copy_from_slice(&receiver_pub);

    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &receiver_pub);
    // Per-message header salt (RFC 8291 §3): fresh randomness per message,
    // and a KDF input to the second HKDF stage (§3.4) — derived before the
    // agreement closure runs so sender and receiver share it via the header.
    let mut salt = [0u8; SALT_LEN];
    rng.fill(&mut salt)
        .map_err(|_| "push encrypt: salt generation failed".to_string())?;
    let body = agreement::agree_ephemeral(sender_private, &peer, |shared_secret| {
        encrypt_with_secret(&salt, &auth_secret, shared_secret, &receiver_point, &sender_point, plaintext)
    })
    .map_err(|_| "push encrypt: ECDH agreement failed".to_string())??;

    Ok(EncryptedPushPayload {
        body,
        content_encoding: CONTENT_ENCODING_AES128GCM,
        salt_len: SALT_LEN,
    })
}

/// Finish encryption once the ECDH secret is known (shared by the sender
/// path above; the receiver-side test re-derives through
/// [`derive_content_keys`] + [`open_record`] instead).
fn encrypt_with_secret(
    salt: &[u8; SALT_LEN],
    auth_secret: &[u8],
    ecdh_secret: &[u8],
    receiver_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    sender_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let (cek, nonce) =
        derive_content_keys(salt, auth_secret, ecdh_secret, receiver_pub, sender_pub)?;
    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    // Final-record delimiter (RFC 8188 §2.1): no padding octets beyond it.
    record.push(0x02);
    seal_record(&cek, &nonce, &mut record)?;
    Ok(assemble_body(salt, sender_pub, &record))
}

/// Derive the content-encryption key and base nonce (RFC 8291 §3.4 combined
/// form, exactly as `mastodon/webpush@9631ac6` layers the `hkdf` gem).
/// Public so the decrypt-check test (and any future receiver) can re-derive
/// independently of [`encrypt_aes128gcm`].
pub fn derive_content_keys(
    salt: &[u8; SALT_LEN],
    auth_secret: &[u8],
    ecdh_secret: &[u8],
    receiver_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    sender_pub: &[u8; UNCOMPRESSED_POINT_LEN],
) -> Result<([u8; CEK_LEN], [u8; NONCE_LEN]), String> {
    let mut info = Vec::with_capacity(13 + 2 * UNCOMPRESSED_POINT_LEN);
    info.extend_from_slice(b"WebPush: info\x00");
    info.extend_from_slice(receiver_pub);
    info.extend_from_slice(sender_pub);
    // No trailing 0x01 here: ring's Expand appends the RFC 5869 counter
    // itself, which is exactly the `|| 0x01` in the §3.4 pseudocode.
    let ikm = hkdf_stage(auth_secret, ecdh_secret, &info, 32)?;
    let cek = hkdf_stage(salt, &ikm, b"Content-Encoding: aes128gcm\x00", CEK_LEN)?;
    let nonce = hkdf_stage(salt, &ikm, b"Content-Encoding: nonce\x00", NONCE_LEN)?;
    let mut cek_arr = [0u8; CEK_LEN];
    cek_arr.copy_from_slice(&cek);
    let mut nonce_arr = [0u8; NONCE_LEN];
    nonce_arr.copy_from_slice(&nonce);
    Ok((cek_arr, nonce_arr))
}

/// AES-128-GCM seal in place (appends the 16-byte tag). Sequence number is
/// 0 (single record), so the nonce is used as-is (RFC 8188 §2.2).
pub fn seal_record(
    cek: &[u8; CEK_LEN],
    nonce: &[u8; NONCE_LEN],
    record: &mut Vec<u8>,
) -> Result<(), String> {
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, cek)
            .map_err(|_| "push seal: invalid cek length".to_string())?,
    );
    let nonce = aead::Nonce::assume_unique_for_key(*nonce);
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), record)
        .map_err(|_| "push seal: encryption failed".to_string())?;
    Ok(())
}

/// AES-128-GCM open in place (verifies + strips the 16-byte tag).
pub fn open_record(
    cek: &[u8; CEK_LEN],
    nonce: &[u8; NONCE_LEN],
    record: &mut Vec<u8>,
) -> Result<(), String> {
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, cek)
            .map_err(|_| "push open: invalid cek length".to_string())?,
    );
    let nonce = aead::Nonce::assume_unique_for_key(*nonce);
    let len = key
        .open_in_place(nonce, aead::Aad::empty(), record)
        .map_err(|_| "push open: authentication failed".to_string())?
        .len();
    record.truncate(len);
    Ok(())
}

/// Parsed `aes128gcm` body: `(salt, record_size, sender_pub, ciphertext)`.
pub fn parse_body(body: &[u8]) -> Result<([u8; SALT_LEN], u32, [u8; 65], Vec<u8>), String> {
    if body.len() < SALT_LEN + 4 + 1 + UNCOMPRESSED_POINT_LEN + 16 + 1 {
        return Err(format!(
            "push body: {} bytes is shorter than the smallest valid aes128gcm record",
            body.len()
        ));
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&body[..SALT_LEN]);
    let rs = u32::from_be_bytes(body[SALT_LEN..SALT_LEN + 4].try_into().unwrap());
    let idlen = body[SALT_LEN + 4] as usize;
    if idlen != UNCOMPRESSED_POINT_LEN {
        return Err(format!("push body: unexpected key id length {idlen}"));
    }
    let mut sender = [0u8; 65];
    sender.copy_from_slice(&body[SALT_LEN + 5..SALT_LEN + 5 + UNCOMPRESSED_POINT_LEN]);
    if sender[0] != 0x04 {
        return Err("push body: sender point is not uncompressed".to_string());
    }
    Ok((salt, rs, sender, body[SALT_LEN + 5 + UNCOMPRESSED_POINT_LEN..].to_vec()))
}

/// Assemble the wire body: `salt || rs_be || idlen || sender_pub || ciphertext`.
fn assemble_body(salt: &[u8; SALT_LEN], sender_pub: &[u8; 65], ciphertext: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(SALT_LEN + 4 + 1 + 65 + ciphertext.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(UNCOMPRESSED_POINT_LEN as u8);
    body.extend_from_slice(sender_pub);
    body.extend_from_slice(ciphertext);
    body
}

// ---------------------------------------------------------------------------
// Legacy `aesgcm` content encoding (draft-ietf-webpush-encryption-04 §3).
// ---------------------------------------------------------------------------
//
// Upstream reference: `mastodon/webpush@9631ac6`
// `lib/webpush/legacy/encryption.rb` (`Webpush::Legacy::Encryption.encrypt`,
// reached via `WebPushRequest#legacy_encrypt` in
// `app/lib/web_push_request.rb:37-39`), sent by Mastodon v4.7.3 (`8ff11c2b`)
// `app/workers/web/push_notification_worker.rb:42-61`
// (`perform_legacy_request`) with `Content-Encoding: aesgcm`,
// `Encryption: salt=<base64url(salt)>` and
// `Crypto-Key: dh=<base64url(server_pub)>;<vapid key>` headers, and
// `Authorization: WebPush <jwt>` (`app/lib/web_push_request.rb:29-31`
// `legacy_authorization_header`). The ciphertext alone is the body.
//
// Construction (a `ring`-only transcription of the gem, checked against an
// independent Node `crypto` transcription with fixed vectors in
// `tests/round4_push_aesgcm.rs`):
//
// - ECDH P-256 between a fresh ephemeral sender key and the subscription's
//   `p256dh` key → shared secret `Z` (same as the standard path).
// - `prk = HKDF(auth_secret, Z, "Content-Encoding: auth\0", 32)`.
// - `context = 0x00 || u16be(len(receiver_pub)) || receiver_pub ||
//   u16be(len(sender_pub)) || sender_pub` (raw uncompressed points; the
//   gem's `create_context`).
// - `cek = HKDF(salt, prk, "Content-Encoding: aesgcm\0P-256" || context, 16)`
//   and `nonce = HKDF(salt, prk, "Content-Encoding: nonce\0P-256" ||
//   context, 12)`, where `salt` is fresh per-message randomness carried in
//   the `Encryption` header (never in the body).
// - Record plaintext = `0x00 || 0x00 || data` (the gem's `encrypt_payload`
//   prepends two zero octets), sealed with AES-128-GCM under `nonce`.
// - The receiver strips the two leading zero octets after opening.
//
// Each `HKDF(salt, IKM, info, L)` stage is RFC 5869 extract-then-expand (the
// `hkdf`-gem `HKDF.new(IKM, salt:, info:).next_bytes(L)` form), i.e. the same
// two-stage layering the gem uses on the standard path (see the module
// interop note above).
//
// No key material ever appears in an error string — only lengths and roles.

/// `Content-Encoding` value for the legacy path.
pub const CONTENT_ENCODING_AESGCM: &str = "aesgcm";

/// The leading padding octets the legacy `encrypt_payload` prepends.
const AESGCM_PADDING_LEN: usize = 2;

/// A legacy encrypted payload: the ciphertext alone is the request body; the
/// salt and the ephemeral sender key travel in the `Encryption` /
/// `Crypto-Key` headers (assembled by
/// [`crate::application::push::build_push_request_aesgcm`]).
#[derive(Debug, Clone)]
pub struct LegacyEncryptedPayload {
    /// The full request body (AES-GCM ciphertext + 16-byte tag, no header).
    pub ciphertext: Vec<u8>,
    /// Per-message salt (base64url-encoded into `Encryption: salt=…`).
    pub salt: [u8; SALT_LEN],
    /// Ephemeral sender point (base64url-encoded into `Crypto-Key: dh=…`).
    pub server_pub: [u8; UNCOMPRESSED_POINT_LEN],
    /// `Content-Encoding: aesgcm`.
    pub content_encoding: &'static str,
}

/// Decode and shape-check the subscription keys (the same rules
/// [`encrypt_aes128gcm`] enforces: base64url-no-pad, a 65-byte uncompressed
/// point for `p256dh`, 16 bytes of auth secret).
///
/// Errors name only the failing role and the expected length — never key
/// bytes. Used by the legacy path only; the standard path keeps its inline
/// checks untouched.
fn parse_subscription_keys(
    key_p256dh_b64: &str,
    key_auth_b64: &str,
) -> Result<([u8; UNCOMPRESSED_POINT_LEN], [u8; AUTH_SECRET_LEN]), String> {
    use base64::Engine as _;
    let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let receiver_pub = enc
        .decode(key_p256dh_b64.trim())
        .map_err(|err| format!("push p256dh key: base64url decode failed: {err}"))?;
    if receiver_pub.len() != UNCOMPRESSED_POINT_LEN || receiver_pub[0] != 0x04 {
        return Err(format!(
            "push p256dh key: must decode to a 65-byte uncompressed point, got {} bytes",
            receiver_pub.len()
        ));
    }
    let auth_secret = enc
        .decode(key_auth_b64.trim())
        .map_err(|err| format!("push auth secret: base64url decode failed: {err}"))?;
    if auth_secret.len() != AUTH_SECRET_LEN {
        return Err(format!(
            "push auth secret: must decode to 16 bytes, got {} bytes",
            auth_secret.len()
        ));
    }
    let mut receiver_point = [0u8; UNCOMPRESSED_POINT_LEN];
    receiver_point.copy_from_slice(&receiver_pub);
    let mut auth_bytes = [0u8; AUTH_SECRET_LEN];
    auth_bytes.copy_from_slice(&auth_secret);
    Ok((receiver_point, auth_bytes))
}

/// One `hkdf`-gem stage: `Expand(HMAC-SHA256(key = salt, data = ikm), info,
/// len)` (RFC 5869 extract-then-expand, i.e. `HKDF.new(ikm, salt:,
/// info:).next_bytes(len)` in `mastodon/webpush@9631ac6`).
fn hkdf_stage(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Result<Vec<u8>, String> {
    let prk_bytes = ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, salt), ikm);
    let prk = hkdf::Prk::new_less_safe(hkdf::HKDF_SHA256, prk_bytes.as_ref());
    let mut out = vec![0u8; len];
    prk.expand(&[info], ByteLen(len))
        .and_then(|okm| okm.fill(&mut out))
        .map_err(|_| "push kdf: expand failed".to_string())?;
    Ok(out)
}

/// Derive the legacy content-encryption key and nonce
/// (draft-ietf-webpush-encryption-04 §3, gem `Legacy::Encryption`).
/// Public so the decrypt-check test can re-derive independently of
/// [`encrypt_aesgcm`].
pub fn derive_aesgcm_keys(
    auth_secret: &[u8],
    ecdh_secret: &[u8],
    salt: &[u8; SALT_LEN],
    receiver_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    sender_pub: &[u8; UNCOMPRESSED_POINT_LEN],
) -> Result<([u8; CEK_LEN], [u8; NONCE_LEN]), String> {
    let prk = hkdf_stage(auth_secret, ecdh_secret, b"Content-Encoding: auth\x00", 32)
        .map_err(|_| "push kdf: legacy prk failed".to_string())?;
    let mut context =
        Vec::with_capacity(1 + 2 + UNCOMPRESSED_POINT_LEN + 2 + UNCOMPRESSED_POINT_LEN);
    context.push(0x00);
    context.extend_from_slice(&(receiver_pub.len() as u16).to_be_bytes());
    context.extend_from_slice(receiver_pub);
    context.extend_from_slice(&(sender_pub.len() as u16).to_be_bytes());
    context.extend_from_slice(sender_pub);
    let mut cek_info = b"Content-Encoding: aesgcm\x00P-256".to_vec();
    cek_info.extend_from_slice(&context);
    let mut nonce_info = b"Content-Encoding: nonce\x00P-256".to_vec();
    nonce_info.extend_from_slice(&context);
    let cek = hkdf_stage(salt, &prk, &cek_info, CEK_LEN)
        .map_err(|_| "push kdf: legacy cek failed".to_string())?;
    let nonce = hkdf_stage(salt, &prk, &nonce_info, NONCE_LEN)
        .map_err(|_| "push kdf: legacy nonce failed".to_string())?;
    let mut cek_arr = [0u8; CEK_LEN];
    cek_arr.copy_from_slice(&cek);
    let mut nonce_arr = [0u8; NONCE_LEN];
    nonce_arr.copy_from_slice(&nonce);
    Ok((cek_arr, nonce_arr))
}

/// Seal one legacy record with all randomness fixed (the fixed-vector test
/// entry point; [`encrypt_aesgcm`] generates the salt and sender key).
pub fn seal_aesgcm_record(
    auth_secret: &[u8],
    ecdh_secret: &[u8],
    salt: &[u8; SALT_LEN],
    receiver_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    sender_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let (cek, nonce) = derive_aesgcm_keys(auth_secret, ecdh_secret, salt, receiver_pub, sender_pub)?;
    let mut record = Vec::with_capacity(AESGCM_PADDING_LEN + plaintext.len());
    // Legacy padding (gem `encrypt_payload`): two zero octets prepended.
    record.extend_from_slice(&[0u8; AESGCM_PADDING_LEN]);
    record.extend_from_slice(plaintext);
    seal_record(&cek, &nonce, &mut record)?;
    Ok(record)
}

/// Encrypt `plaintext` for the subscription keys (`p256dh` / `auth` as
/// base64url-no-pad, exactly as stored on the subscription row) with the
/// legacy `aesgcm` encoding.
///
/// The gem performs no size check on this path (only the standard path
/// refuses messages over 3993 bytes), so neither does this function: our
/// payloads are ~200 bytes and the push service bounds the rest.
///
/// Errors name only the failing role and the expected length — never key
/// bytes.
pub fn encrypt_aesgcm(
    key_p256dh_b64: &str,
    key_auth_b64: &str,
    plaintext: &[u8],
) -> Result<LegacyEncryptedPayload, String> {
    let (receiver_point, auth_secret) = parse_subscription_keys(key_p256dh_b64, key_auth_b64)?;
    let rng = rand::SystemRandom::new();
    let sender_private =
        agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
            .map_err(|_| "push encrypt: ephemeral key generation failed".to_string())?;
    let sender_pub = sender_private
        .compute_public_key()
        .map_err(|_| "push encrypt: ephemeral public key failed".to_string())?;
    let sender_pub_bytes: &[u8] = sender_pub.as_ref();
    if sender_pub_bytes.len() != UNCOMPRESSED_POINT_LEN {
        return Err("push encrypt: ephemeral point has an unexpected length".to_string());
    }
    let mut sender_point = [0u8; UNCOMPRESSED_POINT_LEN];
    sender_point.copy_from_slice(sender_pub_bytes);
    // Per-message salt (gem: `Random.new.bytes(16)`), carried in the
    // `Encryption` header — it IS a KDF input on this path (unlike the
    // standard path's body salt), so it is drawn before deriving.
    let mut salt = [0u8; SALT_LEN];
    rng.fill(&mut salt)
        .map_err(|_| "push encrypt: salt generation failed".to_string())?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &receiver_point[..]);
    let ciphertext = agreement::agree_ephemeral(sender_private, &peer, |shared_secret| {
        seal_aesgcm_record(
            &auth_secret,
            shared_secret,
            &salt,
            &receiver_point,
            &sender_point,
            plaintext,
        )
    })
    .map_err(|_| "push encrypt: ECDH agreement failed".to_string())??;

    Ok(LegacyEncryptedPayload {
        ciphertext,
        salt,
        server_pub: sender_point,
        content_encoding: CONTENT_ENCODING_AESGCM,
    })
}

struct ByteLen(usize);

impl hkdf::KeyType for ByteLen {
    fn len(&self) -> usize {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent receiver-side decrypt: fresh DH from the receiver key,
    /// header parse, KDF re-derive, AES-GCM open, delimiter check.
    #[test]
    fn round_trip_decrypts_from_header() {
        use base64::Engine as _;
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let rng = rand::SystemRandom::new();
        let receiver_private =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let receiver_pub = receiver_private.compute_public_key().unwrap();
        let receiver_pub_bytes: &[u8] = receiver_pub.as_ref();
        let p256dh = enc.encode(receiver_pub_bytes);
        let auth = enc.encode([7u8; AUTH_SECRET_LEN]);
        let plaintext = br#"{"notification_id":"123","notification_type":"mention"}"#;

        let payload = encrypt_aes128gcm(&p256dh, &auth, plaintext).unwrap();
        assert_eq!(payload.content_encoding, "aes128gcm");
        let (salt, rs, sender_pub, mut ciphertext) = parse_body(&payload.body).unwrap();
        assert_eq!(rs, RECORD_SIZE);
        assert_eq!(ciphertext.len(), plaintext.len() + 1 + 16);

        let auth_raw = enc.decode(&auth).unwrap();
        let mut receiver_point = [0u8; 65];
        receiver_point.copy_from_slice(receiver_pub_bytes);
        let opened = agreement::agree_ephemeral(
            receiver_private,
            &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &sender_pub),
            |shared| {
                let (cek, nonce) =
                    derive_content_keys(&salt, &auth_raw, shared, &receiver_point, &sender_pub).unwrap();
                open_record(&cek, &nonce, &mut ciphertext).unwrap();
                // Final-record delimiter + zero padding (none emitted here).
                assert_eq!(ciphertext.last(), Some(&0x02));
                ciphertext[..ciphertext.len() - 1].to_vec()
            },
        )
        .unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn refuses_bad_keys_and_oversize() {
        assert!(encrypt_aes128gcm("!!!", "AAAAAAAAAAAAAAAAAAAAAA", b"x").is_err());
        assert!(encrypt_aes128gcm(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([4u8; 65]),
            "short",
            b"x"
        )
        .is_err());
        let big = vec![0u8; MAX_PLAINTEXT_LEN + 1];
        let rng = rand::SystemRandom::new();
        let privkey =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let pubkey = privkey.compute_public_key().unwrap();
        use base64::Engine as _;
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let err = encrypt_aes128gcm(
            &enc.encode(pubkey.as_ref()),
            &enc.encode([0u8; AUTH_SECRET_LEN]),
            &big,
        )
        .expect_err("oversize refused");
        assert!(err.contains("exceed"), "{err}");
    }
}

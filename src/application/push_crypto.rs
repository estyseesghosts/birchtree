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
//! (no new dependency — `cargo` is offline, so no ECDH/AES-GCM crate could
//! be added):
//!
//! - ECDH P-256 between a fresh ephemeral sender key and the subscription's
//!   `p256dh` (receiver) key → shared secret `Z`.
//! - `PRK = HKDF-Extract(salt = auth_secret, IKM = Z)` (SHA-256).
//! - `info = "WebPush: info" || 0x00 || receiver_pub || sender_pub`
//!   (both raw 65-byte uncompressed points).
//! - `CEK = HKDF-Expand(PRK, "Content-Encoding: aes128gcm" || 0x00 || info, 16)`.
//! - `nonce = HKDF-Expand(PRK, "Content-Encoding: nonce" || 0x00 || info, 12)`.
//! - Record plaintext = `data || 0x02` (final-record delimiter, RFC 8188
//!   §2; no extra padding), sealed with AES-128-GCM under `nonce`.
//! - Body = `salt(16) || rs_be(4) || idlen(1)=65 || sender_pub(65) ||
//!   ciphertext`, with `rs = 4096`.
//!
//! Interop note: the KDF info strings above are per RFC 8291 §3.4 (they
//! match the `webpush`-gem/`pywebpush` construction byte for byte). The
//! round-trip test in `tests/round3_notif_push.rs` re-derives the receiver
//! side independently (fresh DH from the receiver key + header parse +
//! AES-GCM open); delivery against a live push service is still pending lab
//! verification, like all network paths.
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
    let body = agreement::agree_ephemeral(sender_private, &peer, |shared_secret| {
        encrypt_with_secret(&auth_secret, shared_secret, &receiver_point, &sender_point, plaintext)
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
/// [`derive_content_keys`] + [`seal_open`] instead).
fn encrypt_with_secret(
    auth_secret: &[u8],
    ecdh_secret: &[u8],
    receiver_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    sender_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let (cek, nonce) = derive_content_keys(auth_secret, ecdh_secret, receiver_pub, sender_pub)?;
    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    // Final-record delimiter (RFC 8188 §2.1): no padding octets beyond it.
    record.push(0x02);
    seal_record(&cek, &nonce, &mut record)?;
    // Per-message header salt (RFC 8291 §3): fresh randomness per message.
    // It is NOT a KDF input on this path — RFC 8291 §3.4 fixes the HKDF
    // salt to the subscription `auth_secret` (see `derive_content_keys`).
    let rng = rand::SystemRandom::new();
    let mut salt = [0u8; SALT_LEN];
    rng.fill(&mut salt)
        .map_err(|_| "push encrypt: salt generation failed".to_string())?;
    Ok(assemble_body(&salt, sender_pub, &record))
}

/// Derive the content-encryption key and base nonce (RFC 8291 §3.4).
/// Public so the decrypt-check test (and any future receiver) can re-derive
/// independently of [`encrypt_aes128gcm`].
pub fn derive_content_keys(
    auth_secret: &[u8],
    ecdh_secret: &[u8],
    receiver_pub: &[u8; UNCOMPRESSED_POINT_LEN],
    sender_pub: &[u8; UNCOMPRESSED_POINT_LEN],
) -> Result<([u8; CEK_LEN], [u8; NONCE_LEN]), String> {
    let mut info = Vec::with_capacity(13 + 2 * UNCOMPRESSED_POINT_LEN);
    info.extend_from_slice(b"WebPush: info\x00");
    info.extend_from_slice(receiver_pub);
    info.extend_from_slice(sender_pub);
    let mut cek_info = b"Content-Encoding: aes128gcm\x00".to_vec();
    cek_info.extend_from_slice(&info);
    let mut nonce_info = b"Content-Encoding: nonce\x00".to_vec();
    nonce_info.extend_from_slice(&info);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, auth_secret).extract(ecdh_secret);
    let mut cek = [0u8; CEK_LEN];
    prk.expand(&[&cek_info], ByteLen(CEK_LEN))
        .map_err(|_| "push kdf: cek expand failed".to_string())?
        .fill(&mut cek)
        .map_err(|_| "push kdf: cek fill failed".to_string())?;
    let mut nonce = [0u8; NONCE_LEN];
    prk.expand(&[&nonce_info], ByteLen(NONCE_LEN))
        .map_err(|_| "push kdf: nonce expand failed".to_string())?
        .fill(&mut nonce)
        .map_err(|_| "push kdf: nonce fill failed".to_string())?;
    Ok((cek, nonce))
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
        let (_salt, rs, sender_pub, mut ciphertext) = parse_body(&payload.body).unwrap();
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
                    derive_content_keys(&auth_raw, shared, &receiver_point, &sender_pub).unwrap();
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

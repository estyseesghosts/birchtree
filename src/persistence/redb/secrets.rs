//! Authenticated encryption for long-lived application secrets.
//!
//! TOTP seeds, local ActivityPub private keys and mail-outbox bearers are
//! sealed with AES-256-GCM (via `ring`) under a key that lives outside the
//! database. Every sealed value carries a self-describing header so keys can
//! rotate:
//!
//! ```text
//! "BTS" | format version (1 byte) | key id (u32, big endian) | nonce (12) | ciphertext || tag
//! ```
//!
//! The header and a caller-supplied *context* (for example `totp:42`) are
//! authenticated as associated data, so a ciphertext cannot be moved to a
//! different row or downgraded to another key id without failing
//! authentication.
//!
//! Failure policy: this module never invents data. A missing key, an unknown
//! key id, tampering and a wrong context are all distinct errors and callers
//! must propagate them (never reset MFA, never generate a replacement actor
//! key). Losing every key that sealed a row makes that row unrecoverable by
//! design; it is not a migration problem that can be repaired in place.
//!
//! Keyring text format (file or environment variable): one key per line as
//! `<key id>:<64 hex characters>`; blank lines and `#` comments are ignored.
//! The first key is the active (sealing) key; the others only open older
//! values until a re-seal pass retires them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

use super::RedbStore;

const MAGIC: &[u8; 3] = b"BTS";
const FORMAT_VERSION: u8 = 1;
const HEADER_LEN: usize = 8;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const TEXT_PREFIX: &str = "bt1:";

/// Why a secret could not be sealed or opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("secret encryption key is not configured")]
    KeyMissing,
    #[error("secret was sealed under key id {0}, which is not in the keyring")]
    UnknownKey(u32),
    #[error("secret failed authentication (wrong key, wrong context or tampering)")]
    Authentication,
    #[error("sealed secret is malformed")]
    Malformed,
    #[error("invalid keyring: {0}")]
    InvalidKeyring(String),
    #[error("system random source failed")]
    Random,
}

impl From<SecretError> for crate::domain::Error {
    fn from(err: SecretError) -> Self {
        crate::domain::Error::Internal(err.to_string())
    }
}

/// One 256-bit encryption key and its identifier.
#[derive(Clone)]
pub struct SecretKey {
    id: u32,
    bytes: Zeroizing<[u8; KEY_LEN]>,
}

impl SecretKey {
    pub fn new(id: u32, bytes: [u8; KEY_LEN]) -> Self {
        Self {
            id,
            bytes: Zeroizing::new(bytes),
        }
    }

    /// Parse a 64-character hex key.
    pub fn from_hex(id: u32, hex_key: &str) -> Result<Self, SecretError> {
        let decoded: Vec<u8> = hex::decode(hex_key.trim())
            .map_err(|_| SecretError::InvalidKeyring("key is not valid hex".into()))?;
        let bytes: [u8; KEY_LEN] = decoded
            .try_into()
            .map_err(|_| SecretError::InvalidKeyring("key must be 32 bytes".into()))?;
        Ok(Self::new(id, bytes))
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    fn aead_key(&self) -> Result<LessSafeKey, SecretError> {
        let unbound = UnboundKey::new(&aead::AES_256_GCM, self.bytes.as_slice())
            .map_err(|_| SecretError::InvalidKeyring("key rejected by AEAD".into()))?;
        Ok(LessSafeKey::new(unbound))
    }
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretKey")
            .field("id", &self.id)
            .field("bytes", &"[redacted]")
            .finish()
    }
}

/// Text recovered by [`SecretBox::open_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedText {
    pub text: String,
    /// True when the stored value was legacy plaintext or sealed under a
    /// retired key, so the caller should rewrite it with
    /// [`SecretBox::seal_text`].
    pub needs_reseal: bool,
}

/// A keyring: the active sealing key plus retired keys that can still open
/// old values. An empty keyring is the explicit "no key configured" state:
/// it can open nothing that is sealed and refuses to seal.
#[derive(Clone, Default)]
pub struct SecretBox {
    keys: Vec<SecretKey>,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBox")
            .field("key_ids", &self.keys.iter().map(SecretKey::id).collect::<Vec<u32>>())
            .finish()
    }
}

impl SecretBox {
    /// The unconfigured state (development and tests only).
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Build a keyring; `active` seals, `retired` only opens.
    pub fn new(active: SecretKey, retired: Vec<SecretKey>) -> Result<Self, SecretError> {
        let mut keys: Vec<SecretKey> = vec![active];
        keys.extend(retired);
        for (index, key) in keys.iter().enumerate() {
            if keys[..index].iter().any(|earlier| earlier.id == key.id) {
                return Err(SecretError::InvalidKeyring(format!(
                    "duplicate key id {}",
                    key.id
                )));
            }
        }
        Ok(Self { keys })
    }

    /// Parse the keyring text format described in the module docs.
    pub fn from_keyring(text: &str) -> Result<Self, SecretError> {
        let mut keys: Vec<SecretKey> = Vec::new();
        for raw_line in text.lines() {
            let line: &str = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((id_text, key_text)) = line.split_once(':') else {
                return Err(SecretError::InvalidKeyring(
                    "expected `<key id>:<hex key>` per line".into(),
                ));
            };
            let id: u32 = id_text
                .trim()
                .parse()
                .map_err(|_| SecretError::InvalidKeyring("key id must be a u32".into()))?;
            keys.push(SecretKey::from_hex(id, key_text)?);
        }
        let mut iter = keys.into_iter();
        let Some(active) = iter.next() else {
            return Err(SecretError::InvalidKeyring("keyring is empty".into()));
        };
        Self::new(active, iter.collect())
    }

    /// Load a keyring file. On Unix the file must not be readable by group or
    /// others: a world-readable key file is refused rather than used.
    pub fn from_keyring_file(path: &Path) -> Result<Self, SecretError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode: u32 = std::fs::metadata(path)
                .map_err(|_| SecretError::KeyMissing)?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                return Err(SecretError::InvalidKeyring(format!(
                    "keyring file {} must not be accessible by group or others",
                    path.display()
                )));
            }
        }
        let text: Zeroizing<String> = Zeroizing::new(
            std::fs::read_to_string(path).map_err(|_| SecretError::KeyMissing)?,
        );
        Self::from_keyring(text.as_str())
    }

    /// Load a keyring from an environment variable.
    pub fn from_keyring_env(variable: &str) -> Result<Self, SecretError> {
        let text: Zeroizing<String> =
            Zeroizing::new(std::env::var(variable).map_err(|_| SecretError::KeyMissing)?);
        Self::from_keyring(text.as_str())
    }

    pub fn is_enabled(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn active_key_id(&self) -> Option<u32> {
        self.keys.first().map(SecretKey::id)
    }

    /// True when `bytes` is a sealed envelope (as opposed to legacy JSON).
    pub fn is_sealed(bytes: &[u8]) -> bool {
        bytes.len() >= HEADER_LEN && bytes.starts_with(MAGIC)
    }

    fn sealed_key_id(bytes: &[u8]) -> Option<u32> {
        if !Self::is_sealed(bytes) || bytes[3] != FORMAT_VERSION {
            return None;
        }
        let id_bytes: [u8; 4] = bytes[4..8].try_into().ok()?;
        Some(u32::from_be_bytes(id_bytes))
    }

    /// Whether a stored value should be rewritten: it is legacy plaintext or
    /// sealed under a key other than the active one.
    pub fn needs_reseal(&self, stored: &[u8]) -> bool {
        match Self::sealed_key_id(stored) {
            Some(id) => Some(id) != self.active_key_id(),
            None => true,
        }
    }

    /// Seal `plaintext` under the active key, binding it to `context`.
    pub fn seal(&self, context: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let Some(active) = self.keys.first() else {
            return Err(SecretError::KeyMissing);
        };
        let mut nonce_bytes = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| SecretError::Random)?;
        let mut out: Vec<u8> = Vec::with_capacity(HEADER_LEN + NONCE_LEN + plaintext.len() + TAG_LEN);
        out.extend_from_slice(MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&active.id.to_be_bytes());
        out.extend_from_slice(&nonce_bytes);
        let mut body: Vec<u8> = plaintext.to_vec();
        let aad: Vec<u8> = associated_data(&out[..HEADER_LEN], context);
        active
            .aead_key()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(aad),
                &mut body,
            )
            .map_err(|_| SecretError::Authentication)?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Open a sealed envelope. Fails closed on any mismatch.
    pub fn open(&self, context: &[u8], sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SecretError> {
        if !Self::is_sealed(sealed)
            || sealed[3] != FORMAT_VERSION
            || sealed.len() < HEADER_LEN + NONCE_LEN + TAG_LEN
        {
            return Err(SecretError::Malformed);
        }
        let Some(key_id) = Self::sealed_key_id(sealed) else {
            return Err(SecretError::Malformed);
        };
        if self.keys.is_empty() {
            return Err(SecretError::KeyMissing);
        }
        let Some(key) = self.keys.iter().find(|key| key.id == key_id) else {
            return Err(SecretError::UnknownKey(key_id));
        };
        let nonce_bytes: [u8; NONCE_LEN] = sealed[HEADER_LEN..HEADER_LEN + NONCE_LEN]
            .try_into()
            .map_err(|_| SecretError::Malformed)?;
        let aad: Vec<u8> = associated_data(&sealed[..HEADER_LEN], context);
        let mut body: Vec<u8> = sealed[HEADER_LEN + NONCE_LEN..].to_vec();
        let plain_len: usize = key
            .aead_key()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(aad),
                &mut body,
            )
            .map_err(|_| SecretError::Authentication)?
            .len();
        body.truncate(plain_len);
        Ok(Zeroizing::new(body))
    }

    /// Seal text into the printable `bt1:<hex>` form used inside JSON fields.
    pub fn seal_text(&self, context: &[u8], text: &str) -> Result<String, SecretError> {
        let sealed: Vec<u8> = self.seal(context, text.as_bytes())?;
        Ok(format!("{TEXT_PREFIX}{}", hex::encode(sealed)))
    }

    /// Seal when a key is configured; otherwise return the text unchanged.
    /// Only the unconfigured development/test mode takes the second branch.
    pub fn seal_text_if_enabled(&self, context: &[u8], text: &str) -> Result<String, SecretError> {
        if self.is_enabled() {
            self.seal_text(context, text)
        } else {
            Ok(text.to_string())
        }
    }

    /// Open a field written by [`Self::seal_text`]. A value without the
    /// `bt1:` prefix is legacy plaintext and is returned with
    /// `needs_reseal` set (a configured keyring still accepts it so existing
    /// rows can be migrated; an unconfigured one never decrypts anything).
    pub fn open_text(&self, context: &[u8], stored: &str) -> Result<OpenedText, SecretError> {
        let Some(encoded) = stored.strip_prefix(TEXT_PREFIX) else {
            return Ok(OpenedText {
                text: stored.to_string(),
                needs_reseal: self.is_enabled(),
            });
        };
        let sealed: Vec<u8> = hex::decode(encoded).map_err(|_| SecretError::Malformed)?;
        let plain = self.open(context, &sealed)?;
        let text: String =
            String::from_utf8(plain.to_vec()).map_err(|_| SecretError::Malformed)?;
        Ok(OpenedText {
            text,
            needs_reseal: self.needs_reseal(&sealed),
        })
    }
}

fn associated_data(header: &[u8], context: &[u8]) -> Vec<u8> {
    let mut aad: Vec<u8> = Vec::with_capacity(header.len() + context.len());
    aad.extend_from_slice(header);
    aad.extend_from_slice(context);
    aad
}

/// Keyrings installed per database path. A `RedbStore` looks its keyring up
/// here, so wiring needs no change to the store constructor.
static INSTALLED: OnceLock<Mutex<HashMap<PathBuf, SecretBox>>> = OnceLock::new();

fn installed() -> &'static Mutex<HashMap<PathBuf, SecretBox>> {
    INSTALLED.get_or_init(|| Mutex::new(HashMap::new()))
}

impl RedbStore {
    /// Install the keyring used for every secret this store seals or opens.
    /// Call once at startup, before serving traffic, then run
    /// [`RedbStore::seal_legacy_secrets`] to migrate older rows.
    pub fn install_secret_box(&self, secret_box: SecretBox) {
        installed()
            .lock()
            .expect("secret keyring registry")
            .insert(self.path.clone(), secret_box);
    }

    /// The keyring for this store ([`SecretBox::disabled`] until installed).
    pub(in crate::persistence::redb) fn secret_box(&self) -> SecretBox {
        installed()
            .lock()
            .expect("secret keyring registry")
            .get(&self.path)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: u32, fill: u8) -> SecretKey {
        SecretKey::new(id, [fill; KEY_LEN])
    }

    fn keyed() -> SecretBox {
        SecretBox::new(key(7, 1), vec![key(3, 2)]).expect("keyring")
    }

    #[test]
    fn round_trips_and_binds_context() {
        let secrets = keyed();
        let sealed: Vec<u8> = secrets.seal(b"totp:1", b"seed").expect("seal");
        assert!(SecretBox::is_sealed(&sealed));
        assert_eq!(secrets.open(b"totp:1", &sealed).expect("open").as_slice(), b"seed");
        assert_eq!(secrets.open(b"totp:2", &sealed), Err(SecretError::Authentication));
    }

    #[test]
    fn tampering_and_wrong_key_fail_closed() {
        let secrets = keyed();
        let mut sealed: Vec<u8> = secrets.seal(b"c", b"seed").expect("seal");
        let last: usize = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(secrets.open(b"c", &sealed), Err(SecretError::Authentication));

        let sealed: Vec<u8> = secrets.seal(b"c", b"seed").expect("seal");
        let other = SecretBox::new(key(7, 9), Vec::new()).expect("keyring");
        assert_eq!(other.open(b"c", &sealed), Err(SecretError::Authentication));
        let unknown = SecretBox::new(key(8, 1), Vec::new()).expect("keyring");
        assert_eq!(unknown.open(b"c", &sealed), Err(SecretError::UnknownKey(7)));
        assert_eq!(SecretBox::disabled().open(b"c", &sealed), Err(SecretError::KeyMissing));
        assert_eq!(SecretBox::disabled().seal(b"c", b"x"), Err(SecretError::KeyMissing));
    }

    #[test]
    fn header_downgrade_is_rejected() {
        let secrets = keyed();
        let mut sealed: Vec<u8> = secrets.seal(b"c", b"seed").expect("seal");
        // Re-label the envelope as key 3: the header is authenticated.
        sealed[4..8].copy_from_slice(&3u32.to_be_bytes());
        assert_eq!(secrets.open(b"c", &sealed), Err(SecretError::Authentication));
    }

    #[test]
    fn retired_keys_open_and_request_reseal() {
        let old = SecretBox::new(key(3, 2), Vec::new()).expect("keyring");
        let sealed: Vec<u8> = old.seal(b"c", b"seed").expect("seal");
        let rotated = keyed();
        assert_eq!(rotated.open(b"c", &sealed).expect("open").as_slice(), b"seed");
        assert!(rotated.needs_reseal(&sealed));
        let fresh: Vec<u8> = rotated.seal(b"c", b"seed").expect("seal");
        assert!(!rotated.needs_reseal(&fresh));
        assert!(rotated.needs_reseal(b"{\"legacy\":true}"));
    }

    #[test]
    fn text_form_passes_legacy_plaintext_through() {
        let secrets = keyed();
        let sealed: String = secrets.seal_text(b"k", "-----BEGIN PRIVATE KEY-----").expect("seal");
        assert!(sealed.starts_with("bt1:"));
        let opened = secrets.open_text(b"k", &sealed).expect("open");
        assert_eq!(opened.text, "-----BEGIN PRIVATE KEY-----");
        assert!(!opened.needs_reseal);
        let legacy = secrets.open_text(b"k", "-----BEGIN PRIVATE KEY-----").expect("legacy");
        assert!(legacy.needs_reseal);
        assert_eq!(
            SecretBox::disabled().open_text(b"k", &sealed),
            Err(SecretError::KeyMissing)
        );
    }

    #[test]
    fn keyring_text_parses_and_rejects_garbage() {
        let hex_key: String = "ab".repeat(32);
        let text: String = format!("# comment\n\n5:{hex_key}\n2:{hex_key}\n");
        let secrets = SecretBox::from_keyring(&text).expect("keyring");
        assert_eq!(secrets.active_key_id(), Some(5));
        assert!(SecretBox::from_keyring("").is_err());
        assert!(SecretBox::from_keyring("5:abcd").is_err());
        assert!(SecretBox::from_keyring(&format!("5:{hex_key}\n5:{hex_key}")).is_err());
        assert!(SecretBox::from_keyring("nonsense").is_err());
    }

    #[test]
    fn debug_output_redacts_key_material() {
        let rendered: String = format!("{:?}", keyed());
        assert!(!rendered.contains("0101"));
        assert!(rendered.contains("key_ids"));
    }
}

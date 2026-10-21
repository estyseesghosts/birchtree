//! The RSA provider adapter: the only module that touches AWS-LC.
//!
//! Federation keeps the legacy Cavage `rsa-sha256` wire format (RSASSA-PKCS1-v1_5
//! with SHA-256), so signatures and published keys stay compatible with every
//! peer. Private keys are stored as PKCS#8 PEM (`BEGIN PRIVATE KEY`) and public
//! keys are published as SubjectPublicKeyInfo PEM (`BEGIN PUBLIC KEY`).
//!
//! AWS-LC performs the private-key operation with its own blinding; no unblinded
//! signing path exists here. SubjectPublicKeyInfo is converted with the `spki`
//! crate, never by hand-parsing DER.

use crate::error::Error;
use aws_lc_rs::{
    encoding::AsDer,
    rand::SystemRandom,
    rsa::{KeyPair, KeySize},
    signature::{RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256, UnparsedPublicKey},
};
use spki::{
    der::{
        asn1::{BitString, Null},
        pem::{self, LineEnding},
        Any, DecodePem, EncodePem,
    },
    AlgorithmIdentifierOwned, ObjectIdentifier, SubjectPublicKeyInfoOwned,
};
use std::{fmt, sync::Arc};

const PKCS8_PEM_LABEL: &str = "PRIVATE KEY";
const RSA_ENCRYPTION_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");

/// A freshly generated actor key pair, both halves PEM encoded.
pub(crate) struct GeneratedPems {
    pub(crate) private_key: String,
    pub(crate) public_key: String,
}

/// An imported RSA private key that signs legacy `rsa-sha256` signatures.
#[derive(Clone)]
pub struct SigningKey {
    // `KeyPair` is not `Clone`; the shared handle lets delivery tasks and the key cache copy it.
    pair: Arc<KeyPair>,
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SigningKey(..)")
    }
}

impl SigningKey {
    /// Imports a PKCS#8 PEM private key. A key that cannot be imported is an error;
    /// callers must not substitute a new identity.
    pub fn from_pkcs8_pem(private_key_pem: &str) -> Result<Self, Error> {
        let (label, der) = pem::decode_vec(private_key_pem.trim().as_bytes())
            .map_err(|err| Error::Other(format!("private key is not valid PEM: {err}")))?;
        if label != PKCS8_PEM_LABEL {
            return Err(Error::Other("private key PEM is not PKCS#8".to_string()));
        }
        let pair = KeyPair::from_pkcs8(&der)
            .map_err(|_| Error::Other("private key is not a usable RSA PKCS#8 key".to_string()))?;
        Ok(Self { pair: Arc::new(pair) })
    }

    /// Imports a PKCS#1 `RSAPrivateKey` DER key (test fixtures).
    #[cfg(test)]
    pub(crate) fn from_pkcs1_der(der: &[u8]) -> Result<Self, Error> {
        let pair = KeyPair::from_der(der)
            .map_err(|_| Error::Other("private key is not a usable RSA PKCS#1 key".to_string()))?;
        Ok(Self { pair: Arc::new(pair) })
    }

    /// Signs the whole signing string with RSASSA-PKCS1-v1_5 / SHA-256. The message is
    /// hashed inside the operation, so callers pass the signing string, not a digest.
    pub fn sign_legacy_rsa_sha256(&self, signing_string: &[u8]) -> Result<Vec<u8>, Error> {
        let mut signature = vec![0_u8; self.pair.public_modulus_len()];
        self.pair
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_string,
                &mut signature,
            )
            .map_err(|_| Error::Other("RSA signing failed".to_string()))?;
        Ok(signature)
    }

    /// The matching public key as SubjectPublicKeyInfo PEM.
    pub fn public_key_pem(&self) -> Result<String, Error> {
        use aws_lc_rs::signature::KeyPair as _;

        public_key_pem_from_pkcs1(self.pair.public_key().as_ref())
    }

    /// The private key as PKCS#8 PEM.
    pub(crate) fn private_key_pem(&self) -> Result<String, Error> {
        let der = self
            .pair
            .as_der()
            .map_err(|_| Error::Other("RSA private key serialization failed".to_string()))?;
        pem::encode_string(PKCS8_PEM_LABEL, LineEnding::LF, der.as_ref())
            .map_err(|err| Error::Other(format!("PEM encoding failed: {err}")))
    }
}

/// Generates a 2048-bit actor key pair.
pub(crate) fn generate_pems() -> Result<GeneratedPems, Error> {
    let pair = KeyPair::generate(KeySize::Rsa2048)
        .map_err(|_| Error::Other("RSA key generation failed".to_string()))?;
    let key = SigningKey { pair: Arc::new(pair) };
    Ok(GeneratedPems {
        private_key: key.private_key_pem()?,
        public_key: key.public_key_pem()?,
    })
}

fn public_key_pem_from_pkcs1(pkcs1_der: &[u8]) -> Result<String, Error> {
    let subject_public_key = BitString::from_bytes(pkcs1_der)?;
    let info = SubjectPublicKeyInfoOwned {
        algorithm: AlgorithmIdentifierOwned {
            oid: RSA_ENCRYPTION_OID,
            parameters: Some(Any::from(Null)),
        },
        subject_public_key,
    };
    Ok(info.to_pem(LineEnding::LF)?)
}

/// Verifies a legacy `rsa-sha256` signature over `signing_string` with a
/// SubjectPublicKeyInfo PEM public key. An unusable key is an error; a bad
/// signature is `Ok(false)`.
pub fn verify_legacy_rsa_sha256(
    public_key_pem: &str,
    signing_string: &[u8],
    signature: &[u8],
) -> Result<bool, Error> {
    // Some servers (Akkoma) publish the PEM with a trailing blank line, which the strict
    // RFC 7468 parser rejects; normalise the line ending.
    let normalized = format!("{}\n", public_key_pem.trim());
    let info = SubjectPublicKeyInfoOwned::from_pem(normalized.as_bytes())?;
    if info.algorithm.oid != RSA_ENCRYPTION_OID {
        return Err(Error::Other("public key is not an RSA key".to_string()));
    }
    let Some(pkcs1_der) = info.subject_public_key.as_bytes() else {
        return Err(Error::Other(
            "public key bit string is not byte aligned".to_string(),
        ));
    };
    let key = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pkcs1_der);
    Ok(key.verify(signing_string, signature).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_round_trip_through_pem() {
        let pems = generate_pems().unwrap();
        assert!(pems.private_key.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(pems.public_key.starts_with("-----BEGIN PUBLIC KEY-----"));
        let key = SigningKey::from_pkcs8_pem(&pems.private_key).unwrap();
        assert_eq!(key.public_key_pem().unwrap(), pems.public_key);
        let signature = key.sign_legacy_rsa_sha256(b"message").unwrap();
        assert_eq!(signature.len(), 256);
        assert!(verify_legacy_rsa_sha256(&pems.public_key, b"message", &signature).unwrap());
        assert!(!verify_legacy_rsa_sha256(&pems.public_key, b"other", &signature).unwrap());
    }

    #[test]
    fn malformed_keys_are_rejected_not_replaced() {
        assert!(SigningKey::from_pkcs8_pem("").is_err());
        assert!(SigningKey::from_pkcs8_pem("not pem").is_err());
        let pems = generate_pems().unwrap();
        assert!(SigningKey::from_pkcs8_pem(&pems.public_key).is_err());
        let truncated = pems.private_key.replacen("MI", "MJ", 1);
        assert!(SigningKey::from_pkcs8_pem(&truncated).is_err());
        assert!(verify_legacy_rsa_sha256("garbage", b"m", b"s").is_err());
    }
}

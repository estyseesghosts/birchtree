use super::webauthn_cbor::{
    Cbor, cbor_decode, cbor_map_find, parse_auth_data, parse_client_data, sha256_of, verify_es256,
};
use crate::domain::Id;

// ---------------------------------------------------------------------------
// WebAuthn second factor (challenge/response ceremony, ES256).
// ---------------------------------------------------------------------------

/// WebAuthn challenge lifetime (5 minutes; ceremonies are interactive).
pub const WEBAUTHN_CHALLENGE_TTL_MS: u64 = 5 * 60 * 1000;

/// The only COSE algorithm this build verifies: ES256 (P-256 + SHA-256).
/// RS256/EdDSA authenticators are refused with reason at registration â€” a
/// recorded divergence, never a silent accept (see
/// [`WEBAUTHN_UNSUPPORTED_ALGORITHM`]).
pub const WEBAUTHN_COSE_ES256: i64 = -7;

/// Upper bound on ceremony inputs (federation input is untrusted; these are
/// local but bounded all the same).
pub const WEBAUTHN_MAX_CEREMONY_BYTES: usize = 8192;

/// Which ceremony a challenge was minted for. Registration and
/// authentication challenges never cross-accept (distinct `clientData`
/// `type` values, checked both here and against the challenge row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebauthnPurpose {
    Registration,
    Authentication,
}

impl WebauthnPurpose {
    /// The `clientDataJSON.type` this purpose requires.
    pub fn client_data_type(self) -> &'static str {
        match self {
            WebauthnPurpose::Registration => "webauthn.create",
            WebauthnPurpose::Authentication => "webauthn.get",
        }
    }
}

/// A single-use login challenge (kept server-side; the `clientDataJSON`
/// echoes it back inside the signed payload).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebauthnChallenge {
    pub account_id: Id,
    /// base64url-no-pad of at least 16 random bytes (fed by the application
    /// layer â€” no RNG in the domain layer, same rule as invite codes).
    pub challenge_b64: String,
    pub purpose: WebauthnPurpose,
    pub created_at_ms: u64,
}

impl WebauthnChallenge {
    /// Mint a challenge over caller-supplied randomness (`random_bytes`
    /// must hold at least 16 bytes).
    pub fn mint(
        account_id: Id,
        purpose: WebauthnPurpose,
        random_bytes: &[u8],
        now_ms: u64,
    ) -> crate::domain::Result<Self> {
        use base64::Engine as _;
        if random_bytes.len() < 16 {
            return Err(crate::domain::Error::Internal(
                "webauthn challenge needs at least 16 random bytes".into(),
            ));
        }
        Ok(WebauthnChallenge {
            account_id,
            challenge_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes),
            purpose,
            created_at_ms: now_ms,
        })
    }

    /// True when the challenge is still live at `now_ms`.
    pub fn live_at(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.created_at_ms) <= WEBAUTHN_CHALLENGE_TTL_MS
    }
}

/// Most WebAuthn credentials (security keys) one account may register.
/// Enforced atomically by the storage port; registration beyond it is a
/// `Conflict`, not a silent truncation of the login-time listing.
pub const MAX_WEBAUTHN_CREDENTIALS_PER_ACCOUNT: usize = 20;

/// A registered WebAuthn credential (the `WEBAUTHN_CREDS` table row, keyed
/// by raw credential-id bytes; JSON shape).
///
/// The credential id and public key are authentication material: redacted
/// from [`std::fmt::Debug`], never logged.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebauthnCredential {
    pub account_id: Id,
    /// Raw credential-id bytes (the table key).
    pub credential_id: Vec<u8>,
    /// P-256 public point, uncompressed SEC1 (`04 || x || y`, 65 bytes).
    pub public_key_uncompressed: Vec<u8>,
    /// Last authenticator sign count (clone detection, Â§6.1 of the spec).
    pub sign_count: u32,
    pub created_at_ms: u64,
}

impl std::fmt::Debug for WebauthnCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebauthnCredential")
            .field("account_id", &self.account_id)
            .field("credential_id", &"[redacted]")
            .field("public_key_uncompressed", &"[redacted]")
            .field("sign_count", &self.sign_count)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

/// A verified registration: the credential id, P-256 coordinates and
/// authenticator sign count for the caller to persist.
pub struct ParsedWebauthnCredential {
    pub credential_id: Vec<u8>,
    pub x: [u8; 32],
    pub y: [u8; 32],
    pub sign_count: u32,
}

pub(super) fn webauthn_err(message: impl Into<String>) -> crate::domain::Error {
    crate::domain::Error::InvalidInput(message.into())
}

/// Verify a registration ceremony and return the credential to persist.
///
/// Checks, in order: challenge purpose + expiry, `clientDataJSON`
/// (`webauthn.create`, echoed challenge, origin), attestation format
/// (`none` with an empty statement, or `packed` self-attestation verified
/// against the credential key â€” `x5c` chains and every other format are
/// refused with reason), RP ID hash, user presence, and the ES256 COSE key.
/// Raw cryptographic failures are `InvalidInput` (client error), never
/// silent acceptance.
pub fn verify_webauthn_registration(
    rp_id: &str,
    origin: &str,
    challenge: &WebauthnChallenge,
    now_ms: u64,
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> crate::domain::Result<ParsedWebauthnCredential> {
    if challenge.purpose != WebauthnPurpose::Registration {
        return Err(webauthn_err(
            "WebAuthn challenge was not minted for registration",
        ));
    }
    if !challenge.live_at(now_ms) {
        return Err(webauthn_err("WebAuthn challenge has expired"));
    }
    parse_client_data(
        client_data_json,
        WebauthnPurpose::Registration.client_data_type(),
        &challenge.challenge_b64,
        origin,
    )?;
    if attestation_object.len() > WEBAUTHN_MAX_CEREMONY_BYTES {
        return Err(webauthn_err("WebAuthn attestation object is too large"));
    }
    let (auth_data_raw, att_stmt, fmt) = parse_attestation_object(attestation_object)?;
    let parsed = parse_auth_data(&auth_data_raw, true)?;
    if parsed.rp_id_hash != sha256_of(rp_id.as_bytes()) {
        return Err(webauthn_err(
            "WebAuthn credential relies on a different party",
        ));
    }
    let (x, y) = match (parsed.cose_x, parsed.cose_y) {
        (Some(x), Some(y)) => (x, y),
        _ => return Err(webauthn_err("WebAuthn credential key is malformed")),
    };
    let mut point = vec![0x04u8];
    point.extend_from_slice(&x);
    point.extend_from_slice(&y);
    match fmt.as_str() {
        "none" => {
            if !att_stmt.is_empty() {
                return Err(webauthn_err(
                    "WebAuthn none-attestation must carry an empty statement",
                ));
            }
        }
        "packed" => {
            verify_packed_self_attestation(&att_stmt, &auth_data_raw, client_data_json, &point)?;
        }
        _ => {
            return Err(webauthn_err(format!(
                "WebAuthn attestation format {fmt} is not supported"
            )));
        }
    }
    let credential_id = parsed.credential_id.unwrap_or_default();
    if credential_id.is_empty() || credential_id.len() > 1023 {
        return Err(webauthn_err("WebAuthn credential id is malformed"));
    }
    Ok(ParsedWebauthnCredential {
        credential_id,
        x,
        y,
        sign_count: parsed.sign_count,
    })
}

/// Decode the attestation object into its auth-data bytes, statement map,
/// and format name.
fn parse_attestation_object(
    attestation_object: &[u8],
) -> crate::domain::Result<(Vec<u8>, Vec<(Cbor, Cbor)>, String)> {
    let (att_obj, _) = cbor_decode(attestation_object, 0)?;
    let Cbor::Map(ref pairs) = att_obj else {
        return Err(webauthn_err("WebAuthn attestation object is malformed"));
    };
    let fmt = match cbor_map_find(pairs, &Cbor::Text("fmt".to_string())) {
        Some(Cbor::Text(fmt)) => fmt.clone(),
        _ => return Err(webauthn_err("WebAuthn attestation object is malformed")),
    };
    let auth_data_raw: Vec<u8> = match cbor_map_find(pairs, &Cbor::Text("authData".to_string())) {
        Some(Cbor::Bytes(bytes)) => bytes.clone(),
        _ => return Err(webauthn_err("WebAuthn attestation object is malformed")),
    };
    let att_stmt = match cbor_map_find(pairs, &Cbor::Text("attStmt".to_string())) {
        Some(Cbor::Map(stmt)) => stmt.clone(),
        _ => return Err(webauthn_err("WebAuthn attestation object is malformed")),
    };
    Ok((auth_data_raw, att_stmt, fmt))
}

/// Verify a self-attested packed credential (no attestation chains).
fn verify_packed_self_attestation(
    att_stmt: &[(Cbor, Cbor)],
    auth_data_raw: &[u8],
    client_data_json: &[u8],
    point: &[u8],
) -> crate::domain::Result<()> {
    if cbor_map_find(att_stmt, &Cbor::Text("x5c".to_string())).is_some()
        || cbor_map_find(att_stmt, &Cbor::Text("ecdaaKeyId".to_string())).is_some()
    {
        return Err(webauthn_err(
            "WebAuthn attestation chains are not supported (self-attestation only)",
        ));
    }
    let alg = match cbor_map_find(att_stmt, &Cbor::Text("alg".to_string())) {
        Some(Cbor::Negative(n)) => *n,
        Some(Cbor::Unsigned(n)) => i64::try_from(*n).unwrap_or(i64::MAX),
        _ => {
            return Err(webauthn_err(
                "WebAuthn packed attestation is missing its algorithm",
            ));
        }
    };
    if alg != WEBAUTHN_COSE_ES256 {
        return Err(webauthn_err(
            "WebAuthn attestation algorithm is not supported (only ES256)",
        ));
    }
    let sig = match cbor_map_find(att_stmt, &Cbor::Text("sig".to_string())) {
        Some(Cbor::Bytes(sig)) => sig.clone(),
        _ => {
            return Err(webauthn_err(
                "WebAuthn packed attestation is missing its signature",
            ));
        }
    };
    let client_hash = sha256_of(client_data_json);
    let mut message = auth_data_raw.to_vec();
    message.extend_from_slice(&client_hash);
    if !verify_es256(point, &message, &sig) {
        return Err(webauthn_err("WebAuthn attestation signature is invalid"));
    }
    Ok(())
}

/// Verify an authentication ceremony against a stored credential.
///
/// Checks the challenge (purpose + expiry + echo), `clientDataJSON`
/// (`webauthn.get`, origin), RP ID hash, user presence, and the ES256
/// signature over `authenticatorData || sha256(clientDataJSON)`. Returns the
/// sign count for the caller to persist. Clone detection: when both the
/// stored and presented counts are non-zero the presented count must
/// strictly increase, otherwise the ceremony is refused.
pub fn verify_webauthn_assertion(
    credential: &WebauthnCredential,
    rp_id: &str,
    origin: &str,
    challenge: &WebauthnChallenge,
    now_ms: u64,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature_der: &[u8],
) -> crate::domain::Result<u32> {
    if challenge.purpose != WebauthnPurpose::Authentication {
        return Err(webauthn_err(
            "WebAuthn challenge was not minted for authentication",
        ));
    }
    if !challenge.live_at(now_ms) {
        return Err(webauthn_err("WebAuthn challenge has expired"));
    }
    if challenge.account_id != credential.account_id {
        return Err(webauthn_err(
            "WebAuthn challenge belongs to another account",
        ));
    }
    parse_client_data(
        client_data_json,
        WebauthnPurpose::Authentication.client_data_type(),
        &challenge.challenge_b64,
        origin,
    )?;
    let parsed = parse_auth_data(authenticator_data, false)?;
    if parsed.rp_id_hash != sha256_of(rp_id.as_bytes()) {
        return Err(webauthn_err(
            "WebAuthn credential relies on a different party",
        ));
    }
    let client_hash = sha256_of(client_data_json);
    let mut message = authenticator_data.to_vec();
    message.extend_from_slice(&client_hash);
    if !verify_es256(&credential.public_key_uncompressed, &message, signature_der) {
        // One generic shape: a bad signature reveals nothing about which
        // field the prover got wrong.
        return Err(webauthn_err("WebAuthn authentication failed"));
    }
    let presented = parsed.sign_count;
    if credential.sign_count != 0 && presented != 0 && presented <= credential.sign_count {
        return Err(webauthn_err(
            "WebAuthn sign count did not increase (the authenticator may be cloned)",
        ));
    }
    Ok(presented)
}

//! CBOR and ceremony bytestring helpers for WebAuthn (S11 split).
//!
//! Minimal definite-length CBOR decoder (CTAP2 attestation objects only),
//! authenticator-data parser, `clientDataJSON` parser, SHA-256 and the
//! ES256 verifier used by the two ceremony entry points in
//! [`super::webauthn`]. No behavior change; moved verbatim.

use super::totp;
use super::webauthn::{WEBAUTHN_COSE_ES256, WEBAUTHN_MAX_CEREMONY_BYTES, webauthn_err};

/// Minimal definite-length CBOR_decoder (CTAP2 attestation objects only):
/// unsigned/nnegative ints, byte/text strings, arrays, maps. Indefinite
/// lengths, tags, floats and simple values are refused Ã¢â‚¬â€ genuine
/// authenticators never emit them here.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Cbor {
    Unsigned(u64),
    Negative(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
}

fn cbor_int_arg(input: &[u8]) -> crate::domain::Result<(u64, usize)> {
    let err = || webauthn_err("WebAuthn CBOR is malformed");
    let (&head, rest) = input.split_first().ok_or_else(err)?;
    let info = head & 0x1f;
    match info {
        0..=23 => Ok((info as u64, 1)),
        24 => rest.first().map(|&b| (b as u64, 2)).ok_or_else(err),
        25 => rest
            .get(..2)
            .map(|b| (u16::from_be_bytes([b[0], b[1]]) as u64, 3))
            .ok_or_else(err),
        26 => rest
            .get(..4)
            .map(|b| (u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64, 5))
            .ok_or_else(err),
        27 => rest
            .get(..8)
            .map(|b| {
                (
                    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
                    9,
                )
            })
            .ok_or_else(err),
        _ => Err(err()),
    }
}

pub(super) fn cbor_decode(input: &[u8], depth: u8) -> crate::domain::Result<(Cbor, usize)> {
    let err = || webauthn_err("WebAuthn CBOR is malformed");
    if depth > 8 || input.is_empty() {
        return Err(err());
    }
    let major = input[0] >> 5;
    let (arg, head_len) = cbor_int_arg(input)?;
    let body = &input[head_len..];
    match major {
        0 => Ok((Cbor::Unsigned(arg), head_len)),
        1 => {
            let neg = arg
                .checked_add(1)
                .and_then(|n| i64::try_from(n).ok().map(|n| -n))
                .ok_or_else(err)?;
            Ok((Cbor::Negative(neg), head_len))
        }
        2 => {
            let len: usize = arg.try_into().map_err(|_| err())?;
            if body.len() < len || len > WEBAUTHN_MAX_CEREMONY_BYTES {
                return Err(err());
            }
            Ok((Cbor::Bytes(body[..len].to_vec()), head_len + len))
        }
        3 => {
            let len: usize = arg.try_into().map_err(|_| err())?;
            if body.len() < len || len > WEBAUTHN_MAX_CEREMONY_BYTES {
                return Err(err());
            }
            let text = std::str::from_utf8(&body[..len]).map_err(|_| err())?;
            Ok((Cbor::Text(text.to_string()), head_len + len))
        }
        4 => {
            if arg > 64 {
                return Err(err());
            }
            let mut items = Vec::with_capacity(arg as usize);
            let mut off = head_len;
            for _ in 0..arg {
                let (item, used) = cbor_decode(&input[off..], depth + 1)?;
                off += used;
                items.push(item);
            }
            Ok((Cbor::Array(items), off))
        }
        5 => {
            if arg > 64 {
                return Err(err());
            }
            let mut pairs = Vec::with_capacity(arg as usize);
            let mut off = head_len;
            for _ in 0..arg {
                let (key, used) = cbor_decode(&input[off..], depth + 1)?;
                off += used;
                let (value, used) = cbor_decode(&input[off..], depth + 1)?;
                off += used;
                pairs.push((key, value));
            }
            Ok((Cbor::Map(pairs), off))
        }
        _ => Err(err()),
    }
}

pub(super) fn cbor_map_find<'a>(map: &'a [(Cbor, Cbor)], key: &Cbor) -> Option<&'a Cbor> {
    map.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Parsed authenticator data (`authData`): the RP hash, flags, sign count,
/// and Ã¢â‚¬â€ when attested data is present Ã¢â‚¬â€ the credential id plus the ES256
/// COSE coordinates.
pub(super) struct ParsedAuthData {
    pub(super) rp_id_hash: [u8; 32],
    pub(super) sign_count: u32,
    pub(super) credential_id: Option<Vec<u8>>,
    pub(super) cose_x: Option<[u8; 32]>,
    pub(super) cose_y: Option<[u8; 32]>,
}

const AUTH_FLAG_USER_PRESENT: u8 = 0x01;
const AUTH_FLAG_ATTESTED: u8 = 0x40;

pub(super) fn parse_auth_data(
    bytes: &[u8],
    expect_attested: bool,
) -> crate::domain::Result<ParsedAuthData> {
    if bytes.len() < 37 || bytes.len() > WEBAUTHN_MAX_CEREMONY_BYTES {
        return Err(webauthn_err("WebAuthn authenticator data is malformed"));
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&bytes[..32]);
    let flags = bytes[32];
    if flags & AUTH_FLAG_USER_PRESENT == 0 {
        return Err(webauthn_err(
            "WebAuthn ceremony is missing the user-presence flag",
        ));
    }
    let sign_count = u32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]);
    let attested = flags & AUTH_FLAG_ATTESTED != 0;
    if expect_attested && !attested {
        return Err(webauthn_err(
            "WebAuthn registration is missing attested credential data",
        ));
    }
    if !attested {
        return Ok(ParsedAuthData {
            rp_id_hash,
            sign_count,
            credential_id: None,
            cose_x: None,
            cose_y: None,
        });
    }
    // Attested credential data: AAGUID(16) + credIdLen(2) + credId + COSE key.
    if bytes.len() < 55 {
        return Err(webauthn_err("WebAuthn attested data is truncated"));
    }
    let id_len = u16::from_be_bytes([bytes[53], bytes[54]]) as usize;
    if id_len == 0 || id_len > 1023 || bytes.len() < 55 + id_len {
        return Err(webauthn_err("WebAuthn credential id is malformed"));
    }
    let credential_id = bytes[55..55 + id_len].to_vec();
    let (cose, _) = cbor_decode(&bytes[55 + id_len..], 0)?;
    let Cbor::Map(ref pairs) = cose else {
        return Err(webauthn_err("WebAuthn credential key is malformed"));
    };
    // COSE keys use small integer labels: `1` = kty, `3` = alg, `-1` =
    // crv, `-2`/`-3` = x/y. Only EC2/P-256/ES256 verifies here.
    let find_int = |label: Cbor| -> Option<i64> {
        match cbor_map_find(pairs, &label) {
            Some(Cbor::Unsigned(n)) => i64::try_from(*n).ok(),
            Some(Cbor::Negative(n)) => Some(*n),
            _ => None,
        }
    };
    let find_bytes = |label: Cbor| -> Option<Vec<u8>> {
        match cbor_map_find(pairs, &label) {
            Some(Cbor::Bytes(b)) => Some(b.clone()),
            _ => None,
        }
    };
    if find_int(Cbor::Unsigned(1)) != Some(2) {
        return Err(webauthn_err(
            "WebAuthn credential key type is not supported (only P-256)",
        ));
    }
    match find_int(Cbor::Unsigned(3)) {
        Some(alg) if alg == WEBAUTHN_COSE_ES256 => {}
        Some(_) => {
            return Err(webauthn_err(
                "WebAuthn credential algorithm is not supported (only ES256)",
            ));
        }
        None => return Err(webauthn_err("WebAuthn credential key is malformed")),
    }
    if find_int(Cbor::Negative(-1)) != Some(1) {
        return Err(webauthn_err(
            "WebAuthn credential curve is not supported (only P-256)",
        ));
    }
    let x = find_bytes(Cbor::Negative(-2));
    let y = find_bytes(Cbor::Negative(-3));
    let (Some(x), Some(y)) = (x, y) else {
        return Err(webauthn_err("WebAuthn credential key is malformed"));
    };
    if x.len() != 32 || y.len() != 32 {
        return Err(webauthn_err("WebAuthn credential key is malformed"));
    }
    let mut cose_x = [0u8; 32];
    let mut cose_y = [0u8; 32];
    cose_x.copy_from_slice(&x);
    cose_y.copy_from_slice(&y);
    Ok(ParsedAuthData {
        rp_id_hash,
        sign_count,
        credential_id: Some(credential_id),
        cose_x: Some(cose_x),
        cose_y: Some(cose_y),
    })
}

/// Parse and check `clientDataJSON`: the ceremony `type`, the echoed
/// challenge (constant-time against the stored challenge Ã¢â‚¬â€ no oracle), and
/// the exact expected origin.
pub(super) fn parse_client_data(
    client_data_json: &[u8],
    expected_type: &str,
    expected_challenge_b64: &str,
    expected_origin: &str,
) -> crate::domain::Result<()> {
    if client_data_json.len() > WEBAUTHN_MAX_CEREMONY_BYTES {
        return Err(webauthn_err("WebAuthn client data is too large"));
    }
    let value: serde_json::Value = serde_json::from_slice(client_data_json)
        .map_err(|_| webauthn_err("WebAuthn client data is not valid JSON"))?;
    if value.get("type").and_then(|v| v.as_str()) != Some(expected_type) {
        return Err(webauthn_err("WebAuthn client data has the wrong type"));
    }
    let challenge = value
        .get("challenge")
        .and_then(|v| v.as_str())
        .ok_or_else(|| webauthn_err("WebAuthn client data is missing its challenge"))?;
    let challenge_ok =
        totp::constant_time_equal(challenge.as_bytes(), expected_challenge_b64.as_bytes());
    if !challenge_ok {
        return Err(webauthn_err(
            "WebAuthn challenge does not match (unknown, expired, or already used)",
        ));
    }
    if value.get("origin").and_then(|v| v.as_str()) != Some(expected_origin) {
        return Err(webauthn_err("WebAuthn origin does not match this server"));
    }
    Ok(())
}

pub(super) fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Verify an ES256 signature over `message` with an uncompressed P-256 point.
pub(super) fn verify_es256(
    uncompressed_point: &[u8],
    message: &[u8],
    signature_der: &[u8],
) -> bool {
    if uncompressed_point.len() != 65 || uncompressed_point[0] != 0x04 {
        return false;
    }
    let key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::ECDSA_P256_SHA256_FIXED,
        uncompressed_point,
    );
    key.verify(message, signature_der).is_ok()
}

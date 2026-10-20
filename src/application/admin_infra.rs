//! Admin infrastructure use cases (Phase 7.3, subagent C).
//!
//! Pure validation, normalization and network-matching helpers for the domain
//! allows/blocks, email-domain blocks, IP blocks and canonical email blocks
//! admin routes. Storage lives behind [`AdminInfraStore`] in
//! [`crate::application::ports`]; HTTP mapping lives in the endpoint module.

use std::net::IpAddr;

use crate::domain::{AdminIpSeverity, Error, Id, Result};

/// Normalize a domain the way upstream's `DomainNormalizable` does, minus
/// IDNA (this build stores the normalized ASCII-or-Unicode form as given):
/// strip whitespace and any leading `/`/trailing `/`, downcase, drop a single
/// trailing dot.
pub fn normalize_domain(raw: &str) -> String {
    let mut domain = raw.trim().to_ascii_lowercase();
    while domain.ends_with('/') || domain.ends_with('.') {
        domain.pop();
    }
    while domain.starts_with('/') {
        domain.remove(0);
    }
    domain
}

/// Validate a normalized domain, mirroring the pinned user-facing messages:
/// blank → `Validation failed: Domain can't be blank`, malformed → `Validation
/// failed: Domain is invalid`.
pub fn validate_domain(domain: &str) -> Result<()> {
    if domain.is_empty() {
        return Err(Error::InvalidInput(
            "Validation failed: Domain can't be blank".into(),
        ));
    }
    if !is_valid_domain(domain) {
        return Err(Error::InvalidInput(
            "Validation failed: Domain is invalid".into(),
        ));
    }
    Ok(())
}

fn is_valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 || domain.contains([' ', '/', '@', ':']) {
        return false;
    }
    // One label at minimum; each label 1-63 chars of alnum/hyphen/underscore,
    // not leading or trailing with a hyphen.
    let mut labels = 0;
    for label in domain.split('.') {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return false;
        }
        labels += 1;
    }
    labels >= 1 && !domain.starts_with('.')
}

/// Canonicalize an email the way upstream's `CanonicalEmail` does: downcase,
/// drop dots and any `+tag` in the local part.
pub fn canonicalize_email(email: &str) -> String {
    let email = email.trim().to_ascii_lowercase();
    match email.split_once('@') {
        Some((local, domain)) => {
            let local: String = local.chars().filter(|&c| c != '.').collect();
            let local = match local.split_once('+') {
                Some((head, _)) => head.to_string(),
                None => local,
            };
            format!("{local}@{domain}")
        }
        None => email,
    }
}

/// Hex-encoded SHA-256 of the canonical email (upstream `digest`).
pub fn canonical_email_hash(email: &str) -> String {
    crate::application::crypto::sha256_hex(canonicalize_email(email).as_bytes())
}

/// Validate an email for canonical blocking: must contain one `@` with
/// non-empty local and domain parts.
pub fn validate_canonical_email(email: &str) -> Result<()> {
    let email = email.trim();
    match email.split_once('@') {
        Some((local, domain))
            if !local.is_empty() && !domain.is_empty() && !email.contains(' ') =>
        {
            Ok(())
        }
        _ => Err(Error::InvalidInput(
            "Validation failed: Email is invalid".into(),
        )),
    }
}

/// Parsed CIDR block for an IP block row.
#[derive(Debug, Clone, Copy)]
pub struct CidrBlock {
    pub base: IpAddr,
    pub prefix: u8,
}

/// Parse upstream-style `ip` input (`"1.2.3.4"` or `"1.2.3.0/24"`, IPv4 or
/// IPv6). A bare address means a host route (`/32` or `/128`).
pub fn parse_cidr(raw: &str) -> Result<CidrBlock> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::InvalidInput(
            "Validation failed: Ip can't be blank".into(),
        ));
    }
    let (addr_text, prefix) = match raw.split_once('/') {
        Some((addr, prefix)) => {
            let prefix: u8 = prefix.trim().parse().map_err(|_| {
                Error::InvalidInput("Validation failed: Ip is invalid".into())
            })?;
            (addr.trim(), Some(prefix))
        }
        None => (raw, None),
    };
    let base: IpAddr = addr_text
        .parse()
        .map_err(|_| Error::InvalidInput("Validation failed: Ip is invalid".into()))?;
    let max = match base {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    let prefix = prefix.unwrap_or(max);
    if prefix > max {
        return Err(Error::InvalidInput("Validation failed: Ip is invalid".into()));
    }
    Ok(CidrBlock { base, prefix })
}

/// Render a parsed block in upstream `to_cidr` form (`"ip/prefix"`).
pub fn cidr_text(block: CidrBlock) -> String {
    format!("{}/{}", block.base, block.prefix)
}

/// True when `ip` falls inside `block` (family-mismatched pairs never match).
pub fn cidr_contains(block: CidrBlock, ip: IpAddr) -> bool {
    match (block.base, ip) {
        (IpAddr::V4(base), IpAddr::V4(addr)) => {
            let shift = 32 - block.prefix;
            (u32::from(base) >> shift) == (u32::from(addr) >> shift)
        }
        (IpAddr::V6(base), IpAddr::V6(addr)) => {
            let shift = 128 - block.prefix;
            (u128::from(base) >> shift) == (u128::from(addr) >> shift)
        }
        _ => false,
    }
}

/// Parse an IP block severity (`sign_up_requires_approval`, `sign_up_block`,
/// `no_access`).
pub fn parse_ip_severity(raw: &str) -> Result<AdminIpSeverity> {
    AdminIpSeverity::parse(raw.trim()).ok_or_else(|| {
        Error::InvalidInput("Validation failed: Severity is not included in the list".into())
    })
}

/// True when a request from `client_ip` must be refused outright: an
/// unexpired `no_access` block contains it (upstream `IpBlock.blocked?`, which
/// only consults the `no_access` scope).
pub fn is_no_access_blocked(blocks: &[crate::domain::AdminIpBlock], client_ip: IpAddr, now_ms: u64) -> bool {
    blocks.iter().any(|block| {
        if block.severity != AdminIpSeverity::NoAccess {
            return false;
        }
        if block.expires_at_ms.is_some_and(|exp| exp <= now_ms) {
            return false;
        }
        parse_cidr(&block.ip)
            .map(|cidr| cidr_contains(cidr, client_ip))
            .unwrap_or(false)
    })
}

/// True when registration from `client_ip` must be refused: an unexpired
/// `sign_up_block` block contains it (upstream `ip_blocked?`).
pub fn is_sign_up_blocked(blocks: &[crate::domain::AdminIpBlock], client_ip: IpAddr, now_ms: u64) -> bool {
    blocks.iter().any(|block| {
        if block.severity != AdminIpSeverity::SignUpBlock {
            return false;
        }
        if block.expires_at_ms.is_some_and(|exp| exp <= now_ms) {
            return false;
        }
        parse_cidr(&block.ip)
            .map(|cidr| cidr_contains(cidr, client_ip))
            .unwrap_or(false)
    })
}

/// Extract the client IP behind trusted proxies (Phase 1 rule): when the
/// peer is a configured trusted proxy, the leftmost `X-Forwarded-For`
/// entry wins; otherwise the peer address is the client IP.
pub fn client_ip(
    forwarded_for: Option<&str>,
    peer: Option<IpAddr>,
    trusted_proxies: &[IpAddr],
) -> Option<IpAddr> {
    let peer = peer?;
    if trusted_proxies.contains(&peer) {
        if let Some(header) = forwarded_for {
            if let Some(first) = header.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    Some(peer)
}

/// Mint a row id through the caller's id generator.
pub fn new_id(ids: &dyn crate::domain::IdGenerator) -> Id {
    ids.generate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_normalization_and_validation() {
        assert_eq!(normalize_domain("  Example.COM. "), "example.com");
        assert!(validate_domain("example.com").is_ok());
        assert!(validate_domain("").is_err());
        assert!(validate_domain("not a domain").is_err());
        assert!(validate_domain("-bad.com").is_err());
    }

    #[test]
    fn canonical_email_matches_upstream() {
        assert_eq!(canonicalize_email("Foo.Bar+tag@Example.COM"), "foobar@example.com");
        assert!(validate_canonical_email("a@b.co").is_ok());
        assert!(validate_canonical_email("no-at-sign").is_err());
    }

    #[test]
    fn cidr_parsing_and_containment() {
        let block = parse_cidr("192.168.1.0/24").unwrap();
        assert_eq!(cidr_text(block), "192.168.1.0/24");
        assert!(cidr_contains(block, "192.168.1.77".parse().unwrap()));
        assert!(!cidr_contains(block, "192.168.2.1".parse().unwrap()));
        let host = parse_cidr("10.0.0.5").unwrap();
        assert_eq!(cidr_text(host), "10.0.0.5/32");
        assert!(parse_cidr("999.1.1.1").is_err());
        assert!(parse_cidr("").is_err());
        assert!(parse_ip_severity("no_access").is_ok());
        assert!(parse_ip_severity("bogus").is_err());
    }

    #[test]
    fn trusted_proxy_client_ip() {
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        let client: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(
            client_ip(Some("203.0.113.7, 10.0.0.1"), Some(proxy), &[proxy]),
            Some(client)
        );
        // Untrusted peer: the header is ignored.
        let stranger: IpAddr = "198.51.100.9".parse().unwrap();
        assert_eq!(
            client_ip(Some("203.0.113.7"), Some(stranger), &[proxy]),
            Some(stranger)
        );
    }
}

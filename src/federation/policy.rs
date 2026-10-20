//! Address and URL policy for outbound federation requests (SSRF defence).
//!
//! Every outbound request is checked twice: the URL's shape before the request,
//! and every address the resolver returns before a connection is made. A
//! resolution that contains any blocked address is refused as a whole, so a
//! mixed answer cannot be used to reach a private host.
//!
//! The only exception is an explicit operator allowlist of CIDR ranges
//! (`--federation-allow-private`). It is empty by default, and it exists for
//! lab networks, as Mastodon's `ALLOWED_PRIVATE_ADDRESSES` does.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// Why an outbound target was refused. The variant is logged, never sent to a remote host.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("url scheme is not allowed")]
    Scheme,
    #[error("url has no host")]
    NoHost,
    #[error("url carries credentials")]
    Credentials,
    #[error("address is not allowed")]
    Address,
    #[error("malformed allowlist entry: {0}")]
    Allowlist(String),
}

/// An IP range in CIDR notation, used for the operator allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

impl<'de> serde::Deserialize<'de> for Cidr {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(de)?;
        Cidr::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for Cidr {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_string())
    }
}

impl Cidr {
    /// Parse `addr/prefix` or a bare address (a host route).
    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        let bad = || PolicyError::Allowlist(text.to_string());
        let (addr_part, prefix_part) = match text.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (text, None),
        };
        let addr: IpAddr = addr_part.trim().parse().map_err(|_| bad())?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix_part {
            Some(p) => p.trim().parse::<u8>().map_err(|_| bad())?,
            None => max,
        };
        if prefix > max {
            return Err(bad());
        }
        Ok(Self {
            network: mask(addr, prefix),
            prefix,
        })
    }

    /// Whether `ip` lies in this range. An IPv4-mapped IPv6 address matches its IPv4 range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize(ip);
        match (self.network, ip) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                mask(ip, self.prefix) == self.network
            }
            _ => false,
        }
    }
}

/// Parse a comma-separated allowlist (`10.0.0.0/8,fd00::/8`). Blank entries are skipped.
pub fn parse_allowlist(text: &str) -> Result<Vec<Cidr>, PolicyError> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Cidr::parse)
        .collect()
}

/// Keep only the network bits of `ip` for the given prefix length.
fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let m = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
            IpAddr::V4(Ipv4Addr::from(bits & m))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let m = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix as u32) };
            IpAddr::V6(Ipv6Addr::from(bits & m))
        }
    }
}

/// Map an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to its IPv4 form.
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        other => other,
    }
}

/// Whether `ip` is public unicast: safe to dial without an allowlist entry.
///
/// Blocks unspecified, loopback, private, link-local, shared (CGNAT),
/// benchmarking, documentation, multicast and reserved ranges. IPv6 forms that
/// embed an IPv4 address (mapped, NAT64, 6to4) are judged by the embedded
/// address, so `::ffff:127.0.0.1` and `64:ff9b::7f00:1` are refused too.
pub fn is_public_unicast(ip: IpAddr) -> bool {
    match normalize(ip) {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    let blocked = o[0] == 0                                  // 0.0.0.0/8 "this network"
        || o[0] == 10                                        // 10/8 private
        || o[0] == 127                                       // 127/8 loopback
        || (o[0] == 169 && o[1] == 254)                      // 169.254/16 link-local (cloud metadata)
        || (o[0] == 172 && (16..=31).contains(&o[1]))        // 172.16/12 private
        || (o[0] == 192 && o[1] == 168)                      // 192.168/16 private
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)           // 192.0.0/24 IETF protocol assignments
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)           // 192.0.2/24 documentation
        || (o[0] == 100 && (64..=127).contains(&o[1]))       // 100.64/10 shared (CGNAT)
        || (o[0] == 198 && (18..=19).contains(&o[1]))        // 198.18/15 benchmarking
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)        // 198.51.100/24 documentation
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)         // 203.0.113/24 documentation
        || o[0] >= 224; // multicast 224/4, reserved 240/4, broadcast
    !blocked
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // Embedded IPv4 forms: judged by the IPv4 address they carry.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
        // 64:ff9b::/96 NAT64
        return is_public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[0] == 0x2002 {
        // 2002::/16 6to4 carries the IPv4 address in segments 1 and 2.
        return is_public_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    if (s[0] & 0xffc0) == 0xfe80 {
        return false; // fe80::/10 link-local
    }
    if (s[0] & 0xfe00) == 0xfc00 {
        return false; // fc00::/7 unique local
    }
    if s[0] == 0x2001 && s[1] == 0x0db8 {
        return false; // 2001:db8::/32 documentation
    }
    if s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0 {
        return false; // 100::/64 discard-only
    }
    if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        return false; // ::/96 (deprecated IPv4-compatible and friends)
    }
    true
}

/// Whether an address may be dialled: public unicast, or inside the operator allowlist.
pub fn address_allowed(ip: IpAddr, allowlist: &[Cidr]) -> bool {
    is_public_unicast(ip) || allowlist.iter().any(|c| c.contains(ip))
}

/// Check a URL's shape before any request: scheme, host, no credentials, and an
/// IP-literal host judged directly (no DNS is involved for it). Hostnames are
/// checked again at connect time by the resolver.
pub fn check_url(url: &Url, allow_http: bool, allowlist: &[Cidr]) -> Result<(), PolicyError> {
    match url.scheme() {
        "https" => {}
        "http" if allow_http => {}
        _ => return Err(PolicyError::Scheme),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PolicyError::Credentials);
    }
    match url.host() {
        None => Err(PolicyError::NoHost),
        Some(Host::Ipv4(v4)) => check_ip(IpAddr::V4(v4), allowlist),
        Some(Host::Ipv6(v6)) => check_ip(IpAddr::V6(v6), allowlist),
        Some(Host::Domain(domain)) => {
            if domain.is_empty() {
                Err(PolicyError::NoHost)
            } else {
                Ok(())
            }
        }
    }
}

fn check_ip(ip: IpAddr, allowlist: &[Cidr]) -> Result<(), PolicyError> {
    if address_allowed(ip, allowlist) {
        Ok(())
    } else {
        Err(PolicyError::Address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn blocks_loopback_private_link_local_and_unspecified() {
        for blocked in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "64:ff9b::7f00:1",
            "2002:7f00:0001::1",
            "100::1",
        ] {
            assert!(!is_public_unicast(ip(blocked)), "{blocked} must be blocked");
        }
    }

    #[test]
    fn allows_public_addresses() {
        for public in ["8.8.8.8", "1.1.1.1", "172.15.0.1", "172.32.0.1", "100.63.0.1", "2606:4700::1111", "::ffff:8.8.8.8"] {
            assert!(is_public_unicast(ip(public)), "{public} must be allowed");
        }
    }

    #[test]
    fn allowlist_admits_only_listed_ranges() {
        let list = parse_allowlist("172.28.0.0/24, fd00::/8").unwrap();
        assert!(address_allowed(ip("172.28.0.10"), &list));
        assert!(!address_allowed(ip("172.28.1.10"), &list));
        assert!(address_allowed(ip("fd00::5"), &list));
        assert!(!address_allowed(ip("127.0.0.1"), &list));
        assert!(address_allowed(ip("::ffff:172.28.0.10"), &list));
    }

    #[test]
    fn allowlist_rejects_malformed_entries() {
        assert!(parse_allowlist("10.0.0.0/33").is_err());
        assert!(parse_allowlist("not-an-ip").is_err());
        assert!(parse_allowlist("").unwrap().is_empty());
    }

    #[test]
    fn url_shape_requires_https_unless_allowed() {
        let https = Url::parse("https://remote.example/users/a").unwrap();
        let http = Url::parse("http://remote.example/users/a").unwrap();
        assert!(check_url(&https, false, &[]).is_ok());
        assert_eq!(check_url(&http, false, &[]), Err(PolicyError::Scheme));
        assert!(check_url(&http, true, &[]).is_ok());
        let ftp = Url::parse("ftp://remote.example/").unwrap();
        assert_eq!(check_url(&ftp, true, &[]), Err(PolicyError::Scheme));
    }

    #[test]
    fn url_shape_refuses_credentials_and_ip_literals() {
        let creds = Url::parse("https://user:pw@remote.example/").unwrap();
        assert_eq!(check_url(&creds, false, &[]), Err(PolicyError::Credentials));
        for literal in ["https://127.0.0.1/", "https://[::1]/", "https://0.0.0.0/", "https://169.254.169.254/latest"] {
            let url = Url::parse(literal).unwrap();
            assert_eq!(check_url(&url, false, &[]), Err(PolicyError::Address), "{literal}");
        }
        let lab = parse_allowlist("127.0.0.0/8").unwrap();
        let loopback = Url::parse("https://127.0.0.1:8443/").unwrap();
        assert!(check_url(&loopback, false, &lab).is_ok());
    }
}

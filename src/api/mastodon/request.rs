//! Request helpers: client IP resolution behind trusted proxies.

use std::net::IpAddr;

use axum::http::HeaderMap;

/// Resolve the effective client IP.
///
/// `X-Forwarded-For` is honored only when the immediate peer is a trusted
/// proxy; otherwise a client could spoof its address. The left-most address in
/// the header is used.
pub fn client_ip(headers: &HeaderMap, peer: IpAddr, trusted_proxies: &[IpAddr]) -> IpAddr {
    if !trusted_proxies.contains(&peer) {
        return peer;
    }
    let Some(value) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) else {
        return peer;
    };
    let Some(first) = value.split(',').next() else {
        return peer;
    };
    first.trim().parse().unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_xff(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn untrusted_peer_header_is_ignored() {
        let peer: IpAddr = "9.9.9.9".parse().unwrap();
        let headers = headers_with_xff("1.2.3.4");
        assert_eq!(client_ip(&headers, peer, &[]), peer);
    }

    #[test]
    fn trusted_peer_uses_leftmost_forwarded_address() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let headers = headers_with_xff("1.2.3.4, 10.0.0.9");
        assert_eq!(
            client_ip(&headers, peer, &[peer]),
            "1.2.3.4".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn trusted_peer_without_header_falls_back() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!(client_ip(&HeaderMap::new(), peer, &[peer]), peer);
    }
}

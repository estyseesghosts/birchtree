//! Request helpers: client IP resolution behind trusted proxies.
//!
//! The single chain rule lives in
//! [`crate::api::mastodon::rate_limits::resolve_client_ip`]; this helper
//! collects every `X-Forwarded-For` line and delegates to it so all callers
//! share one trust decision.

use std::net::IpAddr;

use axum::http::HeaderMap;

use super::rate_limits::resolve_client_ip;

/// Resolve the effective client IP.
///
/// `X-Forwarded-For` is honored only when the immediate peer is a trusted
/// proxy; otherwise a client could spoof its address. See
/// [`resolve_client_ip`] for the chain rule (right-to-left walk skipping
/// trusted hops; garbage ends the walk at the last verified hop).
pub fn client_ip(headers: &HeaderMap, peer: IpAddr, trusted_proxies: &[IpAddr]) -> IpAddr {
    let mut forwarded: Vec<&str> = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        if let Ok(text) = value.to_str() {
            forwarded.push(text);
        }
    }
    resolve_client_ip(Some(peer), &forwarded, trusted_proxies).unwrap_or(peer)
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
    fn trusted_peer_walks_the_chain_right_to_left() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        // The proxy appended `10.0.0.9`; the leading entry is client-chosen
        // and must not win over the verified hop.
        let headers = headers_with_xff("1.2.3.4, 10.0.0.9");
        assert_eq!(
            client_ip(&headers, peer, &[peer]),
            "10.0.0.9".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn forged_leading_entry_cannot_override_the_verified_hop() {
        let peer: IpAddr = "10.0.0.2".parse().unwrap();
        let trusted: Vec<IpAddr> = vec![peer, "10.0.0.1".parse().unwrap()];
        let headers = headers_with_xff("9.9.9.9, 198.51.100.7, 10.0.0.1");
        assert_eq!(
            client_ip(&headers, peer, &trusted),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn garbage_hop_stops_at_the_last_verified_hop() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let headers = headers_with_xff("not-an-ip");
        assert_eq!(client_ip(&headers, peer, &[peer]), peer);
    }

    #[test]
    fn trusted_peer_without_header_falls_back() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!(client_ip(&HeaderMap::new(), peer, &[peer]), peer);
    }
}

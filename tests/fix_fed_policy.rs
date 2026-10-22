//! P1 IPv6 policy gaps (IANA Special-Purpose Registry alignment) + P2
//! signed-redirect recipient policy (same-origin signing).
//!
//! Pure-policy tests: no network, no fixture servers. Address cases assert
//! [`policy::is_public_unicast`]; redirect cases assert
//! [`media::redirect_keeps_signature`]. The resolver's any-address rule
//! (refuse the name when any resolved IP is prohibited) lives in
//! `federation::net` and is covered by inspection: it collects every address
//! from `lookup_host` and refuses the whole answer when any one of them fails
//! [`policy::address_allowed`].

use std::net::IpAddr;

use birchtree::federation::media;
use birchtree::federation::policy;
use url::Url;

fn ip(text: &str) -> IpAddr {
    text.parse().expect("test address parses")
}

fn url(text: &str) -> Url {
    Url::parse(text).expect("test url parses")
}

#[test]
fn translation_prefixes_are_judged_by_the_embedded_ipv4_address() {
    for blocked in [
        "::ffff:127.0.0.1",
        "::ffff:10.1.2.3",
        "::ffff:169.254.169.254",
        "64:ff9b::7f00:1",
        "64:ff9b::a00:1",
        "64:ff9b::c0a8:101",
    ] {
        assert!(
            !policy::is_public_unicast(ip(blocked)),
            "{blocked} must be blocked"
        );
    }
    for public in ["::ffff:8.8.8.8", "64:ff9b::808:808"] {
        assert!(
            policy::is_public_unicast(ip(public)),
            "{public} must be allowed"
        );
    }
}

/// Local-use `64:ff9b:1::/48` (RFC 8215) is denied outright with no
/// embedded-v4 check: even a public-looking tail is refused, and only an
/// allowlist entry admits it (same as any other private range).
#[test]
fn local_use_nat64_is_denied_regardless_of_tail() {
    for blocked in [
        "64:ff9b:1::7f00:1",
        "64:ff9b:1::a00:1",
        "64:ff9b:1::808:808",
        "64:ff9b:1::101:101",
        "64:ff9b:1:ffff:ffff:ffff:ffff:ffff",
    ] {
        assert!(
            !policy::is_public_unicast(ip(blocked)),
            "{blocked} must be blocked"
        );
    }
    // Well-known `64:ff9b::/96` keeps the embedded-v4 check: a public tail
    // stays dialable.
    assert!(policy::is_public_unicast(ip("64:ff9b::808:808")));
    // An allowlist entry is the only way through for the local-use prefix.
    let list = policy::parse_allowlist("64:ff9b:1::/48").expect("allowlist parses");
    assert!(policy::address_allowed(ip("64:ff9b:1::808:808"), &list));
    assert!(!policy::address_allowed(ip("64:ff9b:1::808:808"), &[]));
}

#[test]
fn historic_transition_mechanisms_are_never_public() {
    // 6to4 is refused outright, even when the embedded IPv4 is public: the
    // prefix itself is not globally reachable (IANA global=false, RFC 7526).
    for blocked in [
        "2002::1",
        "2002:7f00:1::1",
        "2002:808:808::1",
        "2002:c000:201::1",
        "2001::1",
        "2001:0:4136:e378:8000:63bf:3fff:fdd2",
    ] {
        assert!(
            !policy::is_public_unicast(ip(blocked)),
            "{blocked} must be blocked"
        );
    }
}

#[test]
fn documentation_benchmarking_and_protocol_ranges_are_never_public() {
    for blocked in [
        "2001:db8::1",
        "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff",
        "3fff::1",
        "3fff::abcd",
        "3fff:0fff::1",
        "2001:2::1",
        "2001:1::1",
        "2001:3::1",
        "2001:4:112::1",
        "2001:10::1",
        "2001:20::1",
        "2001:1ff:ffff::1",
        "2620:4f:8000::1",
    ] {
        assert!(
            !policy::is_public_unicast(ip(blocked)),
            "{blocked} must be blocked"
        );
    }
}

#[test]
fn scoped_multicast_and_reserved_ranges_stay_blocked() {
    for blocked in [
        "::",
        "::1",
        "fe80::1",
        "fec0::1",
        "fc00::1",
        "fd12:3456::1",
        "ff02::1",
        "ff05::1",
        "100::1",
        "3ffe::1",
        "5f00::1",
        "::0.0.0.1",
    ] {
        assert!(
            !policy::is_public_unicast(ip(blocked)),
            "{blocked} must be blocked"
        );
    }
}

#[test]
fn public_unicast_stays_dialable() {
    for public in [
        "8.8.8.8",
        "1.1.1.1",
        "2606:4700:4700::1111",
        "2001:4860:4860::8888",
        "2001:19e8::1",
    ] {
        assert!(
            policy::is_public_unicast(ip(public)),
            "{public} must be allowed"
        );
    }
}

#[test]
fn allowlist_still_admits_only_listed_ranges() {
    let list = policy::parse_allowlist("fd00::/8").expect("allowlist parses");
    assert!(policy::address_allowed(ip("fd00::5"), &list));
    assert!(!policy::address_allowed(ip("fe80::1"), &list));
    assert!(!policy::address_allowed(ip("2001::1"), &list));
    assert!(!policy::address_allowed(ip("64:ff9b:1::7f00:1"), &list));
}

#[test]
fn redirect_signing_stays_on_same_origin_only() {
    let first = url("https://media.example/attachments/1.png");
    assert!(media::redirect_keeps_signature(
        &first,
        &url("https://media.example/attachments/2.png")
    ));
    assert!(media::redirect_keeps_signature(&first, &first));
    assert!(media::redirect_keeps_signature(
        &first,
        &url("https://media.example:443/other.png")
    ));
    assert!(!media::redirect_keeps_signature(
        &first,
        &url("https://cdn.other/2.png")
    ));
    assert!(!media::redirect_keeps_signature(
        &first,
        &url("https://media.example:8443/2.png")
    ));
    assert!(!media::redirect_keeps_signature(
        &first,
        &url("http://media.example/2.png")
    ));
    assert!(!media::redirect_keeps_signature(
        &first,
        &url("https://media.example.evil.test/2.png")
    ));
}

#[test]
fn private_hops_are_refused_while_cross_origin_public_hops_go_unsigned() {
    // SSRF layer: a private literal is never requested at all.
    let private_hop = url("https://10.0.0.5/secret.png");
    assert_eq!(
        policy::check_url(&private_hop, true, &[]),
        Err(policy::PolicyError::Address)
    );
    let link_local_hop = url("https://[fe80::1]/secret.png");
    assert_eq!(
        policy::check_url(&link_local_hop, true, &[]),
        Err(policy::PolicyError::Address)
    );
    // Recipient layer: a public but unrelated hop passes the SSRF policy yet
    // loses the signature.
    let first = url("https://media.example/a.png");
    let unrelated = url("https://cdn.other/b.png");
    assert!(policy::check_url(&unrelated, false, &[]).is_ok());
    assert!(!media::redirect_keeps_signature(&first, &unrelated));
}

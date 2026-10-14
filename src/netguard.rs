//! One host classifier for outbound fetches (LLM endpoints, JWKS).
//!
//! Hosts are normalized first: brackets removed, one trailing dot stripped,
//! lower-cased. This checks the literal host only; a public name that resolves
//! to a private address is not caught here.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Brackets removed, one trailing dot stripped, ASCII lower-cased.
pub fn normalize_host(host: &str) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.strip_suffix('.').unwrap_or(host);
    host.to_ascii_lowercase()
}

/// `localhost`, `*.localhost`, 127.0.0.0/8 and `::1`.
pub fn is_loopback_host(host: &str) -> bool {
    let host = normalize_host(host);
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return ip.is_loopback();
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        return ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback());
    }
    false
}

/// True for anything that is not a public internet host: loopback, private,
/// link-local, CGNAT, reserved and special-purpose ranges, multicast,
/// single-label names and `.localhost`/`.local`/`.internal` names.
pub fn is_non_public_host(host: &str) -> bool {
    let host = normalize_host(host);
    if host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".internal")
        || host.ends_with(".local")
    {
        return true;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return non_public_v4(ip);
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        return non_public_v6(ip);
    }
    // Single-label names resolve through local search domains.
    !host.contains('.')
}

fn non_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15
        || o[0] >= 224 // 224.0.0.0/3: multicast, reserved, broadcast
}

fn non_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return non_public_v4(v4);
    }
    // ::/96 IPv4-compatible (deprecated), e.g. ::7f00:1.
    if s[..6].iter().all(|&x| x == 0) {
        return true;
    }
    (s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0)) // 64:ff9b::/96 NAT64
        || s[0] == 0x2002 // 2002::/16 6to4
        || (s[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local
        || (s[0] & 0xff00) == 0xff00 // ff00::/8 multicast
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_dots_and_case_do_not_bypass() {
        for host in [
            "localhost.",
            "LOCALHOST",
            "a.localhost.",
            "idp.internal.",
            "metadata.google.internal.",
            "foo.local.",
            "Foo.LOCAL",
            "intranet.",
        ] {
            assert!(is_non_public_host(host), "{host}");
        }
        assert!(is_loopback_host("localhost."));
        assert!(is_loopback_host("[::1]"));
        assert!(!is_non_public_host("auth.example.com."));
        assert!(!is_non_public_host("api.openai.com"));
    }

    #[test]
    fn special_ranges_are_blocked() {
        for host in [
            "[::7f00:1]",
            "::7f00:1",
            "[64:ff9b::a9fe:a9fe]",
            "2002:7f00:1::",
            "fec0::1",
            "ff02::1",
            "fd00::1",
            "fe80::1",
            "::ffff:169.254.169.254",
            "192.0.0.8",
            "198.18.0.1",
            "198.19.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "100.64.0.1",
            "0.0.0.0",
            "169.254.169.254",
            "10.0.0.1",
            "127.0.0.2",
        ] {
            assert!(is_non_public_host(host), "{host}");
        }
        for host in ["8.8.8.8", "198.20.0.1", "192.0.1.1", "2001:4860:4860::8888"] {
            assert!(!is_non_public_host(host), "{host}");
        }
    }
}

#[cfg(test)]
mod url_tests {
    #[test]
    fn jwks_and_llm_refuse_trailing_dot_urls() {
        for uri in [
            "https://localhost./jwks.json",
            "https://a.localhost./jwks.json",
            "https://idp.internal./jwks.json",
            "https://metadata.google.internal./jwks.json",
            "https://foo.local./jwks.json",
            "https://[::7f00:1]/jwks.json",
        ] {
            assert!(crate::runtime::jwks::HttpsJwks::new(uri).is_err(), "{uri}");
            let host = url::Url::parse(uri)
                .unwrap()
                .host_str()
                .unwrap()
                .to_string();
            assert!(crate::llm::is_private_host(&host), "{uri}");
        }
        assert!(
            crate::runtime::jwks::HttpsJwks::new("https://auth.example.com./jwks.json").is_ok()
        );
    }
}

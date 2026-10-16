//! One host classifier for outbound fetches (LLM endpoints, JWKS).
//!
//! Hosts are normalized first: brackets removed, one trailing dot stripped,
//! lower-cased. [`is_non_public_host`] checks the literal host; a public name
//! that resolves to a private address (DNS rebinding) is caught at connect
//! time by [`PublicOnlyResolver`], which every guarded reqwest client uses: it
//! fails the lookup if any resolved address is non-public, so the client only
//! ever connects to addresses that were checked.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

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
    // After the one trailing dot is stripped, an empty label (`localhost..`,
    // `a..b`, `.x`) is malformed and refused.
    if host.split('.').any(str::is_empty) {
        return true;
    }
    // Single-label names resolve through local search domains.
    !host.contains('.')
}

/// True for a non-public address (see [`is_non_public_host`]).
pub fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => non_public_v4(v4),
        IpAddr::V6(v6) => non_public_v6(v6),
    }
}

type LookupFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;
type Lookup = dyn Fn(String) -> LookupFuture + Send + Sync;

/// reqwest DNS resolver that refuses a name if any address it resolves to is
/// non-public. reqwest connects only to the addresses returned here, so a
/// rebinding answer cannot slip in between the check and the connect.
#[derive(Clone)]
pub struct PublicOnlyResolver {
    lookup: Arc<Lookup>,
}

impl Default for PublicOnlyResolver {
    fn default() -> Self {
        Self::with_lookup(|name| {
            Box::pin(
                async move { Ok(tokio::net::lookup_host((name.as_str(), 0)).await?.collect()) },
            )
        })
    }
}

impl PublicOnlyResolver {
    /// A resolver over a custom lookup (tests use a fake DNS).
    pub fn with_lookup(lookup: impl Fn(String) -> LookupFuture + Send + Sync + 'static) -> Self {
        Self {
            lookup: Arc::new(lookup),
        }
    }
}

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let name = name.as_str().to_string();
        let fut = (self.lookup)(name.clone());
        Box::pin(async move {
            let addrs = fut.await?;
            if addrs.is_empty() {
                return Err(format!("{name} did not resolve").into());
            }
            if let Some(bad) = addrs.iter().find(|a| is_non_public_ip(a.ip())) {
                return Err(format!(
                    "{name} resolves to non-public address {}; refusing to connect",
                    bad.ip()
                )
                .into());
            }
            let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// Applies the outbound guard to a client builder: every hostname is resolved
/// through [`PublicOnlyResolver`] and proxies are disabled (a proxy would do
/// its own, unchecked, DNS lookup).
pub fn guard_client(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    guard_client_with(builder, PublicOnlyResolver::default())
}

pub fn guard_client_with(
    builder: reqwest::ClientBuilder,
    resolver: PublicOnlyResolver,
) -> reqwest::ClientBuilder {
    builder.no_proxy().dns_resolver(Arc::new(resolver))
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
        || (o[0] == 192 && o[1] == 0 && o[2] == 2) // 192.0.2.0/24 TEST-NET-1
        || (o[0] == 192 && o[1] == 88 && o[2] == 99) // 192.88.99.0/24 6to4 relay
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15
        || (o[0] == 198 && o[1] == 51 && o[2] == 100) // 198.51.100.0/24 TEST-NET-2
        || (o[0] == 203 && o[1] == 0 && o[2] == 113) // 203.0.113.0/24 TEST-NET-3
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
    // ::ffff:0:0/96 IPv4-translated, e.g. ::ffff:0:7f00:1.
    if s[..4].iter().all(|&x| x == 0) && s[4] == 0xffff && s[5] == 0 {
        return true;
    }
    // 64:ff9b::/96 NAT64
    // 64:ff9b:1::/48 local NAT64
    (s[0] == 0x64 && s[1] == 0xff9b && (s[2..6].iter().all(|&x| x == 0) || s[2] == 1))
        || (s[0] == 0x100 && s[1..4].iter().all(|&x| x == 0)) // 100::/64 discard
        || (s[0] == 0x2001 && s[1] == 0) // 2001::/32 Teredo
        || (s[0] == 0x2001 && s[1] == 2 && s[2] == 0) // 2001:2::/48 benchmarking
        || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x10) // 2001:10::/28 ORCHID
        || (s[0] == 0x2001 && (s[1] & 0xfff0) == 0x20) // 2001:20::/28 ORCHIDv2
        || (s[0] == 0x2001 && s[1] == 0xdb8) // 2001:db8::/32 documentation
        || s[0] == 0x2002 // 2002::/16 6to4
        || (s[0] == 0x3fff && (s[1] & 0xf000) == 0) // 3fff::/20 documentation
        || s[0] == 0x5f00 // 5f00::/16 SRv6 SIDs
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
            "localhost..",
            "idp.internal..",
            "metadata.google.internal..",
            "foo.local..",
            "a..example.com",
            ".example.com",
            "example.com..",
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
            "[::ffff:0:7f00:1]",
            "::ffff:0:a9fe:a9fe",
            "100::1",
            "100::ffff:1",
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

    #[test]
    fn nat64_teredo_documentation_and_relay_ranges_are_blocked() {
        for host in [
            "64:ff9b:1::1",
            "64:ff9b:1:ffff::a9fe:a9fe",
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:db8::1",
            "2001:db8:ffff::1",
            "2001:2::1",
            "2001:2:0:ffff::1",
            "2001:10::1",
            "2001:1f::1",
            "2001:20::1",
            "2001:2f:ffff::1",
            "3fff::",
            "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff",
            "5f00::",
            "5f00:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "192.0.2.1",
            "192.0.2.255",
            "192.88.99.1",
            "192.88.99.255",
            "198.51.100.7",
            "203.0.113.9",
        ] {
            assert!(is_non_public_host(host), "{host}");
        }
        // Neighbours stay public.
        for host in [
            "64:ff9b:2::1",
            "2001:1::1",
            "2001:db9::1",
            "2001:2:1::1",
            "2001:30::1",
            "2001:f::1",
            "3ffe:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "3fff:1000::",
            "5eff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "5f01::",
            "192.0.3.1",
            "192.88.98.1",
            "192.88.100.1",
        ] {
            assert!(!is_non_public_host(host), "{host}");
        }
    }

    fn fake_dns(ip: IpAddr) -> PublicOnlyResolver {
        PublicOnlyResolver::with_lookup(move |_name| {
            Box::pin(async move { Ok(vec![SocketAddr::new(ip, 0)]) })
        })
    }

    #[tokio::test]
    async fn resolver_refuses_names_that_resolve_to_non_public_addresses() {
        use reqwest::dns::Resolve;
        for ip in [
            "127.0.0.1",
            "169.254.169.254",
            "10.0.0.1",
            "::1",
            "2001:db8::1",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            let name: reqwest::dns::Name = "rebind.example.com".parse().unwrap();
            assert!(fake_dns(ip).resolve(name).await.is_err(), "{ip}");
        }
        // Mixed answers fail too: one bad address is enough.
        let mixed = PublicOnlyResolver::with_lookup(|_| {
            Box::pin(async {
                Ok(vec![
                    "93.184.216.34:0".parse().unwrap(),
                    "127.0.0.1:0".parse().unwrap(),
                ])
            })
        });
        let name: reqwest::dns::Name = "rebind.example.com".parse().unwrap();
        assert!(mixed.resolve(name).await.is_err());
        let name: reqwest::dns::Name = "ok.example.com".parse().unwrap();
        let got: Vec<_> = fake_dns("93.184.216.34".parse().unwrap())
            .resolve(name)
            .await
            .unwrap()
            .collect();
        assert_eq!(got, vec!["93.184.216.34:0".parse::<SocketAddr>().unwrap()]);
    }

    #[tokio::test]
    async fn resolver_classifies_ipv4_mapped_and_nat64_by_embedded_v4() {
        use reqwest::dns::Resolve;
        // IPv4-mapped (::ffff:x): classified by the embedded IPv4.
        for ip in [
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            let name: reqwest::dns::Name = "rebind.example.com".parse().unwrap();
            assert!(fake_dns(ip).resolve(name).await.is_err(), "{ip}");
        }
        // Mapped public v4 stays allowed.
        {
            let ip: IpAddr = "::ffff:8.8.8.8".parse().unwrap();
            let name: reqwest::dns::Name = "ok.example.com".parse().unwrap();
            let got: Vec<_> = fake_dns(ip).resolve(name).await.unwrap().collect();
            assert_eq!(got, vec![SocketAddr::new(ip, 0)]);
        }
        // NAT64 (64:ff9b::/96) of private/link-local IPv4 is refused.
        for ip in [
            "64:ff9b::a9fe:a9fe", // 169.254.169.254
            "64:ff9b::7f00:1",    // 127.0.0.1
            "64:ff9b::a00:1",     // 10.0.0.1
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            let name: reqwest::dns::Name = "rebind.example.com".parse().unwrap();
            assert!(fake_dns(ip).resolve(name).await.is_err(), "{ip}");
        }
    }

    #[tokio::test]
    async fn real_localhost_lookup_is_refused() {
        use reqwest::dns::Resolve;
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        assert!(PublicOnlyResolver::default().resolve(name).await.is_err());
    }

    #[tokio::test]
    async fn guarded_client_never_connects_to_a_rebound_name() {
        // A public-looking name whose DNS answer is 127.0.0.1, pointed at a
        // live local listener: the request must fail without connecting.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = tokio::spawn(async move { listener.accept().await.is_ok() });
        let client = guard_client_with(
            reqwest::Client::builder(),
            fake_dns("127.0.0.1".parse().unwrap()),
        )
        .build()
        .unwrap();
        let err = client
            .get(format!("http://rebind.example.com:{port}/"))
            .send()
            .await
            .unwrap_err();
        assert!(
            err.is_connect() || err.to_string().contains("error"),
            "{err}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!accepted.is_finished(), "the client connected to 127.0.0.1");
        accepted.abort();
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
            "https://localhost../jwks.json",
            "https://idp.internal../jwks.json",
            "https://metadata.google.internal../jwks.json",
            "https://foo.local../jwks.json",
            "https://[::ffff:0:7f00:1]/jwks.json",
            "https://[100::1]/jwks.json",
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

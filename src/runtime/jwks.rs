//! JWKS key source with the spec §4.1 (decision 44) cache rules:
//! keys are cached for 10 minutes; an unknown `kid` triggers an immediate
//! refetch, at most once every 60 seconds; when fetching fails, the old cache
//! is used for at most 1 hour after the last successful fetch, after which
//! every token is rejected. Only keys of the configured alg are kept
//! (ES256: EC P-256; RS256: RSA >= 2048-bit), filtered when the document is
//! parsed, and every key needs a `kid`. A `kid` that appears more than once
//! in the document is not used at all (decision 139).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey};
use serde_json::Value;
use tokio::sync::Mutex;

pub const FRESH_FOR: Duration = Duration::from_secs(600);
pub const REFETCH_GAP: Duration = Duration::from_secs(60);
pub const STALE_LIMIT: Duration = Duration::from_secs(3600);
/// Largest JWKS document read.
pub const MAX_JWKS_BYTES: usize = 64 * 1024;
/// Most keys kept from one document.
pub const MAX_KEYS: usize = 32;

/// Where the JWKS document comes from.
#[async_trait]
pub trait JwksSource: Send + Sync {
    async fn fetch(&self) -> Result<Value, String>;
}

/// Fetches over HTTPS only.
pub struct HttpsJwks {
    uri: String,
    client: reqwest::Client,
}

/// True for hosts a JWKS fetch must not reach: loopback, private, link-local
/// (cloud metadata), CGNAT, unspecified, single-label and `.local`/
/// `.internal`/`.localhost` names (SSRF). Uses the shared
/// [`crate::netguard`] classifier (trailing dots and case normalized).
pub fn non_public_host(host: &str) -> bool {
    crate::netguard::is_non_public_host(host)
}

impl HttpsJwks {
    /// Public https hosts only.
    pub fn new(uri: &str) -> Result<Self, String> {
        Self::with_policy(uri, false)
    }

    /// `allow_private` is the development-only escape hatch
    /// (`VIBE_JWT_ALLOW_PRIVATE_HOST=true` with a development `VIBE_ENV`).
    pub fn with_policy(uri: &str, allow_private: bool) -> Result<Self, String> {
        let parsed = url::Url::parse(uri).map_err(|err| format!("jwks-uri: {err}"))?;
        if parsed.scheme() != "https" {
            return Err("jwks-uri must use https".into());
        }
        let host = parsed.host_str().unwrap_or("");
        if !allow_private && non_public_host(host) {
            return Err(format!(
                "jwks-uri host {host} is loopback, private or link-local; use a public https host"
            ));
        }
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none());
        if !allow_private {
            // Also check every address the name resolves to (DNS rebinding).
            builder = crate::netguard::guard_client(builder);
        }
        let client = builder.build().map_err(|err| err.to_string())?;
        Ok(Self {
            uri: uri.to_string(),
            client,
        })
    }
}

#[async_trait]
impl JwksSource for HttpsJwks {
    async fn fetch(&self) -> Result<Value, String> {
        let resp = self
            .client
            .get(&self.uri)
            .send()
            .await
            .map_err(|err| err.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("jwks fetch returned {}", resp.status()));
        }
        let mut resp = resp;
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|err| err.to_string())? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_JWKS_BYTES {
                return Err(format!("jwks response exceeds {MAX_JWKS_BYTES} bytes"));
            }
        }
        serde_json::from_slice(&body).map_err(|err| err.to_string())
    }
}

#[derive(Clone)]
pub struct JwkKey {
    pub alg: Algorithm,
    pub key: DecodingKey,
}

struct State {
    keys: HashMap<String, JwkKey>,
    fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
    /// A fetch is running; others keep using the current cache meanwhile.
    in_flight: bool,
}

pub struct JwksCache {
    source: Arc<dyn JwksSource>,
    /// The only alg whose keys are kept.
    alg: Algorithm,
    state: Mutex<State>,
}

impl JwksCache {
    /// ES256 keys only (the default `VIBE_JWT_ALG`).
    pub fn new(source: Arc<dyn JwksSource>) -> Self {
        Self::with_alg(source, Algorithm::ES256)
    }

    /// Keeps only keys of `alg` (decision 139).
    pub fn with_alg(source: Arc<dyn JwksSource>, alg: Algorithm) -> Self {
        Self {
            source,
            alg,
            state: Mutex::new(State {
                keys: HashMap::new(),
                fetched_at: None,
                last_attempt: None,
                in_flight: false,
            }),
        }
    }

    pub async fn key(&self, kid: &str) -> Option<JwkKey> {
        self.key_at(kid, Instant::now()).await
    }

    /// `key` with an explicit clock, for tests.
    ///
    /// Single-flight: the cache lock is never held across the network call.
    /// One caller fetches; concurrent callers answer from the current cache.
    pub async fn key_at(&self, kid: &str, now: Instant) -> Option<JwkKey> {
        let fetch = {
            let mut state = self.state.lock().await;
            let fresh = state
                .fetched_at
                .is_some_and(|at| now.duration_since(at) < FRESH_FOR);
            let known = state.keys.contains_key(kid);
            let may_refetch = state
                .last_attempt
                .is_none_or(|at| now.duration_since(at) >= REFETCH_GAP);
            let go = (!fresh || !known) && may_refetch && !state.in_flight;
            if go {
                state.last_attempt = Some(now);
                state.in_flight = true;
            }
            go
        };
        if fetch {
            let result = self.source.fetch().await;
            let mut state = self.state.lock().await;
            state.in_flight = false;
            match result {
                Ok(doc) => {
                    state.keys = parse_jwks(&doc, self.alg);
                    state.fetched_at = Some(now);
                }
                Err(err) => tracing::warn!(%err, "JWKS fetch failed"),
            }
        }
        let state = self.state.lock().await;
        let usable = state
            .fetched_at
            .is_some_and(|at| now.duration_since(at) < STALE_LIMIT);
        if !usable {
            return None;
        }
        state.keys.get(kid).cloned()
    }
}

/// Parses a JWKS document, keeping only signing keys of `alg`.
///
/// Keys are filtered by `alg` (kty, crv and `alg` must match) before they
/// are put in the kid map, so a key of another type is never stored. A
/// `kid` that appears more than once among the document's keys (of any
/// type) is ambiguous and is not used at all; a warning is logged. The
/// result does not depend on key order.
pub fn parse_jwks(doc: &Value, alg: Algorithm) -> HashMap<String, JwkKey> {
    let mut out = HashMap::new();
    let Some(keys) = doc.get("keys").and_then(Value::as_array) else {
        return out;
    };
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for jwk in keys {
        if let Some(kid) = jwk.get("kid").and_then(Value::as_str) {
            *seen.entry(kid).or_default() += 1;
        }
    }
    let duplicates = seen.values().filter(|n| **n > 1).count();
    if duplicates > 0 {
        tracing::warn!(
            duplicate_kids = duplicates,
            "JWKS has keys sharing a kid; those kids are ignored"
        );
    }
    for jwk in keys {
        let s = |k: &str| jwk.get(k).and_then(Value::as_str);
        let Some(kid) = s("kid").filter(|k| !k.is_empty()) else {
            continue;
        };
        if seen.get(kid).copied().unwrap_or(0) > 1 {
            continue;
        }
        if s("use").is_some_and(|u| u != "sig") {
            continue;
        }
        let parsed = match (alg, s("kty"), s("alg")) {
            (Algorithm::ES256, Some("EC"), None | Some("ES256")) if s("crv") == Some("P-256") => {
                match (s("x"), s("y")) {
                    (Some(x), Some(y)) => validated_ec_key(x, y),
                    _ => None,
                }
            }
            (Algorithm::RS256, Some("RSA"), None | Some("RS256")) => match (s("n"), s("e")) {
                (Some(n), Some(e)) if rsa_bits(n) >= 2048 => {
                    DecodingKey::from_rsa_components(n, e).ok()
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(key) = parsed {
            if out.len() >= MAX_KEYS {
                break;
            }
            out.insert(kid.to_string(), JwkKey { alg, key });
        }
    }
    out
}

/// Validate the uncompressed point as SPKI before keeping an EC JWK.
fn validated_ec_key(x: &str, y: &str) -> Option<DecodingKey> {
    use base64::{
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
        Engine,
    };

    let x_bytes = URL_SAFE_NO_PAD.decode(x).ok()?;
    let y_bytes = URL_SAFE_NO_PAD.decode(y).ok()?;
    if x_bytes.len() != 32 || y_bytes.len() != 32 {
        return None;
    }
    // SubjectPublicKeyInfo: id-ecPublicKey, prime256v1, uncompressed point.
    let mut der = vec![
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04,
    ];
    der.extend_from_slice(&x_bytes);
    der.extend_from_slice(&y_bytes);
    let pem = format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        STANDARD.encode(der)
    );
    super::eckey::p256_public(&pem).ok()?;
    DecodingKey::from_ec_components(x, y).ok()
}

/// Modulus size in bits: `n` decoded (base64url), leading zero bytes and
/// leading zero bits not counted.
fn rsa_bits(n: &str) -> usize {
    let Some(bytes) = b64url_decode(n.trim_end_matches('=')) else {
        return 0;
    };
    let Some(first) = bytes.iter().position(|b| *b != 0) else {
        return 0;
    };
    (bytes.len() - first) * 8 - bytes[first].leading_zeros() as usize
}

fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // P-256 public key coordinates (RFC 7515 A.3 example).
    const X: &str = "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU";
    const Y: &str = "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0";

    struct Fake {
        calls: AtomicUsize,
        doc: std::sync::Mutex<Result<Value, String>>,
    }

    #[async_trait]
    impl JwksSource for Fake {
        async fn fetch(&self) -> Result<Value, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.doc.lock().unwrap().clone()
        }
    }

    fn doc(kids: &[&str]) -> Value {
        json!({"keys": kids.iter().map(|k| json!({"kty": "EC", "crv": "P-256", "kid": k, "x": X, "y": Y})).collect::<Vec<_>>()})
    }

    fn fake(kids: &[&str]) -> Arc<Fake> {
        Arc::new(Fake {
            calls: AtomicUsize::new(0),
            doc: std::sync::Mutex::new(Ok(doc(kids))),
        })
    }

    #[tokio::test]
    async fn caches_and_refetches_by_the_rules() {
        let src = fake(&["a"]);
        let cache = JwksCache::new(src.clone());
        let t0 = Instant::now();
        assert!(cache.key_at("a", t0).await.is_some());
        assert!(cache
            .key_at("a", t0 + Duration::from_secs(300))
            .await
            .is_some());
        assert_eq!(src.calls.load(Ordering::SeqCst), 1, "fresh cache is reused");
        // Unknown kid: refetch, but not again within 60 s.
        assert!(cache
            .key_at("b", t0 + Duration::from_secs(301))
            .await
            .is_none());
        assert!(cache
            .key_at("b", t0 + Duration::from_secs(320))
            .await
            .is_none());
        assert_eq!(src.calls.load(Ordering::SeqCst), 2);
        *src.doc.lock().unwrap() = Ok(doc(&["a", "b"]));
        assert!(cache
            .key_at("b", t0 + Duration::from_secs(362))
            .await
            .is_some());
        assert_eq!(src.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stale_cache_is_used_for_at_most_an_hour() {
        let src = fake(&["a"]);
        let cache = JwksCache::new(src.clone());
        let t0 = Instant::now();
        assert!(cache.key_at("a", t0).await.is_some());
        *src.doc.lock().unwrap() = Err("down".into());
        assert!(cache
            .key_at("a", t0 + Duration::from_secs(1800))
            .await
            .is_some());
        assert!(cache
            .key_at("a", t0 + Duration::from_secs(3601))
            .await
            .is_none());
    }

    #[test]
    fn jwks_ec_keys_are_validated() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

        let mut y = URL_SAFE_NO_PAD.decode(Y).unwrap();
        y[31] ^= 1;
        let keys = parse_jwks(
            &json!({"keys": [
                ec("valid"),
                {"kty": "EC", "crv": "P-256", "kid": "off-curve", "x": X, "y": URL_SAFE_NO_PAD.encode(y)},
                {"kty": "EC", "crv": "P-256", "kid": "short", "x": "AA", "y": Y},
                {"kty": "EC", "crv": "P-256", "kid": "malformed", "x": "!", "y": Y}
            ]}),
            Algorithm::ES256,
        );
        assert_eq!(keys.keys().collect::<Vec<_>>(), vec!["valid"]);
    }

    #[test]
    fn only_acceptable_keys_are_kept() {
        let keys = parse_jwks(
            &json!({"keys": [
                {"kty": "EC", "crv": "P-256", "kid": "ok", "x": X, "y": Y},
                {"kty": "EC", "crv": "P-256", "kid": "wrong-alg", "alg": "RS256", "x": X, "y": Y},
                {"kty": "EC", "crv": "P-256", "x": X, "y": Y},
                {"kty": "EC", "crv": "P-384", "kid": "p384", "x": X, "y": Y},
                {"kty": "EC", "crv": "P-256", "kid": "enc", "use": "enc", "x": X, "y": Y},
                {"kty": "RSA", "kid": "small", "n": "AQAB", "e": "AQAB"},
                {"kty": "oct", "kid": "hmac", "k": "c2VjcmV0"}
            ]}),
            Algorithm::ES256,
        );
        assert_eq!(keys.keys().collect::<Vec<_>>(), vec!["ok"]);
    }

    /// 2048-bit RSA modulus (src/runtime/testdata/rsa-p2048.pem).
    const RSA_N: &str = "tupUT5sUR0iufz_2dX_ZWfdJ9oluerIsYtUELeYxbe_Bu71_oJUILJiNUyceUtmDJRNHj6NWalJ36zkykGm3ZU65cAI91A9d7mSN0ryaDn6-b45VINQIEtMSutWrGh2I7FgxLNGmDFMLQQ1sDVCS1lwtAD_EtYzzWrqjxdqRal1eaGF58dtMjqCk_rrGcQ7oEm_tTBZVge24sf7bQUliuwZQo6t7SI2tvocawcgDuZvFMU_I99dyGJNQxyY1pKkPqIdMxtrTrBoHcBXNvlSu8IoFslK49w3XY8kpkb7KGoJvDmT6NYQxXtjNY0_-ZZIdM_egJboeibuVVzi549nPjQ";

    fn ec(kid: &str) -> Value {
        json!({"kty": "EC", "crv": "P-256", "kid": kid, "x": X, "y": Y})
    }

    fn rsa(kid: &str) -> Value {
        json!({"kty": "RSA", "kid": kid, "n": RSA_N, "e": "AQAB"})
    }

    #[test]
    fn keys_are_filtered_by_the_configured_alg_at_parse_time() {
        let doc = json!({"keys": [ec("e1"), rsa("r1")]});
        let es = parse_jwks(&doc, Algorithm::ES256);
        assert_eq!(es.keys().collect::<Vec<_>>(), vec!["e1"]);
        assert_eq!(es["e1"].alg, Algorithm::ES256);
        let rs = parse_jwks(&doc, Algorithm::RS256);
        assert_eq!(rs.keys().collect::<Vec<_>>(), vec!["r1"]);
        assert_eq!(rs["r1"].alg, Algorithm::RS256);
    }

    #[test]
    fn duplicate_kids_are_ignored_in_any_order() {
        for alg in [Algorithm::ES256, Algorithm::RS256] {
            for keys in [
                vec![ec("k1"), rsa("k1"), ec("ok"), rsa("rok")],
                vec![rsa("k1"), ec("k1"), rsa("rok"), ec("ok")],
                vec![ec("k1"), ec("k1"), ec("ok"), rsa("rok")],
            ] {
                let parsed = parse_jwks(&json!({ "keys": keys }), alg);
                assert!(!parsed.contains_key("k1"), "{alg:?}: duplicate kid used");
                let want = if alg == Algorithm::ES256 { "ok" } else { "rok" };
                assert!(parsed.contains_key(want), "{alg:?}");
            }
        }
    }

    #[test]
    fn parsing_does_not_depend_on_key_order() {
        let keys = vec![ec("a"), rsa("b"), ec("c"), rsa("c"), ec("d"), rsa("e")];
        for alg in [Algorithm::ES256, Algorithm::RS256] {
            let forward = parse_jwks(&json!({ "keys": keys }), alg);
            let mut reversed = keys.clone();
            reversed.reverse();
            let backward = parse_jwks(&json!({ "keys": reversed }), alg);
            let mut f: Vec<_> = forward.keys().collect();
            let mut b: Vec<_> = backward.keys().collect();
            f.sort();
            b.sort();
            assert_eq!(f, b, "{alg:?}");
        }
    }

    #[test]
    fn rsa_size_is_measured_from_the_decoded_modulus() {
        let enc = |bytes: &[u8]| {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let n =
                    chunk.iter().fold(0u32, |a, b| (a << 8) | *b as u32) << (8 * (3 - chunk.len()));
                for i in 0..=chunk.len() {
                    out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
                }
            }
            out
        };
        let mut k2048 = vec![0x80u8];
        k2048.extend(vec![1u8; 255]);
        assert_eq!(rsa_bits(&enc(&k2048)), 2048);
        let mut k2047 = vec![0x40u8];
        k2047.extend(vec![1u8; 255]);
        assert_eq!(rsa_bits(&enc(&k2047)), 2047);
        let mut padded = vec![0u8];
        padded.extend(&k2047);
        assert_eq!(
            rsa_bits(&enc(&padded)),
            2047,
            "leading zero bytes do not count"
        );
    }

    #[tokio::test]
    async fn fetch_does_not_hold_the_cache_lock() {
        struct Slow(std::sync::Arc<tokio::sync::Notify>);
        #[async_trait]
        impl JwksSource for Slow {
            async fn fetch(&self) -> Result<Value, String> {
                self.0.notified().await;
                Ok(doc(&["a"]))
            }
        }
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let cache = std::sync::Arc::new(JwksCache::new(std::sync::Arc::new(Slow(gate.clone()))));
        let t0 = Instant::now();
        let first = {
            let cache = cache.clone();
            tokio::spawn(async move { cache.key_at("a", t0).await.is_some() })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        // While the first fetch is blocked, another caller returns at once.
        let other = tokio::time::timeout(Duration::from_millis(200), cache.key_at("a", t0)).await;
        assert!(other.is_ok(), "second caller must not wait for the fetch");
        gate.notify_one();
        assert!(first.await.unwrap());
    }

    #[test]
    fn https_is_required() {
        assert!(HttpsJwks::new("http://idp.example.com/jwks.json").is_err());
        assert!(HttpsJwks::new("https://idp.example.com/jwks.json").is_ok());
        for bad in [
            "https://127.0.0.1/jwks.json",
            "https://localhost/jwks.json",
            "https://169.254.169.254/latest/meta-data",
            "https://10.0.0.5/k",
            "https://192.168.1.1/k",
            "https://172.16.0.1/k",
            "https://100.64.0.1/k",
            "https://0.0.0.0/k",
            "https://[::1]/k",
            "https://[::ffff:127.0.0.1]/k",
            "https://[fe80::1]/k",
            "https://idp.internal/k",
            "https://intranet/k",
        ] {
            assert!(HttpsJwks::new(bad).is_err(), "{bad}");
        }
        assert!(HttpsJwks::with_policy("https://localhost/jwks.json", true).is_ok());
    }
}

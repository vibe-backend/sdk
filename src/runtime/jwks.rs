//! JWKS key source with the spec §4.1 (decision 44) cache rules:
//! keys are cached for 10 minutes; an unknown `kid` triggers an immediate
//! refetch, at most once every 60 seconds; when fetching fails, the old cache
//! is used for at most 1 hour after the last successful fetch, after which
//! every token is rejected. Only ES256 (P-256) and RS256 (>= 2048-bit) keys
//! are accepted, and every key needs a `kid`.

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
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| err.to_string())?;
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
    state: Mutex<State>,
}

impl JwksCache {
    pub fn new(source: Arc<dyn JwksSource>) -> Self {
        Self {
            source,
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
                    state.keys = parse_jwks(&doc);
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

/// Parses a JWKS document, keeping only acceptable signing keys.
pub fn parse_jwks(doc: &Value) -> HashMap<String, JwkKey> {
    let mut out = HashMap::new();
    let Some(keys) = doc.get("keys").and_then(Value::as_array) else {
        return out;
    };
    for jwk in keys {
        let s = |k: &str| jwk.get(k).and_then(Value::as_str);
        let Some(kid) = s("kid").filter(|k| !k.is_empty()) else {
            continue;
        };
        if s("use").is_some_and(|u| u != "sig") {
            continue;
        }
        let parsed = match (s("kty"), s("alg")) {
            (Some("EC"), None | Some("ES256")) if s("crv") == Some("P-256") => {
                match (s("x"), s("y")) {
                    (Some(x), Some(y)) => {
                        DecodingKey::from_ec_components(x, y)
                            .ok()
                            .map(|key| JwkKey {
                                alg: Algorithm::ES256,
                                key,
                            })
                    }
                    _ => None,
                }
            }
            (Some("RSA"), None | Some("RS256")) => match (s("n"), s("e")) {
                (Some(n), Some(e)) if rsa_bits(n) >= 2048 => DecodingKey::from_rsa_components(n, e)
                    .ok()
                    .map(|key| JwkKey {
                        alg: Algorithm::RS256,
                        key,
                    }),
                _ => None,
            },
            _ => None,
        };
        if let Some(key) = parsed {
            if out.len() >= MAX_KEYS {
                break;
            }
            out.insert(kid.to_string(), key);
        }
    }
    out
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
    fn only_acceptable_keys_are_kept() {
        let keys = parse_jwks(&json!({"keys": [
            {"kty": "EC", "crv": "P-256", "kid": "ok", "x": X, "y": Y},
            {"kty": "EC", "crv": "P-256", "x": X, "y": Y},
            {"kty": "EC", "crv": "P-384", "kid": "p384", "x": X, "y": Y},
            {"kty": "EC", "crv": "P-256", "kid": "enc", "use": "enc", "x": X, "y": Y},
            {"kty": "RSA", "kid": "small", "n": "AQAB", "e": "AQAB"},
            {"kty": "oct", "kid": "hmac", "k": "c2VjcmV0"}
        ]}));
        assert_eq!(keys.keys().collect::<Vec<_>>(), vec!["ok"]);
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

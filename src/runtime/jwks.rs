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

impl HttpsJwks {
    pub fn new(uri: &str) -> Result<Self, String> {
        let parsed = url::Url::parse(uri).map_err(|err| format!("jwks-uri: {err}"))?;
        if parsed.scheme() != "https" {
            return Err("jwks-uri must use https".into());
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
        resp.json().await.map_err(|err| err.to_string())
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
            }),
        }
    }

    pub async fn key(&self, kid: &str) -> Option<JwkKey> {
        self.key_at(kid, Instant::now()).await
    }

    /// `key` with an explicit clock, for tests.
    pub async fn key_at(&self, kid: &str, now: Instant) -> Option<JwkKey> {
        let mut state = self.state.lock().await;
        let fresh = state
            .fetched_at
            .is_some_and(|at| now.duration_since(at) < FRESH_FOR);
        let known = state.keys.contains_key(kid);
        let may_refetch = state
            .last_attempt
            .is_none_or(|at| now.duration_since(at) >= REFETCH_GAP);
        if (!fresh || !known) && may_refetch {
            state.last_attempt = Some(now);
            match self.source.fetch().await {
                Ok(doc) => {
                    state.keys = parse_jwks(&doc);
                    state.fetched_at = Some(now);
                }
                Err(err) => tracing::warn!(%err, "JWKS fetch failed"),
            }
        }
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
            out.insert(kid.to_string(), key);
        }
    }
    out
}

/// Modulus size in bits from a base64url `n`.
fn rsa_bits(n: &str) -> usize {
    let bytes = n.trim_end_matches('=').len() * 6 / 8;
    bytes * 8
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
    fn https_is_required() {
        assert!(HttpsJwks::new("http://idp.example.com/jwks.json").is_err());
        assert!(HttpsJwks::new("https://idp.example.com/jwks.json").is_ok());
    }
}

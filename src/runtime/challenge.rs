//! Server-side verification of challenge proofs (`x-challenge-proof`, #118).
//!
//! A route with `challenge: { type: captcha, when: always }` passes only when
//! a configured [`ChallengeVerifier`] accepts the proof. v1 ships Cloudflare
//! Turnstile ([`TurnstileVerifier`]); other providers (hCaptcha) plug in
//! behind the same trait later. Everything fails closed: no verifier, an
//! unreachable provider, a bad answer, a wrong hostname or action, or a token
//! seen before are all rejections.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::http::RunEnv;

/// Cloudflare's siteverify endpoint.
pub const TURNSTILE_SITEVERIFY_URL: &str =
    "https://challenges.cloudflare.com/turnstile/v0/siteverify";
/// Turnstile tokens are valid for 300 seconds.
pub const TOKEN_LIFETIME: Duration = Duration::from_secs(300);
/// Turnstile tokens are at most 2048 characters.
pub const MAX_TOKEN_LEN: usize = 2048;
const DEFAULT_REPLAY_CAPACITY: usize = 100_000;

/// What the proof must be bound to.
#[derive(Clone, Debug)]
pub struct ChallengeContext {
    /// Expected widget action (see [`challenge_action`]).
    pub action: String,
    /// Client IP, forwarded to the provider as a signal.
    pub remote_ip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChallengeError {
    /// The proof is invalid, expired, replayed or bound to something else.
    Rejected(String),
    /// The verifier could not reach a decision (provider down, timeout, bad
    /// response). The request fails closed.
    Unavailable(String),
}

#[async_trait]
pub trait ChallengeVerifier: Send + Sync {
    /// Provider name (`turnstile`, later `hcaptcha`).
    fn provider(&self) -> &'static str;
    /// Accepts `token` only if the provider verified it for `ctx` and it was
    /// not used before.
    async fn verify(&self, token: &str, ctx: &ChallengeContext) -> Result<(), ChallengeError>;
}

/// Widget action expected for a route: the endpoint name (last segment of
/// the feature id), limited to Turnstile's 32 characters of `[A-Za-z0-9_-]`.
pub fn challenge_action(feature_id: &str) -> String {
    let name = feature_id.rsplit('/').next().unwrap_or(feature_id);
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect()
}

/// Used tokens (SHA-256 of the token), kept for the token lifetime. A token is
/// reserved before the provider is asked, so concurrent reuse is refused too.
pub struct ReplayCache {
    ttl: Duration,
    capacity: usize,
    seen: Mutex<HashMap<[u8; 32], Instant>>,
}

impl ReplayCache {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Marks `token` as used. Errors if it was used before, or if the cache
    /// is full of live tokens (fail closed).
    pub fn claim(&self, token: &str) -> Result<(), ChallengeError> {
        let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = seen.get(&key) {
            if now.duration_since(*at) < self.ttl {
                return Err(ChallengeError::Rejected(
                    "challenge token already used".into(),
                ));
            }
        }
        if seen.len() >= self.capacity {
            let ttl = self.ttl;
            seen.retain(|_, at| now.duration_since(*at) < ttl);
            if seen.len() >= self.capacity {
                return Err(ChallengeError::Unavailable(
                    "challenge replay cache is full".into(),
                ));
            }
        }
        seen.insert(key, now);
        Ok(())
    }
}

impl Default for ReplayCache {
    fn default() -> Self {
        // A little over the token lifetime, so a token cannot outlive its entry.
        Self::new(
            TOKEN_LIFETIME + Duration::from_secs(60),
            DEFAULT_REPLAY_CAPACITY,
        )
    }
}

pub struct TurnstileConfig {
    pub secret: String,
    /// Hostnames the widget may be served from; the siteverify `hostname`
    /// must be one of them.
    pub hostnames: Vec<String>,
    pub siteverify_url: String,
}

pub struct TurnstileVerifier {
    config: TurnstileConfig,
    client: reqwest::Client,
    replay: ReplayCache,
}

#[derive(Deserialize)]
struct SiteverifyResponse {
    success: bool,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    challenge_ts: Option<String>,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
}

impl TurnstileVerifier {
    /// A verifier over `client`. Production uses [`TurnstileVerifier::guarded`];
    /// tests pass a plain client pointed at a mock siteverify.
    pub fn new(config: TurnstileConfig, client: reqwest::Client) -> Self {
        Self {
            config,
            client,
            replay: ReplayCache::default(),
        }
    }

    pub fn with_replay_cache(mut self, replay: ReplayCache) -> Self {
        self.replay = replay;
        self
    }

    /// The production client: 5 s timeout, no redirects, outbound guard
    /// (public addresses only, no proxy).
    pub fn guarded(config: TurnstileConfig) -> Result<Self, String> {
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none());
        let client = crate::netguard::guard_client(builder)
            .build()
            .map_err(|err| format!("challenge http client: {err}"))?;
        Ok(Self::new(config, client))
    }
}

#[async_trait]
impl ChallengeVerifier for TurnstileVerifier {
    fn provider(&self) -> &'static str {
        "turnstile"
    }

    async fn verify(&self, token: &str, ctx: &ChallengeContext) -> Result<(), ChallengeError> {
        let token = token.trim();
        if token.is_empty() || token.len() > MAX_TOKEN_LEN {
            return Err(ChallengeError::Rejected("malformed challenge token".into()));
        }
        // Single use: reserve before asking the provider; the reservation
        // stays even if verification fails.
        self.replay.claim(token)?;
        let mut form = vec![("secret", self.config.secret.as_str()), ("response", token)];
        if let Some(ip) = ctx.remote_ip.as_deref() {
            form.push(("remoteip", ip));
        }
        let response = self
            .client
            .post(&self.config.siteverify_url)
            .form(&form)
            .send()
            .await
            .map_err(|err| ChallengeError::Unavailable(format!("siteverify: {err}")))?;
        if !response.status().is_success() {
            return Err(ChallengeError::Unavailable(format!(
                "siteverify answered {}",
                response.status()
            )));
        }
        let body: SiteverifyResponse = response
            .json()
            .await
            .map_err(|err| ChallengeError::Unavailable(format!("siteverify body: {err}")))?;
        if !body.success {
            return Err(ChallengeError::Rejected(format!(
                "turnstile rejected the token: {}",
                body.error_codes.join(",")
            )));
        }
        let hostname = body.hostname.unwrap_or_default().to_ascii_lowercase();
        if !self
            .config
            .hostnames
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&hostname))
        {
            return Err(ChallengeError::Rejected(format!(
                "challenge solved on unexpected hostname {hostname:?}"
            )));
        }
        if body.action.as_deref() != Some(ctx.action.as_str()) {
            return Err(ChallengeError::Rejected(format!(
                "challenge action {:?} does not match {:?}",
                body.action, ctx.action
            )));
        }
        if let Some(ts) = body.challenge_ts.as_deref() {
            let issued = chrono::DateTime::parse_from_rfc3339(ts)
                .map_err(|_| ChallengeError::Rejected("challenge_ts is not a timestamp".into()))?;
            let age = chrono::Utc::now().signed_duration_since(issued);
            if age > chrono::Duration::from_std(TOKEN_LIFETIME).unwrap_or_default() {
                return Err(ChallengeError::Rejected("challenge token expired".into()));
            }
        }
        Ok(())
    }
}

/// Builds the verifier from the environment:
///
/// - `VIBE_CHALLENGE_PROVIDER`: `turnstile` (default). `hcaptcha` is reserved.
/// - `VIBE_TURNSTILE_SECRET`: the Turnstile secret key.
/// - `VIBE_CHALLENGE_HOSTNAMES`: comma-separated hostnames the widget runs on.
///
/// `Ok(None)` means not configured (no secret). A partial or unknown
/// configuration is an error.
pub fn verifier_from_env() -> Result<Option<Arc<dyn ChallengeVerifier>>, String> {
    verifier_from_vars(|name| std::env::var(name).ok())
}

pub fn verifier_from_vars(
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<Arc<dyn ChallengeVerifier>>, String> {
    let provider = get("VIBE_CHALLENGE_PROVIDER")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "turnstile".into());
    let config = match turnstile_config(&provider, &get)? {
        Some(config) => config,
        None => return Ok(None),
    };
    Ok(Some(Arc::new(TurnstileVerifier::guarded(config)?)))
}

fn turnstile_config(
    provider: &str,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<TurnstileConfig>, String> {
    match provider {
        "turnstile" => {}
        "hcaptcha" => {
            return Err(
                "VIBE_CHALLENGE_PROVIDER=hcaptcha is not supported yet; use turnstile".into(),
            )
        }
        other => return Err(format!("unknown VIBE_CHALLENGE_PROVIDER {other:?}")),
    }
    let Some(secret) = get("VIBE_TURNSTILE_SECRET").filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    let hostnames: Vec<String> = get("VIBE_CHALLENGE_HOSTNAMES")
        .unwrap_or_default()
        .split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if hostnames.is_empty() {
        return Err("VIBE_TURNSTILE_SECRET is set but VIBE_CHALLENGE_HOSTNAMES is empty".into());
    }
    Ok(Some(TurnstileConfig {
        secret: secret.trim().to_string(),
        hostnames,
        siteverify_url: TURNSTILE_SITEVERIFY_URL.into(),
    }))
}

/// Whether the environment configures a challenge verifier, without building
/// a client (for `vibe validate`).
pub fn configured_in_env() -> Result<bool, String> {
    let get = |name: &str| std::env::var(name).ok();
    let provider = get("VIBE_CHALLENGE_PROVIDER")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "turnstile".into());
    Ok(turnstile_config(&provider, &get)?.is_some())
}

/// Error when production would serve `challenge_when: always` routes without
/// a verifier. `routes` are the offending lock or route ids.
pub fn production_gate(env: RunEnv, configured: bool, routes: &[String]) -> Result<(), String> {
    if env != RunEnv::Production || configured || routes.is_empty() {
        return Ok(());
    }
    Err(format!(
        "challenge `when: always` needs a configured verifier in production (set VIBE_TURNSTILE_SECRET and VIBE_CHALLENGE_HOSTNAMES); affected: {}",
        routes.join(", ")
    ))
}

/// Lock files (project-relative) whose `## Access` declares a challenge with
/// `when: always`.
pub fn always_challenge_locks(root: &Path) -> Vec<String> {
    let Ok(sources) = crate::discover::discover(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for source in sources {
        let rel = source.lock_rel();
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        let Ok(doc) = crate::lock::parse_lock(&text) else {
            continue;
        };
        let raw = doc
            .sections
            .iter()
            .find(|s| s.name == "Access")
            .and_then(|s| s.get("challenge"))
            .unwrap_or("");
        let (ty, when) = crate::manifest::parse_challenge(raw);
        if ty != "none" && when == "always" {
            out.push(rel);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn action_is_the_endpoint_name() {
        assert_eq!(challenge_action("shop/v1/register"), "register");
        assert_eq!(challenge_action("a/v1/x.y z"), "x-y-z");
        assert_eq!(challenge_action(&"a".repeat(40)).len(), 32);
    }

    #[test]
    fn replay_cache_refuses_reuse_and_fails_closed_when_full() {
        let cache = ReplayCache::new(Duration::from_secs(60), 2);
        assert!(cache.claim("a").is_ok());
        assert!(matches!(cache.claim("a"), Err(ChallengeError::Rejected(_))));
        assert!(cache.claim("b").is_ok());
        assert!(matches!(
            cache.claim("c"),
            Err(ChallengeError::Unavailable(_))
        ));
        let short = ReplayCache::new(Duration::from_millis(0), 1);
        assert!(short.claim("a").is_ok());
        assert!(short.claim("b").is_ok(), "expired entries are pruned");
    }

    #[test]
    fn env_configuration() {
        assert!(verifier_from_vars(vars(&[])).unwrap().is_none());
        assert!(verifier_from_vars(vars(&[("VIBE_TURNSTILE_SECRET", "s")])).is_err());
        assert!(verifier_from_vars(vars(&[("VIBE_CHALLENGE_PROVIDER", "hcaptcha")])).is_err());
        assert!(verifier_from_vars(vars(&[("VIBE_CHALLENGE_PROVIDER", "pow")])).is_err());
        let v = verifier_from_vars(vars(&[
            ("VIBE_TURNSTILE_SECRET", "s"),
            ("VIBE_CHALLENGE_HOSTNAMES", "example.com, www.example.com"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(v.provider(), "turnstile");
    }

    #[test]
    fn production_refuses_always_without_a_verifier() {
        let routes = vec!["api/x.lock.md".to_string()];
        assert!(production_gate(RunEnv::Production, false, &routes).is_err());
        assert!(production_gate(RunEnv::Production, true, &routes).is_ok());
        assert!(production_gate(RunEnv::Production, false, &[]).is_ok());
        assert!(production_gate(RunEnv::Dev, false, &routes).is_ok());
        assert!(production_gate(RunEnv::Ci, false, &routes).is_ok());
    }
}

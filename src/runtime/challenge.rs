//! Server-side verification of challenge proofs (`x-challenge-proof`, #118).
//!
//! A route with `challenge: { type: captcha, when: always }` passes only when
//! a configured [`ChallengeVerifier`] accepts the proof. v1 ships Cloudflare
//! Turnstile ([`TurnstileVerifier`]); other providers (hCaptcha) plug in
//! behind the same trait later. Everything fails closed: no verifier, an
//! unreachable provider, a bad answer, a wrong hostname or action, or a token
//! seen before are all rejections.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;

use super::http::RunEnv;
pub use super::replay::{token_key, Outcome, PgReplayStore, ReplaySlot, ReplayStore};

/// Cloudflare's siteverify endpoint.
pub const TURNSTILE_SITEVERIFY_URL: &str =
    "https://challenges.cloudflare.com/turnstile/v0/siteverify";
/// Turnstile tokens are valid for 300 seconds.
pub const TOKEN_LIFETIME: Duration = Duration::from_secs(300);
/// Turnstile tokens are at most 2048 characters.
pub const MAX_TOKEN_LEN: usize = 2048;
const DEFAULT_REPLAY_CAPACITY: usize = 100_000;
/// Unverified challenge slots per client IP.
pub const DEFAULT_PENDING_PER_IP: usize = 16;
/// Unverified challenge slots per tenant.
pub const DEFAULT_PENDING_PER_TENANT: usize = 1_000;
/// Upper bound on a pending reservation (siteverify times out after 5 s).
const DEFAULT_PENDING_HOLD: Duration = Duration::from_secs(30);
/// How long a token Turnstile rejected stays reserved.
pub const DEFAULT_REJECTED_HOLD: Duration = Duration::from_secs(10);
/// Allowed clock skew for a `challenge_ts` in the future.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(60);

/// What the proof must be bound to.
#[derive(Clone, Debug)]
pub struct ChallengeContext {
    /// Expected widget action (see [`challenge_action`]).
    pub action: String,
    /// Client IP, forwarded to the provider as a signal.
    pub remote_ip: Option<String>,
    /// Tenant of the route, for the per-tenant quota of pending tokens.
    pub tenant: Option<String>,
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

/// Turnstile allows at most 32 characters of `[A-Za-z0-9_-]` in an action.
const MAX_ACTION_LEN: usize = 32;

/// Widget action expected for a route, derived from the whole feature id so
/// that endpoints sharing a last segment (`shop/v1/register`,
/// `admin/v1/register`) get different actions (#159).
///
/// - If every character is in `[A-Za-z0-9_-]` once `/` is mapped to `_`, and
///   the result fits in 32 characters, the action is that string
///   (`shop/v1/register` → `shop_v1_register`).
/// - Otherwise (too long, or a character had to be replaced) it is the first
///   23 sanitized characters, `-`, and the first 8 hex digits of SHA-256 of
///   the feature id, so names that only differ after 23 characters or in a
///   replaced character still differ.
///
/// [`action_collisions`] refuses the rare remaining hash collision at startup.
pub fn challenge_action(feature_id: &str) -> String {
    let mut changed = false;
    let sanitized: String = feature_id
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => c,
            '/' => '_',
            _ => {
                changed = true;
                '-'
            }
        })
        .collect();
    if !changed && sanitized.len() <= MAX_ACTION_LEN {
        return sanitized;
    }
    let digest = Sha256::digest(feature_id.as_bytes());
    let suffix: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    let prefix: String = sanitized.chars().take(MAX_ACTION_LEN - 9).collect();
    format!("{prefix}-{suffix}")
}

/// Refuses a manifest where two challenge routes would expect the same
/// widget action (a token solved for one would be accepted by the other).
pub fn action_collisions<'a>(feature_ids: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut seen: std::collections::BTreeMap<String, &str> = Default::default();
    let mut clashes = Vec::new();
    for id in feature_ids {
        let action = challenge_action(id);
        match seen.get(&action) {
            Some(other) if *other != id => {
                clashes.push(format!("{other} and {id} (action {action})"))
            }
            _ => {
                seen.insert(action, id);
            }
        }
    }
    if clashes.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "challenge routes share a widget action; rename one of: {}",
            clashes.join("; ")
        ))
    }
}

/// Single-use bookkeeping for challenge tokens (SHA-256 of the token).
///
/// A token is reserved before the provider is asked, so concurrent reuse is
/// refused too. While the provider is asked the reservation is *pending* and
/// counts against a per-client-IP and a per-tenant quota. Only a token the
/// provider accepted is kept for the full `ttl`; a rejected token is kept for
/// `rejected_hold` (still counted against the quotas), and a reservation that
/// ends without an answer (provider unavailable, request dropped) is released.
pub struct ReplayCache {
    ttl: Duration,
    capacity: usize,
    per_ip: usize,
    per_tenant: usize,
    pending_hold: Duration,
    rejected_hold: Duration,
    state: Mutex<CacheState>,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<[u8; 32], Entry>,
    ip_counts: HashMap<String, usize>,
    tenant_counts: HashMap<String, usize>,
}

struct Entry {
    expires: Instant,
    /// Counted against the IP and tenant quotas (pending or rejected).
    counted: bool,
    ip: Option<String>,
    tenant: Option<String>,
}

fn dec(counts: &mut HashMap<String, usize>, key: &Option<String>) {
    if let Some(key) = key {
        if let Some(n) = counts.get_mut(key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.remove(key);
            }
        }
    }
}

impl CacheState {
    fn uncount(&mut self, entry: &mut Entry) {
        if entry.counted {
            entry.counted = false;
            dec(&mut self.ip_counts, &entry.ip);
            dec(&mut self.tenant_counts, &entry.tenant);
        }
    }

    fn remove(&mut self, key: &[u8; 32]) {
        if let Some(mut entry) = self.entries.remove(key) {
            self.uncount(&mut entry);
        }
    }

    fn prune(&mut self, now: Instant) {
        let expired: Vec<[u8; 32]> = self
            .entries
            .iter()
            .filter(|(_, e)| e.expires <= now)
            .map(|(k, _)| *k)
            .collect();
        for key in expired {
            self.remove(&key);
        }
    }
}

/// A pending reservation. Dropping it without [`Reservation::verified`] or
/// [`Reservation::rejected`] releases the slot.
pub struct Reservation<'a> {
    cache: &'a ReplayCache,
    key: [u8; 32],
    settled: bool,
}

impl Reservation<'_> {
    /// The provider accepted the token: keep it for the full TTL.
    pub fn verified(mut self) {
        self.settled = true;
        self.cache.settle_key(&self.key, Outcome::Verified);
    }

    /// The provider rejected the token: keep it only briefly.
    pub fn rejected(mut self) {
        self.settled = true;
        self.cache.settle_key(&self.key, Outcome::Rejected);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.cache.settle_key(&self.key, Outcome::Released);
        }
    }
}

impl ReplayCache {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            per_ip: DEFAULT_PENDING_PER_IP,
            per_tenant: DEFAULT_PENDING_PER_TENANT,
            pending_hold: DEFAULT_PENDING_HOLD,
            rejected_hold: DEFAULT_REJECTED_HOLD,
            state: Mutex::new(CacheState::default()),
        }
    }

    /// Unverified (pending or rejected) slots allowed per client IP and per tenant.
    pub fn with_quotas(mut self, per_ip: usize, per_tenant: usize) -> Self {
        self.per_ip = per_ip;
        self.per_tenant = per_tenant;
        self
    }

    /// How long a rejected token stays reserved (zero: released at once).
    pub fn with_rejected_hold(mut self, hold: Duration) -> Self {
        self.rejected_hold = hold;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of live entries (for tests and metrics).
    pub fn len(&self) -> usize {
        let mut st = self.lock();
        st.prune(Instant::now());
        st.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reserves `token` for a client. Errors if the token is reserved or was
    /// used, if the IP or tenant is over its quota of unverified slots
    /// (rejected), or if the cache is full of live tokens (unavailable).
    pub fn reserve(
        &self,
        token: &str,
        ip: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<Reservation<'_>, ChallengeError> {
        let key = token_key(token);
        self.reserve_key(&key, ip, tenant)?;
        Ok(Reservation {
            cache: self,
            key,
            settled: false,
        })
    }

    fn reserve_key(
        &self,
        key: &[u8; 32],
        ip: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<(), ChallengeError> {
        let key = *key;
        let now = Instant::now();
        let mut st = self.lock();
        match st.entries.get(&key) {
            Some(e) if e.expires > now => {
                return Err(ChallengeError::Rejected(
                    "challenge token already used".into(),
                ))
            }
            Some(_) => st.remove(&key),
            None => {}
        }
        let over = |counts: &HashMap<String, usize>, k: Option<&str>, limit: usize| {
            k.map(|k| counts.get(k).copied().unwrap_or(0) >= limit)
                .unwrap_or(false)
        };
        if over(&st.ip_counts, ip, self.per_ip) || over(&st.tenant_counts, tenant, self.per_tenant)
        {
            st.prune(now);
            if over(&st.ip_counts, ip, self.per_ip) {
                return Err(ChallengeError::Rejected(
                    "too many unverified challenge tokens from this client".into(),
                ));
            }
            if over(&st.tenant_counts, tenant, self.per_tenant) {
                return Err(ChallengeError::Rejected(
                    "too many unverified challenge tokens for this tenant".into(),
                ));
            }
        }
        if st.entries.len() >= self.capacity {
            st.prune(now);
            if st.entries.len() >= self.capacity {
                return Err(ChallengeError::Unavailable(
                    "challenge replay cache is full".into(),
                ));
            }
        }
        let ip = ip.map(str::to_string);
        let tenant = tenant.map(str::to_string);
        if let Some(ip) = &ip {
            *st.ip_counts.entry(ip.clone()).or_default() += 1;
        }
        if let Some(tenant) = &tenant {
            *st.tenant_counts.entry(tenant.clone()).or_default() += 1;
        }
        st.entries.insert(
            key,
            Entry {
                expires: now + self.pending_hold,
                counted: true,
                ip,
                tenant,
            },
        );
        Ok(())
    }

    fn settle_key(&self, key: &[u8; 32], outcome: Outcome) {
        let mut st = self.lock();
        match outcome {
            Outcome::Verified => {
                if let Some(mut entry) = st.entries.remove(key) {
                    st.uncount(&mut entry);
                    entry.expires = Instant::now() + self.ttl;
                    st.entries.insert(*key, entry);
                }
            }
            Outcome::Rejected if !self.rejected_hold.is_zero() => {
                if let Some(entry) = st.entries.get_mut(key) {
                    entry.expires = Instant::now() + self.rejected_hold;
                }
            }
            Outcome::Rejected | Outcome::Released => st.remove(key),
        }
    }
}

#[async_trait]
impl super::replay::ReplayStore for ReplayCache {
    fn name(&self) -> &'static str {
        "memory"
    }

    async fn reserve(
        &self,
        key: &[u8; 32],
        ip: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<super::replay::ReplaySlot, ChallengeError> {
        self.reserve_key(key, ip, tenant)?;
        Ok(super::replay::ReplaySlot {
            key: *key,
            tenant: tenant.map(str::to_string),
            nonce: [0; 16],
        })
    }

    async fn settle(
        &self,
        slot: &super::replay::ReplaySlot,
        outcome: Outcome,
    ) -> Result<(), ChallengeError> {
        self.settle_key(&slot.key, outcome);
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
    replay: Arc<dyn ReplayStore>,
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
            replay: Arc::new(ReplayCache::default()),
        }
    }

    pub fn with_replay_cache(self, replay: ReplayCache) -> Self {
        self.with_replay_store(Arc::new(replay))
    }

    /// Uses `store` for single-use bookkeeping (e.g. [`PgReplayStore`] when
    /// several instances serve the same routes).
    pub fn with_replay_store(mut self, store: Arc<dyn ReplayStore>) -> Self {
        self.replay = store;
        self
    }

    pub fn replay_store(&self) -> &'static str {
        self.replay.name()
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
        // Single use: reserve before asking the provider, so concurrent reuse
        // is refused. Only a verified token keeps the slot for the full TTL.
        let key = token_key(token);
        let slot = self
            .replay
            .reserve(&key, ctx.remote_ip.as_deref(), ctx.tenant.as_deref())
            .await?;
        let mut guard = super::replay::PendingGuard::new(self.replay.clone(), slot.clone());
        match self.ask(token, ctx).await {
            Ok(()) => {
                guard.disarm();
                // Fail closed: a token that cannot be recorded is not accepted.
                self.replay.settle(&slot, Outcome::Verified).await?;
                Ok(())
            }
            Err(err @ ChallengeError::Rejected(_)) => {
                guard.disarm();
                // Best effort: the pending entry expires on its own.
                let _ = self.replay.settle(&slot, Outcome::Rejected).await;
                Err(err)
            }
            Err(err) => {
                guard.disarm();
                let _ = self.replay.settle(&slot, Outcome::Released).await;
                Err(err)
            }
        }
    }
}

impl TurnstileVerifier {
    async fn ask(&self, token: &str, ctx: &ChallengeContext) -> Result<(), ChallengeError> {
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
        let ts = body
            .challenge_ts
            .as_deref()
            .ok_or_else(|| ChallengeError::Rejected("challenge_ts is missing".into()))?;
        let issued = chrono::DateTime::parse_from_rfc3339(ts)
            .map_err(|_| ChallengeError::Rejected("challenge_ts is not a timestamp".into()))?;
        let age = chrono::Utc::now().signed_duration_since(issued);
        if age > chrono::Duration::from_std(TOKEN_LIFETIME).unwrap_or_default() {
            return Err(ChallengeError::Rejected("challenge token expired".into()));
        }
        if -age > chrono::Duration::from_std(MAX_CLOCK_SKEW).unwrap_or_default() {
            return Err(ChallengeError::Rejected(
                "challenge_ts is in the future".into(),
            ));
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

/// [`verifier_from_env`] with a replay store (the in-memory cache when `None`).
pub fn verifier_from_env_with_store(
    store: Option<Arc<dyn ReplayStore>>,
) -> Result<Option<Arc<dyn ChallengeVerifier>>, String> {
    verifier_from_vars_with_store(|name| std::env::var(name).ok(), store)
}

pub fn verifier_from_vars(
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<Arc<dyn ChallengeVerifier>>, String> {
    verifier_from_vars_with_store(get, None)
}

pub fn verifier_from_vars_with_store(
    get: impl Fn(&str) -> Option<String>,
    store: Option<Arc<dyn ReplayStore>>,
) -> Result<Option<Arc<dyn ChallengeVerifier>>, String> {
    let provider = get("VIBE_CHALLENGE_PROVIDER")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "turnstile".into());
    let config = match turnstile_config(&provider, &get)? {
        Some(config) => config,
        None => return Ok(None),
    };
    let mut verifier = TurnstileVerifier::guarded(config)?;
    if let Some(store) = store {
        verifier = verifier.with_replay_store(store);
    }
    Ok(Some(Arc::new(verifier)))
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

/// Feature ids of every lock that declares a challenge (any `when`), for
/// the widget action collision check in `vibe validate --deploy`.
pub fn challenge_feature_ids(root: &Path) -> Vec<String> {
    let Ok(sources) = crate::discover::discover(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for source in sources {
        let Ok(text) = std::fs::read_to_string(root.join(source.lock_rel())) else {
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
        let (ty, _) = crate::manifest::parse_challenge(raw);
        if ty != "none" && !ty.is_empty() {
            out.push(doc.meta.id.clone());
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
    fn action_is_derived_from_the_feature_id() {
        assert_eq!(challenge_action("shop/v1/register"), "shop_v1_register");
        assert_ne!(
            challenge_action("shop/v1/register"),
            challenge_action("admin/v1/register")
        );
        let odd = challenge_action("a/v1/x.y z");
        assert!(odd.starts_with("a_v1_x-y-z-"), "{odd}");
        assert_ne!(odd, challenge_action("a/v1/x-y-z"));
        // Long names sharing their first 32 characters stay distinct.
        let base = "shop/v1/".to_string() + &"a".repeat(40);
        let (x, y) = (
            challenge_action(&(base.clone() + "x")),
            challenge_action(&(base + "y")),
        );
        assert_ne!(x, y);
        for action in [&x, &y, &odd] {
            assert!(action.len() <= 32, "{action}");
            assert!(action
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        }
        assert!(action_collisions(["shop/v1/register", "admin/v1/register"]).is_ok());
        // The same route listed twice is not a collision.
        assert!(action_collisions(["shop/v1/register", "shop/v1/register"]).is_ok());
        // `a/b` and `a_b` both map to `a_b`.
        let err = action_collisions(["x/a/b", "x/a_b"]).unwrap_err();
        assert!(err.contains("x/a/b and x/a_b"), "{err}");
    }

    #[test]
    fn replay_cache_refuses_reuse_and_fails_closed_when_full() {
        let cache = ReplayCache::new(Duration::from_secs(60), 2);
        cache.reserve("a", None, None).unwrap().verified();
        assert!(matches!(
            cache.reserve("a", None, None),
            Err(ChallengeError::Rejected(_))
        ));
        cache.reserve("b", None, None).unwrap().verified();
        assert!(matches!(
            cache.reserve("c", None, None),
            Err(ChallengeError::Unavailable(_))
        ));
        let short = ReplayCache::new(Duration::from_millis(0), 1);
        short.reserve("a", None, None).unwrap().verified();
        assert!(
            short.reserve("b", None, None).is_ok(),
            "expired entries are pruned"
        );
    }

    #[test]
    fn pending_reservation_blocks_concurrent_reuse_and_is_released_on_drop() {
        let cache = ReplayCache::new(Duration::from_secs(60), 10);
        let held = cache.reserve("a", Some("1.1.1.1"), None).unwrap();
        assert!(matches!(
            cache.reserve("a", Some("2.2.2.2"), None),
            Err(ChallengeError::Rejected(_))
        ));
        drop(held);
        assert!(cache.is_empty());
        assert!(cache.reserve("a", Some("2.2.2.2"), None).is_ok());
    }

    #[test]
    fn per_ip_quota_of_unverified_slots() {
        let cache = ReplayCache::new(Duration::from_secs(60), 100).with_quotas(2, 100);
        let ip = Some("1.1.1.1");
        let a = cache.reserve("a", ip, None).unwrap();
        cache.reserve("b", ip, None).unwrap().rejected();
        assert!(matches!(
            cache.reserve("c", ip, None),
            Err(ChallengeError::Rejected(_))
        ));
        // Other clients are unaffected.
        assert!(cache.reserve("c", Some("2.2.2.2"), None).is_ok());
        // A verified token no longer counts against the quota.
        a.verified();
        assert!(cache.reserve("d", ip, None).is_ok());
    }

    #[test]
    fn per_tenant_quota_of_unverified_slots() {
        let cache = ReplayCache::new(Duration::from_secs(60), 100).with_quotas(100, 1);
        let _a = cache.reserve("a", Some("1.1.1.1"), Some("t1")).unwrap();
        assert!(matches!(
            cache.reserve("b", Some("2.2.2.2"), Some("t1")),
            Err(ChallengeError::Rejected(_))
        ));
        assert!(cache.reserve("b", Some("2.2.2.2"), Some("t2")).is_ok());
    }

    #[test]
    fn rejected_tokens_do_not_hold_the_full_ttl() {
        let cache = ReplayCache::new(Duration::from_secs(3600), 1)
            .with_rejected_hold(Duration::from_millis(20));
        cache
            .reserve("a", Some("1.1.1.1"), None)
            .unwrap()
            .rejected();
        std::thread::sleep(Duration::from_millis(40));
        // The slot is free again long before the TTL.
        cache
            .reserve("b", Some("1.1.1.1"), None)
            .unwrap()
            .verified();
        assert_eq!(cache.len(), 1);
        let zero =
            ReplayCache::new(Duration::from_secs(3600), 1).with_rejected_hold(Duration::ZERO);
        zero.reserve("a", None, None).unwrap().rejected();
        assert!(zero.is_empty());
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

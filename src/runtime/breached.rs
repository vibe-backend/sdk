//! Breached-password check for every stored password (spec §4A.2, decision
//! 7). The password's SHA-1 is looked up k-anonymity style: only the first
//! five hex characters leave the platform; the plaintext and the full hash
//! never do. Sources:
//! - a range API (Pwned Passwords format, `SUFFIX:COUNT` lines), with no
//!   redirects, a public-host check, a body cap, strict parsing, a prefix
//!   cache and a concurrency cap; or
//! - an offline list of SHA-1 digests (sorted 20-byte digests, binary search).
//!
//! The check is mandatory. Range-API errors, timeouts and invalid responses
//! fail closed in every environment (503 SERVICE_UNAVAILABLE, Retry-After: 1).
//! An offline list can only be configured as the sole source.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha1::{Digest, Sha1};

/// Default range API (Pwned Passwords).
pub const DEFAULT_RANGE_URL: &str = "https://api.pwnedpasswords.com/range/";
/// Default lookup timeout (`VIBE_BREACHED_PASSWORDS_TIMEOUT_MS`): one total
/// deadline for a lookup, covering the wait for a lookup slot, connect and
/// the full response.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
/// Smallest accepted lookup timeout.
pub const MIN_TIMEOUT: Duration = Duration::from_millis(100);
/// Largest accepted lookup timeout.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(10);
/// Real padded responses are ~30–40 KB.
const MAX_BODY: usize = 256 * 1024;
/// With `Add-Padding`, a genuine response has hundreds of lines.
const MIN_LINES: usize = 100;
const CACHE_TTL: Duration = Duration::from_secs(3600);
const CACHE_ENTRIES: usize = 4096;
const MAX_LOOKUPS: usize = 8;
/// Largest accepted `VIBE_BREACHED_PASSWORDS_MIN_COUNT`. The recommended
/// value is 1 (any breach counts); a small margin is allowed for operators
/// who want to ignore one-off entries, anything above it is a startup error.
pub const MAX_MIN_COUNT: u64 = 10;

/// A few well-known breached passwords that pass the length floor; the
/// default for tests and `AuthConfig::anonymous`.
const BUILTIN: &[&str] = &[
    "password1234",
    "123456789012",
    "qwertyuiopas",
    "passwordpassword",
    "iloveyou1234",
    "correcthorsebatterystaple",
];

type Suffixes = HashMap<[u8; 18], u64>;

/// Range lookups that failed since start, across all sources in the process.
/// Read with [`breached_lookup_failures`]; see docs/release-hardening.md.
static LOOKUP_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of failed range lookups (outage, timeout, unexpected
/// response). Each failure is also logged at `WARN` with target
/// `vibe::breached_password_unavailable` for log-based alerting.
pub fn breached_lookup_failures() -> u64 {
    LOOKUP_FAILURES.load(Ordering::Relaxed)
}

/// Why a range lookup failed. The text is a fixed category (plus the HTTP
/// status or line number); upstream response content is never included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupError {
    /// The total lookup deadline passed (slot wait, connect or response).
    Timeout,
    /// Connect or transfer error.
    Network,
    /// A status other than 200.
    Status(u16),
    /// Content type is not `text/plain`.
    ContentType,
    /// Body larger than the cap.
    TooLarge,
    /// Not valid UTF-8.
    Encoding,
    /// A line that is not `SUFFIX:COUNT` (1-based line number).
    MalformedLine(usize),
    /// Fewer entries than a padded response has.
    TooFewEntries,
    /// Lookup slots closed (shutdown).
    Closed,
}

impl LookupError {
    /// Stable category for logs and alerts.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Status(_) => "status",
            Self::ContentType => "content_type",
            Self::TooLarge => "too_large",
            Self::Encoding | Self::MalformedLine(_) | Self::TooFewEntries => "malformed",
            Self::Closed => "closed",
        }
    }
}

impl std::fmt::Display for LookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("breached-password lookup: timed out"),
            Self::Network => f.write_str("breached-password lookup: network error"),
            Self::Status(code) => write!(f, "breached-password lookup: HTTP {code}"),
            Self::ContentType => {
                f.write_str("breached-password lookup: response is not text/plain")
            }
            Self::TooLarge => f.write_str("breached-password lookup: response too large"),
            Self::Encoding => f.write_str("breached-password response is not UTF-8"),
            Self::MalformedLine(n) => write!(f, "breached-password response: malformed line {n}"),
            Self::TooFewEntries => {
                f.write_str("breached-password response has too few entries for a padded response")
            }
            Self::Closed => f.write_str("breached-password lookups closed"),
        }
    }
}

impl From<LookupError> for String {
    fn from(err: LookupError) -> Self {
        err.to_string()
    }
}

pub struct RangeApi {
    url: String,
    host: String,
    client: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, std::sync::Arc<Suffixes>)>>,
    slots: tokio::sync::Semaphore,
    timeout: Duration,
    failures: AtomicU64,
}

/// Sorted 20-byte SHA-1 digests.
pub struct OfflineList(Vec<[u8; 20]>);

pub enum BreachedSource {
    Range(Box<RangeApi>),
    Offline(OfflineList),
}

/// Breached-password settings.
pub struct BreachedConfig {
    pub source: BreachedSource,
    /// A password counts as breached when seen at least this many times
    /// (`VIBE_BREACHED_PASSWORDS_MIN_COUNT`, default 1: any breach counts,
    /// as NIST SP 800-63B recommends).
    pub min_count: u64,
}

fn sha1(plain: &str) -> [u8; 20] {
    Sha1::digest(plain.as_bytes()).into()
}

fn parse_digest(text: &str) -> Option<[u8; 20]> {
    let mut out = [0u8; 20];
    hex::decode_to_slice(text, &mut out).ok()?;
    Some(out)
}

/// Whether a host is not on the public internet (loopback, private,
/// link-local incl. cloud metadata, CGNAT, ULA, unspecified, multicast,
/// documentation) or is a local-only name.
pub fn non_public_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || v4.is_multicast()
                    || v4.is_documentation()
                    || (o[0] == 100 && (64..128).contains(&o[1]))
                    || o[0] == 0
            }
            IpAddr::V6(v6) => {
                let seg = v6.segments();
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (seg[0] & 0xfe00) == 0xfc00
                    || (seg[0] & 0xffc0) == 0xfe80
                    || v6
                        .to_ipv4_mapped()
                        .is_some_and(|v4| non_public_host(&v4.to_string()))
            }
        };
    }
    let lower = bare.to_ascii_lowercase();
    lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
}

/// DNS resolver for the range API: every address a name resolves to must be
/// public (loopback allowed only for local test servers), so names such as
/// `169.254.169.254.nip.io` cannot reach internal hosts. The connection uses
/// only the checked addresses (no second lookup).
#[derive(Clone, Copy, Debug)]
pub struct PublicResolver {
    pub allow_loopback: bool,
}

impl PublicResolver {
    /// Resolves `host` and keeps it only if every address is allowed.
    pub async fn resolve_checked(&self, host: &str) -> Result<Vec<std::net::SocketAddr>, String> {
        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, 0))
            .await
            .map_err(|_| "breached-password host does not resolve".to_string())?
            .collect();
        if addrs.is_empty() {
            return Err("breached-password host does not resolve".into());
        }
        for addr in &addrs {
            let ip = addr.ip();
            let ok = !non_public_host(&ip.to_string()) || (self.allow_loopback && ip.is_loopback());
            if !ok {
                return Err("breached-password host resolves to a non-public address".into());
            }
        }
        Ok(addrs)
    }
}

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = *self;
        Box::pin(async move {
            let addrs = this.resolve_checked(name.as_str()).await?;
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

fn is_loopback(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare == "localhost" || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Permission check for an offline list file: the file and every directory
/// from its parent up to `/` must not be writable by group or other. A
/// shared sticky directory such as `/tmp` counts as too open. Returns a
/// description of the first problem found.
#[cfg(unix)]
pub fn unsafe_permissions(path: &str, meta: &std::fs::Metadata) -> Result<Option<String>, String> {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o022 != 0 {
        return Ok(Some("file is writable by group or other".into()));
    }
    let full = std::fs::canonicalize(path).map_err(|err| format!("{path}: {err}"))?;
    for dir in full.ancestors().skip(1) {
        let mode = std::fs::metadata(dir)
            .map_err(|err| format!("{}: {err}", dir.display()))?
            .permissions()
            .mode();
        if mode & 0o022 != 0 {
            return Ok(Some(format!(
                "directory {} is writable by group or other",
                dir.display()
            )));
        }
    }
    Ok(None)
}

impl OfflineList {
    /// One SHA-1 hex digest per line, optionally `:count`; blank lines and
    /// `#` comments are skipped. Read line by line and kept as sorted
    /// 20-byte digests (binary search), so top-N lists of a few million
    /// entries fit comfortably; the full corpus does not (use the range API).
    pub fn parse(reader: impl std::io::BufRead) -> Result<Self, String> {
        let mut digests = Vec::new();
        for (n, line) in reader.lines().enumerate() {
            let line = line.map_err(|err| format!("breached-password list: {err}"))?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let text = line.split(':').next().unwrap_or("").trim();
            let digest = (text.len() == 40)
                .then(|| parse_digest(text))
                .flatten()
                .ok_or_else(|| {
                    format!("breached-password list line {}: not a SHA-1 digest", n + 1)
                })?;
            digests.push(digest);
        }
        if digests.is_empty() {
            return Err("breached-password list is empty".into());
        }
        digests.sort_unstable();
        digests.dedup();
        Ok(Self(digests))
    }

    /// Loads a list file. A file, or any directory above it, writable by
    /// group or other is refused in production (fail closed) and only warned
    /// about elsewhere.
    pub fn load(path: &str, production: bool) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|err| format!("{path}: {err}"))?;
        #[cfg(unix)]
        {
            let meta = file.metadata().map_err(|err| format!("{path}: {err}"))?;
            if let Some(problem) = unsafe_permissions(path, &meta)? {
                if production {
                    return Err(format!(
                        "breached-password list {path}: {problem}; restrict it (for example chmod 644 for the file and 755 for its directories) before starting in production"
                    ));
                }
                tracing::warn!(%path, %problem, "breached-password list permissions are too open; restrict them (production refuses to start)");
            }
        }
        #[cfg(not(unix))]
        let _ = production;
        Self::parse(std::io::BufReader::new(file))
    }

    fn contains(&self, digest: &[u8; 20]) -> bool {
        self.0.binary_search(digest).is_ok()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl RangeApi {
    /// `production` refuses plain http entirely; elsewhere http is allowed
    /// to loopback only (local test servers). Non-public hosts are refused
    /// except loopback outside production.
    pub fn new(url: &str, production: bool) -> Result<Self, String> {
        let parsed = url::Url::parse(url).map_err(|err| format!("breached-password URL: {err}"))?;
        let host = parsed
            .host_str()
            .ok_or("breached-password URL needs a host")?
            .to_string();
        let local_test = !production && is_loopback(&host);
        match parsed.scheme() {
            "https" => {}
            "http" if local_test => {}
            _ => return Err("breached-password URL must be https".into()),
        }
        if non_public_host(&host) && !local_test {
            return Err(format!(
                "breached-password URL host {host} is not a public host"
            ));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("breached-password URL must not carry credentials".into());
        }
        let client = reqwest::Client::builder()
            // Backstop only: each lookup runs under its own configured
            // deadline (at most MAX_TIMEOUT), see `suffixes`.
            .timeout(MAX_TIMEOUT)
            .connect_timeout(MAX_TIMEOUT)
            .user_agent(concat!("vibe-sdk/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(std::sync::Arc::new(PublicResolver {
                allow_loopback: local_test,
            }))
            .build()
            .map_err(|err| err.to_string())?;
        let mut url = url.to_string();
        if !url.ends_with('/') {
            url.push('/');
        }
        Ok(Self {
            url,
            host,
            client,
            cache: Mutex::new(HashMap::new()),
            slots: tokio::sync::Semaphore::new(MAX_LOOKUPS),
            timeout: DEFAULT_TIMEOUT,
            failures: AtomicU64::new(0),
        })
    }

    /// Total per-lookup deadline (slot wait + connect + response),
    /// clamped to 100 ms ..= 10 s.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.clamp(MIN_TIMEOUT, MAX_TIMEOUT);
        self
    }

    /// The per-lookup timeout in use.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// Range lookups that failed (outage, bad response) since start.
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }

    fn cached(&self, prefix: &str) -> Option<std::sync::Arc<Suffixes>> {
        let cache = self.cache.lock().ok()?;
        cache
            .get(prefix)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, s)| s.clone())
    }

    async fn suffixes(&self, prefix: &str) -> Result<std::sync::Arc<Suffixes>, LookupError> {
        if let Some(hit) = self.cached(prefix) {
            return Ok(hit);
        }
        // One deadline for the whole lookup, so waiting for a slot and the
        // request together never exceed the configured timeout.
        tokio::time::timeout(self.timeout, self.lookup(prefix))
            .await
            .map_err(|_| LookupError::Timeout)?
    }

    async fn lookup(&self, prefix: &str) -> Result<std::sync::Arc<Suffixes>, LookupError> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| LookupError::Closed)?;
        if let Some(hit) = self.cached(prefix) {
            return Ok(hit);
        }
        let parsed = std::sync::Arc::new(self.fetch(prefix).await?);
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= CACHE_ENTRIES {
                cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
                if cache.len() >= CACHE_ENTRIES {
                    cache.clear();
                }
            }
            cache.insert(prefix.to_string(), (Instant::now(), parsed.clone()));
        }
        Ok(parsed)
    }

    async fn fetch(&self, prefix: &str) -> Result<Suffixes, LookupError> {
        let net = |err: reqwest::Error| {
            if err.is_timeout() {
                LookupError::Timeout
            } else {
                LookupError::Network
            }
        };
        let mut res = self
            .client
            .get(format!("{}{prefix}", self.url))
            .header("Add-Padding", "true")
            .send()
            .await
            .map_err(net)?;
        if res.status() != reqwest::StatusCode::OK {
            return Err(LookupError::Status(res.status().as_u16()));
        }
        let text_plain = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().to_ascii_lowercase().starts_with("text/plain"));
        if !text_plain {
            return Err(LookupError::ContentType);
        }
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(net)? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(LookupError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        parse_range(&body)
    }
}

/// Strict range-response parse: every non-empty line is `SUFFIX:COUNT` with
/// a 35-hex suffix and a decimal count, and there are at least
/// [`MIN_LINES`] lines. Anything else is an error (fail closed).
/// Errors name only the line number, never the line's content.
pub fn parse_range(body: &[u8]) -> Result<Suffixes, LookupError> {
    let text = std::str::from_utf8(body).map_err(|_| LookupError::Encoding)?;
    let mut out = Suffixes::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let bad = || LookupError::MalformedLine(n + 1);
        let (suffix, count) = line.split_once(':').ok_or_else(bad)?;
        if suffix.len() != 35 || count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        // 35 hex chars = 17.5 bytes; pad with a leading 0 nibble.
        let mut key = [0u8; 18];
        hex::decode_to_slice(format!("0{suffix}"), &mut key).map_err(|_| bad())?;
        let count: u64 = count.parse().map_err(|_| bad())?;
        out.insert(key, count);
    }
    if out.len() < MIN_LINES {
        return Err(LookupError::TooFewEntries);
    }
    Ok(out)
}

fn suffix_key(digest: &[u8; 20]) -> [u8; 18] {
    let hex = hex::encode_upper(digest);
    let mut key = [0u8; 18];
    hex::decode_to_slice(format!("0{}", &hex[5..]), &mut key).expect("hex");
    key
}

impl BreachedSource {
    pub fn builtin() -> Self {
        let list = BUILTIN
            .iter()
            .map(|p| hex::encode(sha1(p)))
            .collect::<Vec<_>>()
            .join("\n");
        Self::Offline(OfflineList::parse(list.as_bytes()).expect("builtin list"))
    }

    pub fn offline(text: &str) -> Result<Self, String> {
        OfflineList::parse(text.as_bytes()).map(Self::Offline)
    }

    pub fn range(url: &str, production: bool) -> Result<Self, String> {
        RangeApi::new(url, production).map(|api| Self::Range(Box::new(api)))
    }

    /// One line for the startup log, naming the external dependency.
    pub fn describe(&self) -> String {
        match self {
            Self::Range(api) => format!(
                "range API {} (only 5-hex SHA-1 prefixes are sent), fails closed during outages",
                api.host
            ),
            Self::Offline(list) => format!("offline list ({} digests)", list.len()),
        }
    }

    /// Whether `plain` was seen in breaches at least `min_count` times.
    /// `Err` when no source can answer (the caller fails closed).
    pub async fn is_breached(&self, plain: &str, min_count: u64) -> Result<bool, String> {
        let digest = sha1(plain);
        match self {
            Self::Offline(list) => Ok(list.contains(&digest)),
            Self::Range(api) => {
                let prefix = &hex::encode_upper(digest)[..5];
                match api.suffixes(prefix).await {
                    Ok(suffixes) => Ok(suffixes
                        .get(&suffix_key(&digest))
                        .is_some_and(|count| *count >= min_count.max(1))),
                    Err(err) => {
                        let failures = api.failures.fetch_add(1, Ordering::Relaxed) + 1;
                        LOOKUP_FAILURES.fetch_add(1, Ordering::Relaxed);
                        let status = match err {
                            LookupError::Status(code) => code,
                            _ => 0,
                        };
                        // Stable target and fields for alerting
                        // (docs/release-hardening.md). Only a fixed category
                        // and the status code are logged.
                        tracing::warn!(
                            target: "vibe::breached_password_unavailable",
                            reason = err.category(),
                            status,
                            failures,
                            failures_total = breached_lookup_failures(),
                            "breached-password range lookup failed"
                        );
                        Err(err.to_string())
                    }
                }
            }
        }
    }
}

impl BreachedConfig {
    pub fn builtin() -> Self {
        Self {
            source: BreachedSource::builtin(),
            min_count: 1,
        }
    }

    /// From the environment:
    /// - `VIBE_BREACHED_PASSWORDS_FILE`: offline list only, or
    /// - `VIBE_BREACHED_PASSWORDS_URL` (default Pwned Passwords). Errors,
    ///   timeouts and invalid responses fail closed (503 with Retry-After: 1)
    ///   in every environment;
    /// - `VIBE_BREACHED_PASSWORDS_MIN_COUNT` (default 1, at most
    ///   [`MAX_MIN_COUNT`]),
    ///   `VIBE_BREACHED_PASSWORDS_TIMEOUT_MS` (default 2000, 100..=10000:
    ///   one total deadline per lookup).
    pub fn from_env(
        get: &dyn Fn(&str) -> Option<String>,
        production: bool,
    ) -> Result<Self, String> {
        for name in [
            "VIBE_BREACHED_PASSWORDS_FALLBACK_FILE",
            "VIBE_BREACHED_PASSWORDS_FALLBACK_COMPLETE",
        ] {
            if get(name).is_some() {
                return Err(format!(
                    "{name} was removed; unset it. Range-API outages always fail closed"
                ));
            }
        }
        let var = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let file = var("VIBE_BREACHED_PASSWORDS_FILE");
        let url = var("VIBE_BREACHED_PASSWORDS_URL");
        let min_count = match var("VIBE_BREACHED_PASSWORDS_MIN_COUNT") {
            None => 1,
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=MAX_MIN_COUNT).contains(n))
                .ok_or_else(|| {
                    format!(
                        "VIBE_BREACHED_PASSWORDS_MIN_COUNT must be a whole number from 1 to {MAX_MIN_COUNT}"
                    )
                })?,
        };
        let timeout = match var("VIBE_BREACHED_PASSWORDS_TIMEOUT_MS") {
            None => DEFAULT_TIMEOUT,
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .map(Duration::from_millis)
                .filter(|t| (MIN_TIMEOUT..=MAX_TIMEOUT).contains(t))
                .ok_or_else(|| {
                    format!(
                        "VIBE_BREACHED_PASSWORDS_TIMEOUT_MS must be a whole number of milliseconds from {} to {}",
                        MIN_TIMEOUT.as_millis(),
                        MAX_TIMEOUT.as_millis()
                    )
                })?,
        };
        let source = match (file, url) {
            (Some(_), Some(_)) => {
                return Err(
                    "set VIBE_BREACHED_PASSWORDS_FILE or VIBE_BREACHED_PASSWORDS_URL, not both"
                        .into(),
                )
            }
            (Some(path), None) => BreachedSource::Offline(OfflineList::load(&path, production)?),
            (None, url) => {
                let api = RangeApi::new(url.as_deref().unwrap_or(DEFAULT_RANGE_URL), production)?
                    .with_timeout(timeout);
                BreachedSource::Range(Box::new(api))
            }
        };
        Ok(Self { source, min_count })
    }

    pub async fn is_breached(&self, plain: &str) -> Result<bool, String> {
        self.source.is_breached(plain, self.min_count).await
    }
}

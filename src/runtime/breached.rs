//! Breached-password check for `hash-password` (spec §4A.2, decision 7).
//! The password's SHA-1 is looked up k-anonymity style: only the first five
//! hex characters leave the platform; the plaintext and the full hash never
//! do. The source is a range API (Pwned Passwords format, `SUFFIX:COUNT`
//! lines) or an offline list of SHA-1 hashes. The check is mandatory, so an
//! unreachable source fails closed.

use std::collections::HashSet;
use std::time::Duration;

use sha1::{Digest, Sha1};

/// Default range API (Pwned Passwords).
pub const DEFAULT_RANGE_URL: &str = "https://api.pwnedpasswords.com/range/";
const RANGE_TIMEOUT: Duration = Duration::from_secs(3);

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

pub enum BreachedSource {
    /// `GET {url}{PREFIX}` with `Add-Padding: true`.
    Range {
        url: String,
        client: reqwest::Client,
    },
    /// Uppercase SHA-1 hex digests.
    Offline(HashSet<String>),
}

fn sha1_hex(plain: &str) -> String {
    hex::encode_upper(Sha1::digest(plain.as_bytes()))
}

impl BreachedSource {
    pub fn builtin() -> Self {
        Self::Offline(BUILTIN.iter().map(|p| sha1_hex(p)).collect())
    }

    pub fn range(url: &str) -> Result<Self, String> {
        let parsed = url::Url::parse(url).map_err(|err| format!("breached-password URL: {err}"))?;
        let loopback = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
            return Err("breached-password URL must be https".into());
        }
        let client = reqwest::Client::builder()
            .timeout(RANGE_TIMEOUT)
            .build()
            .map_err(|err| err.to_string())?;
        let mut url = url.to_string();
        if !url.ends_with('/') {
            url.push('/');
        }
        Ok(Self::Range { url, client })
    }

    /// Offline list: one SHA-1 hex digest per line, optionally `:count`;
    /// blank lines and `#` comments are skipped.
    pub fn offline(text: &str) -> Result<Self, String> {
        let mut set = HashSet::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let digest = line
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_uppercase();
            if digest.len() != 40 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "breached-password list line {}: not a SHA-1 digest",
                    n + 1
                ));
            }
            set.insert(digest);
        }
        Ok(Self::Offline(set))
    }

    /// From `VIBE_BREACHED_PASSWORDS_FILE` (offline) or
    /// `VIBE_BREACHED_PASSWORDS_URL` (range API, default Pwned Passwords).
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        let file = get("VIBE_BREACHED_PASSWORDS_FILE").filter(|v| !v.trim().is_empty());
        let url = get("VIBE_BREACHED_PASSWORDS_URL").filter(|v| !v.trim().is_empty());
        match (file, url) {
            (Some(_), Some(_)) => Err(
                "set VIBE_BREACHED_PASSWORDS_FILE or VIBE_BREACHED_PASSWORDS_URL, not both".into(),
            ),
            (Some(path), None) => {
                let text = std::fs::read_to_string(path.trim())
                    .map_err(|err| format!("VIBE_BREACHED_PASSWORDS_FILE: {err}"))?;
                Self::offline(&text)
            }
            (None, url) => Self::range(url.as_deref().unwrap_or(DEFAULT_RANGE_URL).trim()),
        }
    }

    /// Whether `plain` is a known breached password. `Err` when the source
    /// cannot answer (the caller fails closed).
    pub async fn is_breached(&self, plain: &str) -> Result<bool, String> {
        let digest = sha1_hex(plain);
        match self {
            Self::Offline(set) => Ok(set.contains(&digest)),
            Self::Range { url, client } => {
                let (prefix, suffix) = digest.split_at(5);
                let res = client
                    .get(format!("{url}{prefix}"))
                    .header("Add-Padding", "true")
                    .send()
                    .await
                    .map_err(|err| format!("breached-password lookup: {err}"))?;
                if !res.status().is_success() {
                    return Err(format!("breached-password lookup: HTTP {}", res.status()));
                }
                let body = res
                    .text()
                    .await
                    .map_err(|err| format!("breached-password lookup: {err}"))?;
                Ok(body.lines().any(|line| {
                    let mut parts = line.trim().split(':');
                    let hit = parts.next().is_some_and(|s| s.eq_ignore_ascii_case(suffix));
                    // Padding entries have count 0.
                    let count: u64 = parts
                        .next()
                        .and_then(|c| c.trim().parse().ok())
                        .unwrap_or(0);
                    hit && count > 0
                }))
            }
        }
    }
}

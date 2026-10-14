use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::diag::{Diag, Report};

pub const OPENAI: &str = "openai";
pub const ANTHROPIC: &str = "anthropic";
pub const GEMINI: &str = "gemini";

#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
    pub max_retries: u32,
    pub seed: u64,
    pub endpoint_host: String,
    pub endpoint_private: bool,
}

#[derive(Clone, Debug)]
pub struct LlmRequest {
    pub system: String,
    pub input: String,
    pub model: String,
    pub seed: u64,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct LlmResponse {
    pub text: String,
    pub model_reported: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Capabilities {
    pub seed: bool,
    pub tools: bool,
}

#[async_trait]
pub trait LlmAdapter: Send + Sync {
    fn provider(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    async fn compile(&self, request: &LlmRequest) -> Result<LlmResponse, String>;
}

pub struct FakeAdapter<F>
where
    F: Fn(&LlmRequest) -> Result<String, String> + Send + Sync,
{
    pub responder: F,
}

#[async_trait]
impl<F> LlmAdapter for FakeAdapter<F>
where
    F: Fn(&LlmRequest) -> Result<String, String> + Send + Sync,
{
    fn provider(&self) -> &'static str {
        "fake"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            seed: true,
            tools: false,
        }
    }

    async fn compile(&self, request: &LlmRequest) -> Result<LlmResponse, String> {
        Ok(LlmResponse {
            text: (self.responder)(request)?,
            model_reported: Some(request.model.clone()),
        })
    }
}

pub fn load_llm_config(
    env: &HashMap<String, String>,
    ci_or_prod: bool,
) -> Result<LlmConfig, Report> {
    let mut report = Report::new();
    let provider = env
        .get("LLM_PROVIDER")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| OPENAI.to_string());
    if !matches!(provider.as_str(), OPENAI | ANTHROPIC | GEMINI) {
        report.push(Diag::error(
            "env",
            "LLM_PROVIDER must be openai, anthropic, or gemini",
        ));
    }
    let alias_set = ["OPENAI_BASE_URL", "OPENAI_API_KEY", "OPENAI_MODEL"]
        .iter()
        .any(|name| env_present(env, name));
    if provider != OPENAI && alias_set {
        report.push(Diag::error(
            "env",
            "OPENAI_* variables are only valid when LLM_PROVIDER=openai",
        ));
    }
    let base = pick(
        env,
        "LLM_BASE_URL",
        "OPENAI_BASE_URL",
        provider == OPENAI,
        &mut report,
    );
    let key = pick(
        env,
        "LLM_API_KEY",
        "OPENAI_API_KEY",
        provider == OPENAI,
        &mut report,
    );
    let model = pick(
        env,
        "LLM_MODEL",
        "OPENAI_MODEL",
        provider == OPENAI,
        &mut report,
    );
    if key.as_deref().unwrap_or("").is_empty() {
        report.push(Diag::error("env", "LLM_API_KEY is missing or invalid"));
    }
    if model.as_deref().unwrap_or("").is_empty() {
        report.push(Diag::error("env", "LLM_MODEL is missing"));
    }
    if let Some(model) = &model {
        if !model_is_pinned(model) {
            report.push(Diag::error(
                "env",
                "LLM_MODEL must be a pinned snapshot id, not an alias like latest",
            ));
        }
    }
    let timeout = parse_timeout(
        env.get("LLM_TIMEOUT").map(String::as_str).unwrap_or("60s"),
        &mut report,
    );
    let max_retries = parse_retries(
        env.get("LLM_MAX_RETRIES")
            .map(String::as_str)
            .unwrap_or("2"),
        &mut report,
    );
    let seed = parse_seed(
        env.get("LLM_SEED").map(String::as_str).unwrap_or("0"),
        &mut report,
    );
    let allow_private = env_flag(env, "LLM_ALLOW_PRIVATE_HOST");
    if ci_or_prod && allow_private {
        report.push(Diag::error(
            "env",
            "LLM_ALLOW_PRIVATE_HOST is not allowed in CI or production",
        ));
    }
    let default_base = default_base_url(&provider);
    let base_url = base.unwrap_or_else(|| default_base.to_string());
    let endpoint = classify_endpoint(&base_url, allow_private, ci_or_prod);
    let (host, private) = match endpoint {
        Ok(info) => info,
        Err(message) => {
            report.push(Diag::error("env", message));
            (String::new(), false)
        }
    };
    if report.has_errors() {
        return Err(report);
    }
    Ok(LlmConfig {
        provider,
        base_url,
        api_key: key.unwrap_or_default(),
        model: model.unwrap_or_default(),
        timeout,
        max_retries,
        seed,
        endpoint_host: host,
        endpoint_private: private,
    })
}

fn env_present(env: &HashMap<String, String>, name: &str) -> bool {
    env.get(name).is_some_and(|value| !value.trim().is_empty())
}

fn env_flag(env: &HashMap<String, String>, name: &str) -> bool {
    matches!(
        env.get(name)
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn pick(
    env: &HashMap<String, String>,
    primary: &str,
    alias: &str,
    alias_ok: bool,
    report: &mut Report,
) -> Option<String> {
    let primary_v = env
        .get(primary)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let alias_v = if alias_ok {
        env.get(alias)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    } else {
        None
    };
    match (primary_v, alias_v) {
        (Some(a), Some(b)) if a != b => {
            report.push(Diag::error(
                "env",
                format!("{primary} and {alias} are both set and differ"),
            ));
            Some(a)
        }
        (Some(a), _) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

pub fn model_is_pinned(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    if lower.is_empty()
        || lower == "latest"
        || lower == "default"
        || lower.ends_with(":latest")
        || lower.ends_with("-latest")
    {
        return false;
    }
    if regex_has(r"20\d{2}-\d{2}-\d{2}", model) || regex_has(r"20\d{6}", model) {
        return true;
    }
    if model.contains(':') || model.contains('@') {
        return true;
    }
    regex_has(r"(?:^|[-_])v?\d{4,}(?:$|[-_.])", model)
}

fn regex_has(pattern: &str, text: &str) -> bool {
    regex::Regex::new(pattern)
        .expect("static regex")
        .is_match(text)
}

fn default_base_url(provider: &str) -> &'static str {
    match provider {
        ANTHROPIC => "https://api.anthropic.com",
        GEMINI => "https://generativelanguage.googleapis.com/v1beta",
        _ => "https://api.openai.com/v1",
    }
}

fn classify_endpoint(
    base: &str,
    allow_private: bool,
    ci_or_prod: bool,
) -> Result<(String, bool), String> {
    let parsed =
        url::Url::parse(base).map_err(|_| "LLM_BASE_URL is not an absolute URL".to_string())?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "LLM_BASE_URL is missing a host".to_string())?
        .to_string();
    let loopback = is_loopback(&host);
    let private = loopback || is_private_host(&host);
    if ci_or_prod && (private || parsed.scheme() != "https") {
        return Err("CI and production compilation require an https public host".into());
    }
    if !private && parsed.scheme() != "https" {
        return Err("public LLM hosts must use https".into());
    }
    if private && !loopback && !allow_private {
        return Err("private LLM hosts require LLM_ALLOW_PRIVATE_HOST=true".into());
    }
    if private && parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("LLM_BASE_URL scheme must be http or https".into());
    }
    Ok((host, private))
}

fn is_loopback(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn is_private_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.ends_with(".internal")
        || host.ends_with(".local")
    {
        return true;
    }
    if !host.contains('.') && !host.contains(':') {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return ip.is_private() || ip.is_loopback() || ip.is_link_local();
    }
    if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        return ip.is_loopback() || is_ula_or_link_local(ip);
    }
    false
}

fn is_ula_or_link_local(ip: std::net::Ipv6Addr) -> bool {
    let octets = ip.octets();
    octets[0] & 0xfe == 0xfc || octets[0] == 0xfe && octets[1] & 0xc0 == 0x80
}

fn parse_timeout(raw: &str, report: &mut Report) -> Duration {
    match parse_duration_ms(raw) {
        Some(ms) if ms > 0 && ms <= 300_000 => Duration::from_millis(ms),
        Some(_) => {
            report.push(Diag::error(
                "env",
                "LLM_TIMEOUT must be between 1ms and 300s",
            ));
            Duration::from_secs(60)
        }
        None => {
            report.push(Diag::error("env", "LLM_TIMEOUT is not a duration"));
            Duration::from_secs(60)
        }
    }
}

fn parse_retries(raw: &str, report: &mut Report) -> u32 {
    match raw.trim().parse::<u32>() {
        Ok(n) if n <= 5 => n,
        _ => {
            report.push(Diag::error(
                "env",
                "LLM_MAX_RETRIES must be an integer from 0 to 5",
            ));
            2
        }
    }
}

fn parse_seed(raw: &str, report: &mut Report) -> u64 {
    match raw.trim().parse::<u64>() {
        Ok(n) => n,
        Err(_) => {
            report.push(Diag::error("env", "LLM_SEED must be an integer"));
            0
        }
    }
}

pub fn parse_duration_ms(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Some(n) = raw.strip_suffix("ms") {
        return n.trim().parse().ok();
    }
    if let Some(n) = raw.strip_suffix('s') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|sec| sec.saturating_mul(1000));
    }
    if let Some(n) = raw.strip_suffix('m') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|min| min.saturating_mul(60_000));
    }
    if let Some(n) = raw.strip_suffix('h') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|hour| hour.saturating_mul(3_600_000));
    }
    if let Some(n) = raw.strip_suffix('d') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|day| day.saturating_mul(86_400_000));
    }
    raw.parse::<u64>().ok().map(|sec| sec.saturating_mul(1000))
}

pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if secret.len() >= 8 && *secret != "replace-me" {
            out = out.replace(secret, "[redacted]");
        }
    }
    out
}

pub struct HttpAdapter {
    pub config: LlmConfig,
    client: Client,
}

impl HttpAdapter {
    pub fn new(config: LlmConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|err| format!("http client: {err}"))?;
        Ok(Self { config, client })
    }
}

#[async_trait]
impl LlmAdapter for HttpAdapter {
    fn provider(&self) -> &'static str {
        match self.config.provider.as_str() {
            ANTHROPIC => "anthropic",
            GEMINI => "gemini",
            _ => "openai",
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            seed: self.config.provider != ANTHROPIC,
            tools: false,
        }
    }

    async fn compile(&self, request: &LlmRequest) -> Result<LlmResponse, String> {
        let mut attempt = 0u32;
        let max = self.config.max_retries;
        loop {
            match self.compile_once(request).await {
                Ok(response) => return Ok(response),
                Err(err) if err.retryable && attempt < max => {
                    attempt += 1;
                }
                Err(err) => {
                    return Err(redact(&err.message, &[&self.config.api_key]));
                }
            }
        }
    }
}

struct CallError {
    message: String,
    retryable: bool,
}

impl HttpAdapter {
    async fn compile_once(&self, request: &LlmRequest) -> Result<LlmResponse, CallError> {
        let (url, body, headers) = match self.config.provider.as_str() {
            ANTHROPIC => anthropic_call(&self.config, request),
            GEMINI => gemini_call(&self.config, request),
            _ => openai_call(&self.config, request),
        };
        let mut builder = self.client.post(&url).json(&body);
        for (key, value) in headers {
            builder = builder.header(key, value);
        }
        let response = builder.send().await.map_err(|err| CallError {
            message: format!("LLM request failed: {err}"),
            retryable: true,
        })?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|err| CallError {
            message: format!("LLM response body: {err}"),
            retryable: true,
        })?;
        if !status.is_success() {
            return Err(CallError {
                message: format!(
                    "LLM_API_KEY is missing or invalid (http {})",
                    status.as_u16()
                ),
                retryable: status.is_server_error() || status.as_u16() == 429,
            });
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| CallError {
            message: "LLM response was not JSON".into(),
            retryable: false,
        })?;
        let parsed = match self.config.provider.as_str() {
            ANTHROPIC => parse_anthropic(&value),
            GEMINI => parse_gemini(&value),
            _ => parse_openai(&value),
        }
        .map_err(|message| CallError {
            message,
            retryable: false,
        })?;
        if let Some(reported) = &parsed.model_reported {
            if reported != &request.model && !reported.is_empty() {
                return Err(CallError {
                    message: format!(
                        "provider reported model {reported}, expected {}",
                        request.model
                    ),
                    retryable: false,
                });
            }
        }
        Ok(parsed)
    }
}

pub fn openai_body(request: &LlmRequest, seed: u64, send_seed: bool) -> Value {
    let mut body = json!({
        "model": request.model,
        "temperature": 0,
        "top_p": 1,
        "messages": [
            {"role": "system", "content": request.system},
            {"role": "user", "content": request.input}
        ]
    });
    if send_seed {
        body["seed"] = json!(seed);
    }
    body
}

pub fn anthropic_body(request: &LlmRequest) -> Value {
    json!({
        "model": request.model,
        "max_tokens": 8192,
        "temperature": 0,
        "system": request.system,
        "messages": [{"role": "user", "content": request.input}]
    })
}

pub fn gemini_body(request: &LlmRequest, seed: u64) -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": request.system}]},
        "contents": [{"role": "user", "parts": [{"text": request.input}]}],
        "generationConfig": {"temperature": 0, "topP": 1, "topK": 1, "seed": seed}
    })
}

fn openai_call(
    config: &LlmConfig,
    request: &LlmRequest,
) -> (String, Value, Vec<(&'static str, String)>) {
    let url = join_url(&config.base_url, "chat/completions");
    let body = openai_body(request, config.seed, true);
    let headers = vec![("Authorization", format!("Bearer {}", config.api_key))];
    (url, body, headers)
}

fn anthropic_call(
    config: &LlmConfig,
    request: &LlmRequest,
) -> (String, Value, Vec<(&'static str, String)>) {
    let url = join_url(&config.base_url, "v1/messages");
    let body = anthropic_body(request);
    let headers = vec![
        ("x-api-key", config.api_key.clone()),
        ("anthropic-version", "2023-06-01".into()),
    ];
    (url, body, headers)
}

fn gemini_call(
    config: &LlmConfig,
    request: &LlmRequest,
) -> (String, Value, Vec<(&'static str, String)>) {
    let url = join_url(
        &config.base_url,
        &format!("models/{}:generateContent", request.model),
    );
    let body = gemini_body(request, config.seed);
    let headers = vec![("x-goog-api-key", config.api_key.clone())];
    (url, body, headers)
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn parse_openai(value: &Value) -> Result<LlmResponse, String> {
    let text = value["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("openai response missing message content")?
        .to_string();
    let model_reported = value["model"].as_str().map(str::to_string);
    Ok(LlmResponse {
        text,
        model_reported,
    })
}

fn parse_anthropic(value: &Value) -> Result<LlmResponse, String> {
    let text = value["content"]
        .as_array()
        .and_then(|parts| parts.iter().find_map(|part| part["text"].as_str()))
        .ok_or("anthropic response missing text")?
        .to_string();
    let model_reported = value["model"].as_str().map(str::to_string);
    Ok(LlmResponse {
        text,
        model_reported,
    })
}

fn parse_gemini(value: &Value) -> Result<LlmResponse, String> {
    let text = value["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or("gemini response missing text")?
        .to_string();
    let model_reported = value["modelVersion"].as_str().map(str::to_string);
    Ok(LlmResponse {
        text,
        model_reported,
    })
}

pub fn env_map_from_process() -> HashMap<String, String> {
    std::env::vars().collect()
}

pub fn runtime_llm_env_present() -> Option<String> {
    std::env::vars()
        .find(|(key, value)| {
            !value.is_empty() && (key.starts_with("LLM_") || key.starts_with("OPENAI_"))
        })
        .map(|(key, _)| key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn rejects_alias_conflict_and_latest() {
        let report = load_llm_config(
            &env(&[
                ("LLM_MODEL", "gpt-4o-2024-08-06"),
                ("OPENAI_MODEL", "other-model-2024-08-06"),
                ("LLM_API_KEY", "sk-test-value"),
            ]),
            false,
        );
        assert!(report.is_err());
        let bad_model = load_llm_config(
            &env(&[("LLM_API_KEY", "sk-test-value"), ("LLM_MODEL", "latest")]),
            false,
        );
        assert!(bad_model.is_err());
    }

    #[test]
    fn openai_alias_and_loopback() {
        let cfg = load_llm_config(
            &env(&[
                ("OPENAI_API_KEY", "sk-local-placeholder"),
                ("OPENAI_MODEL", "qwen2.5-coder:32b-instruct-q8_0"),
                ("OPENAI_BASE_URL", "http://127.0.0.1:11434/v1"),
            ]),
            false,
        )
        .expect("config");
        assert_eq!(cfg.provider, "openai");
        assert!(cfg.endpoint_private);
        assert_eq!(cfg.endpoint_host, "127.0.0.1");
    }

    #[test]
    fn ci_rejects_loopback() {
        let report = load_llm_config(
            &env(&[
                ("LLM_API_KEY", "sk-test-value"),
                ("LLM_MODEL", "vendor-model-2026-09-01"),
                ("LLM_BASE_URL", "http://localhost:11434/v1"),
            ]),
            true,
        );
        assert!(report.is_err());
    }

    #[test]
    fn non_openai_rejects_openai_env() {
        let report = load_llm_config(
            &env(&[
                ("LLM_PROVIDER", "anthropic"),
                ("LLM_API_KEY", "sk-ant-test-value"),
                ("LLM_MODEL", "claude-sonnet-2026-09-01"),
                ("OPENAI_API_KEY", "sk-test-value"),
            ]),
            false,
        );
        assert!(report.is_err());
    }

    #[test]
    fn deterministic_bodies_have_no_tools() {
        let request = LlmRequest {
            system: "sys".into(),
            input: "in".into(),
            model: "vendor-model-2026-09-01".into(),
            seed: 0,
            timeout: Duration::from_secs(1),
        };
        let openai = openai_body(&request, 0, true);
        assert_eq!(openai["temperature"], 0);
        assert_eq!(openai["top_p"], 1);
        assert_eq!(openai["seed"], 0);
        assert!(openai.get("tools").is_none());
        assert!(openai.get("functions").is_none());
        assert!(openai.get("tool_choice").is_none());
        let anthropic = anthropic_body(&request);
        assert_eq!(anthropic["temperature"], 0);
        assert!(anthropic.get("tools").is_none());
        assert!(anthropic.get("top_p").is_none());
        let gemini = gemini_body(&request, 0);
        assert_eq!(gemini["generationConfig"]["temperature"], 0);
        assert_eq!(gemini["generationConfig"]["topK"], 1);
        assert!(gemini.get("tools").is_none());
        assert!(gemini.get("toolConfig").is_none());
    }

    #[test]
    fn capabilities_never_enable_tools() {
        let cfg = load_llm_config(
            &env(&[
                ("LLM_API_KEY", "sk-test-value-1234"),
                ("LLM_MODEL", "vendor-model-2026-09-01"),
            ]),
            false,
        )
        .expect("config");
        let adapter = HttpAdapter::new(cfg).expect("adapter");
        assert!(!adapter.capabilities().tools);
        assert!(adapter.capabilities().seed);
    }
}

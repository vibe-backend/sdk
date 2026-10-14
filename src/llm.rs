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
    /// OpenAI API style: `chat` (default) or `responses` (`LLM_API_STYLE`, #40).
    pub api_style: String,
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
    pub system_fingerprint: Option<String>,
}

/// Sampling parameters the adapter is allowed to send for one model.
///
/// Absent fields are omitted from the request. Unregistered models send none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SentParams {
    pub temperature: Option<String>,
    pub top_p: Option<String>,
    pub top_k: Option<String>,
    pub seed: Option<String>,
    pub reasoning_effort: Option<String>,
}

impl SentParams {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.seed.is_none()
            && self.reasoning_effort.is_none()
    }

    pub fn to_map(&self) -> std::collections::BTreeMap<String, String> {
        let mut map = std::collections::BTreeMap::new();
        let mut put = |key: &str, value: &Option<String>| {
            if let Some(value) = value {
                map.insert(key.to_string(), value.clone());
            }
        };
        put("reasoning_effort", &self.reasoning_effort);
        put("seed", &self.seed);
        put("temperature", &self.temperature);
        put("top_k", &self.top_k);
        put("top_p", &self.top_p);
        map
    }
}

/// Capability table shipped with this SDK version (spec 2A.8.6, decision 58).
///
/// Gemini accepts temperature, topP, topK, and seed for every model. OpenAI
/// sends sampling parameters only for models that also accept
/// `reasoning_effort: none` (GPT-5.1 and GPT-5.2). Everything else, including
/// Anthropic and local OpenAI-compatible tags, gets no sampling parameters.
pub fn model_capabilities(provider: &str, model: &str, seed: u64) -> SentParams {
    let model = model.to_ascii_lowercase();
    if provider == GEMINI {
        return SentParams {
            temperature: Some("0".into()),
            top_p: Some("1".into()),
            top_k: Some("1".into()),
            seed: Some(seed.to_string()),
            reasoning_effort: None,
        };
    }
    if provider == OPENAI && (model.contains("gpt-5.1") || model.contains("gpt-5.2")) {
        return SentParams {
            temperature: Some("0".into()),
            top_p: Some("1".into()),
            top_k: None,
            seed: Some(seed.to_string()),
            reasoning_effort: Some("none".into()),
        };
    }
    SentParams::default()
}

/// Sampling parameters for one config and model, by API style. This is the
/// single map both the request body and the lock's `sent-params` stamp use.
/// The Responses API has no `seed`, so it is dropped for `responses`.
pub fn sent_params(config: &LlmConfig, model: &str) -> SentParams {
    let mut params = model_capabilities(&config.provider, model, config.seed);
    if config.provider == OPENAI && config.api_style == "responses" {
        params.seed = None;
    }
    params
}

/// Provider name recorded in the lock: `openai-responses` when the OpenAI
/// Responses API is used (#40), otherwise the provider itself.
pub fn lock_provider(config: &LlmConfig) -> String {
    if config.provider == OPENAI && config.api_style == "responses" {
        "openai-responses".into()
    } else {
        config.provider.clone()
    }
}

pub fn sampling_registered(provider: &str, model: &str) -> bool {
    !model_capabilities(provider, model, 0).is_empty()
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
            system_fingerprint: None,
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
    let api_style = env
        .get("LLM_API_STYLE")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "chat".into());
    if !matches!(api_style.as_str(), "chat" | "responses") {
        report.push(Diag::error(
            "env",
            "LLM_API_STYLE must be chat or responses",
        ));
        return Err(report);
    }
    if api_style == "responses" && provider != OPENAI {
        report.push(Diag::error(
            "env",
            "LLM_API_STYLE=responses is only valid with LLM_PROVIDER=openai",
        ));
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
        api_style,
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
    !FLOATING_ALIASES.contains(&lower.as_str())
}

const FLOATING_ALIASES: &[&str] = &[
    "chatgpt-4o-latest",
    "gpt-3.5-turbo",
    "gpt-4",
    "gpt-4-turbo",
    "gpt-4o",
    "gpt-4o-mini",
    "gpt-5",
    "gpt-5-mini",
    "o1",
    "o1-mini",
    "o3",
    "o3-mini",
    "o4-mini",
];

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
    crate::netguard::is_loopback_host(host)
}

pub(crate) fn is_private_host(host: &str) -> bool {
    crate::netguard::is_non_public_host(host)
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

/// Masks API keys and common secret query/fragment params. Shared by
/// `describe_failure` and transport-error paths so secrets are never printed.
pub fn mask_secrets(text: &str, api_key: &str) -> String {
    let mut out = text.to_string();
    if api_key.len() >= 4 {
        out = out.replace(api_key, "***");
    }
    // Query, fragment, and `;`-separated params: key / api_key / token / …
    static KEY_PARAM: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)([?&#;](?:key|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|private[_-]?key|token|secret|password|auth)=)[^&\s#;]*",
        )
        .expect("regex")
    });
    KEY_PARAM.replace_all(&out, "${1}***").into_owned()
}

/// Explains a failed or non-JSON LLM response. Only 401/403 blame the key;
/// HTML (or a 404) hints at the base URL. The request URL and a short body
/// excerpt are included, with the API key and secret query values masked.
pub fn describe_failure(
    url: &str,
    status: u16,
    content_type: &str,
    body: &[u8],
    api_key: &str,
) -> String {
    let shown_url = mask_secrets(url, api_key);
    let raw = String::from_utf8_lossy(body);
    let excerpt: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    // Mask the full body first so a key straddling the 200-char cut cannot leak.
    let excerpt = mask_secrets(&excerpt, api_key);
    let excerpt: String = excerpt.chars().take(200).collect();
    let html = content_type.to_ascii_lowercase().contains("text/html")
        || excerpt
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<!doctype html")
        || excerpt
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<html");
    let lead = match status {
        401 | 403 => format!("LLM_API_KEY was rejected (http {status})"),
        404 => format!("LLM endpoint not found (http {status})"),
        429 => format!("LLM rate limit or quota exceeded (http {status})"),
        400 => format!("LLM request rejected (http {status}); check LLM_MODEL and the provider"),
        s if s >= 500 => format!("LLM provider error (http {s})"),
        s if (200..300).contains(&s) => "LLM response was not JSON".to_string(),
        s => format!("LLM request failed (http {s})"),
    };
    let hint = if html || status == 404 {
        "; the response looks like a web page, not an API: check LLM_BASE_URL (OpenAI-compatible endpoints usually end in /v1)"
    } else {
        ""
    };
    format!("{lead}{hint}. URL: {shown_url}. Response: {excerpt}")
}

impl HttpAdapter {
    pub fn new(config: LlmConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(config.timeout)
            // A redirect could lead to a host the endpoint check never saw.
            .redirect(reqwest::redirect::Policy::none())
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
            _ if self.config.api_style == "responses" => "openai-responses",
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
            _ if self.config.api_style == "responses" => {
                openai_responses_call(&self.config, request)
            }
            _ => openai_call(&self.config, request),
        };
        let mut builder = self.client.post(&url).json(&body);
        for (key, value) in headers {
            builder = builder.header(key, value);
        }
        let response = builder.send().await.map_err(|err| CallError {
            message: mask_secrets(&format!("LLM request failed: {err}"), &self.config.api_key),
            retryable: true,
        })?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response.bytes().await.map_err(|err| CallError {
            message: mask_secrets(&format!("LLM response body: {err}"), &self.config.api_key),
            retryable: true,
        })?;
        if !status.is_success() {
            return Err(CallError {
                message: describe_failure(
                    &url,
                    status.as_u16(),
                    &content_type,
                    &bytes,
                    &self.config.api_key,
                ),
                retryable: status.is_server_error() || status.as_u16() == 429,
            });
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| CallError {
            message: describe_failure(
                &url,
                status.as_u16(),
                &content_type,
                &bytes,
                &self.config.api_key,
            ),
            retryable: false,
        })?;
        let parsed = match self.config.provider.as_str() {
            ANTHROPIC => parse_anthropic(&value),
            GEMINI => parse_gemini(&value),
            _ if self.config.api_style == "responses" => {
                parse_openai_responses_masked(&value, &self.config.api_key)
            }
            _ => parse_openai(&value),
        }
        .map_err(|message| CallError {
            message: mask_secrets(&message, &self.config.api_key),
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

pub fn openai_body(request: &LlmRequest, params: &SentParams) -> Value {
    let mut body = json!({
        "model": request.model,
        "messages": [
            {"role": "system", "content": request.system},
            {"role": "user", "content": request.input}
        ]
    });
    apply_number(&mut body, "temperature", params.temperature.as_deref());
    apply_number(&mut body, "top_p", params.top_p.as_deref());
    apply_number(&mut body, "seed", params.seed.as_deref());
    if let Some(effort) = &params.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

pub fn anthropic_body(request: &LlmRequest, params: &SentParams) -> Value {
    let mut body = json!({
        "model": request.model,
        "max_tokens": 8192,
        "system": request.system,
        "messages": [{"role": "user", "content": request.input}]
    });
    apply_number(&mut body, "temperature", params.temperature.as_deref());
    body
}

pub fn gemini_body(request: &LlmRequest, params: &SentParams) -> Value {
    let mut generation = serde_json::Map::new();
    if let Some(value) = params.temperature.as_deref() {
        generation.insert("temperature".into(), number_value(value));
    }
    if let Some(value) = params.top_p.as_deref() {
        generation.insert("topP".into(), number_value(value));
    }
    if let Some(value) = params.top_k.as_deref() {
        generation.insert("topK".into(), number_value(value));
    }
    if let Some(value) = params.seed.as_deref() {
        generation.insert("seed".into(), number_value(value));
    }
    json!({
        "systemInstruction": {"parts": [{"text": request.system}]},
        "contents": [{"role": "user", "parts": [{"text": request.input}]}],
        "generationConfig": generation
    })
}

fn apply_number(body: &mut Value, key: &str, raw: Option<&str>) {
    if let Some(raw) = raw {
        body[key] = number_value(raw);
    }
}

fn number_value(raw: &str) -> Value {
    raw.parse::<i64>()
        .map(|n| json!(n))
        .unwrap_or_else(|_| json!(raw.parse::<f64>().unwrap_or(0.0)))
}

fn openai_call(
    config: &LlmConfig,
    request: &LlmRequest,
) -> (String, Value, Vec<(&'static str, String)>) {
    let url = join_url(&config.base_url, "chat/completions");
    let params = sent_params(config, &request.model);
    let body = openai_body(request, &params);
    let headers = vec![("Authorization", format!("Bearer {}", config.api_key))];
    (url, body, headers)
}

fn anthropic_call(
    config: &LlmConfig,
    request: &LlmRequest,
) -> (String, Value, Vec<(&'static str, String)>) {
    let url = join_url(&config.base_url, "v1/messages");
    let params = sent_params(config, &request.model);
    let body = anthropic_body(request, &params);
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
    let params = sent_params(config, &request.model);
    let body = gemini_body(request, &params);
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
    let system_fingerprint = value["system_fingerprint"].as_str().map(str::to_string);
    Ok(LlmResponse {
        text,
        model_reported,
        system_fingerprint,
    })
}

/// Body for `POST /v1/responses` (#40). Sampling params are sent the same way
/// as chat; `reasoning_effort` maps to `reasoning.effort`.
pub fn openai_responses_body(request: &LlmRequest, params: &SentParams) -> Value {
    let mut body = json!({
        "model": request.model,
        "instructions": request.system,
        "input": request.input,
        "store": false
    });
    apply_number(&mut body, "temperature", params.temperature.as_deref());
    apply_number(&mut body, "top_p", params.top_p.as_deref());
    apply_number(&mut body, "seed", params.seed.as_deref());
    if let Some(effort) = &params.reasoning_effort {
        body["reasoning"] = json!({"effort": effort});
    }
    body
}

fn openai_responses_call(
    config: &LlmConfig,
    request: &LlmRequest,
) -> (String, Value, Vec<(&'static str, String)>) {
    let url = join_url(&config.base_url, "responses");
    let params = sent_params(config, &request.model);
    let body = openai_responses_body(request, &params);
    let headers = vec![("Authorization", format!("Bearer {}", config.api_key))];
    (url, body, headers)
}

/// Reads `output_text`, or concatenates `output[].content[]` text parts.
/// A response with `status: incomplete`, `failed` or `error`, or a non-null
/// `error`, is an error even when some text was produced.
pub fn parse_openai_responses(value: &Value) -> Result<LlmResponse, String> {
    parse_openai_responses_masked(value, "")
}

/// Like [`parse_openai_responses`]; provider error messages are masked
/// (API key, `key=`-style values) before they are cut to 200 characters.
pub fn parse_openai_responses_masked(value: &Value, api_key: &str) -> Result<LlmResponse, String> {
    let status = value["status"].as_str().unwrap_or("");
    if status == "incomplete" {
        let reason = value["incomplete_details"]["reason"]
            .as_str()
            .unwrap_or("unknown");
        return Err(format!("openai responses output is incomplete ({reason})"));
    }
    if matches!(status, "failed" | "error") || !value["error"].is_null() {
        let error = &value["error"];
        let code = error["code"].as_str().unwrap_or("unknown");
        let message = mask_secrets(error["message"].as_str().unwrap_or("no message"), api_key);
        let message: String = message.chars().take(200).collect();
        return Err(format!(
            "openai responses returned an error ({code}): {message}"
        ));
    }
    let text = if let Some(text) = value["output_text"].as_str() {
        text.to_string()
    } else {
        let mut out = String::new();
        for item in value["output"].as_array().into_iter().flatten() {
            if item["type"].as_str().is_some_and(|t| t != "message") {
                continue;
            }
            for part in item["content"].as_array().into_iter().flatten() {
                if let Some(t) = part["text"].as_str() {
                    out.push_str(t);
                }
            }
        }
        out
    };
    if text.is_empty() {
        return Err("openai responses output has no text".into());
    }
    Ok(LlmResponse {
        text,
        model_reported: value["model"].as_str().map(str::to_string),
        system_fingerprint: None,
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
        system_fingerprint: None,
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
        system_fingerprint: None,
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
        let unknown = model_capabilities(OPENAI, &request.model, 0);
        assert!(unknown.is_empty());
        let openai = openai_body(&request, &unknown);
        assert!(openai.get("temperature").is_none());
        assert!(openai.get("seed").is_none());
        assert!(openai.get("tools").is_none());
        assert!(openai.get("functions").is_none());
        assert!(openai.get("tool_choice").is_none());
        let pinned = model_capabilities(OPENAI, "gpt-5.1", 0);
        let openai_pinned = openai_body(&request, &pinned);
        assert_eq!(openai_pinned["temperature"], 0);
        assert_eq!(openai_pinned["top_p"], 1);
        assert_eq!(openai_pinned["seed"], 0);
        assert_eq!(openai_pinned["reasoning_effort"], "none");
        let anthropic = anthropic_body(&request, &SentParams::default());
        assert!(anthropic.get("temperature").is_none());
        assert!(anthropic.get("tools").is_none());
        assert!(anthropic.get("top_p").is_none());
        let gemini = gemini_body(&request, &model_capabilities(GEMINI, &request.model, 0));
        assert_eq!(gemini["generationConfig"]["temperature"], 0);
        assert_eq!(gemini["generationConfig"]["topK"], 1);
        assert!(gemini.get("tools").is_none());
        assert!(gemini.get("toolConfig").is_none());
    }

    #[test]
    fn undated_fixed_snapshot_is_allowed() {
        assert!(model_is_pinned("claude-opus-4-6"));
        assert!(!model_is_pinned("gpt-4o"));
        assert!(!model_is_pinned("vendor:latest"));
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

#[cfg(test)]
mod failure_message_tests {
    use super::describe_failure;

    #[test]
    fn only_auth_failures_blame_the_key() {
        let m = describe_failure(
            "https://api.x/v1/chat/completions",
            401,
            "application/json",
            b"{}",
            "sk-secret",
        );
        assert!(m.contains("LLM_API_KEY was rejected"));
        for status in [400, 404, 429, 500] {
            let m = describe_failure(
                "https://api.x/v1/chat/completions",
                status,
                "application/json",
                b"{}",
                "k",
            );
            assert!(!m.contains("LLM_API_KEY"), "{status}: {m}");
            assert!(m.contains(&status.to_string()));
        }
    }

    #[test]
    fn html_hints_at_the_base_url_and_secrets_are_masked() {
        let m = describe_failure(
            "https://api.openai.com/chat/completions",
            200,
            "text/html; charset=utf-8",
            b"<!DOCTYPE html><html>sk-secret-value</html>",
            "sk-secret-value",
        );
        assert!(m.contains("not JSON") && m.contains("/v1"), "{m}");
        assert!(!m.contains("sk-secret-value"), "{m}");
        let m = describe_failure(
            "https://g.example/v1beta/models/x:generateContent?key=AIzaSECRET",
            404,
            "text/html",
            b"<html>nope</html>",
            "AIzaSECRET",
        );
        assert!(!m.contains("AIzaSECRET") && m.contains("key=***"), "{m}");
        assert!(m.contains("LLM_BASE_URL"));
    }

    #[test]
    fn mask_runs_before_truncate_so_straddling_keys_do_not_leak() {
        let key = "sk-STRADDLE-SECRET-KEY-VALUE";
        // 190 filler chars + key → key straddles the 200-char boundary if we
        // truncated first; masking first removes it entirely.
        let filler: String = "x".repeat(190);
        let body = format!("{filler} {key} trailing");
        let m = describe_failure(
            "https://api.example/v1/chat",
            500,
            "text/plain",
            body.as_bytes(),
            key,
        );
        assert!(!m.contains("sk-STRADDLE"), "{m}");
        assert!(!m.contains(key), "{m}");
        assert!(m.contains("***"), "{m}");
    }

    #[test]
    fn query_and_fragment_tokens_are_masked() {
        for url in [
            "https://api.example/v1?access_token=tok123&x=1",
            "https://api.example/v1?password=s3cret&token=tOk",
            "https://api.example/v1#api_key=fragsecret",
            "https://api.example/v1?a=1;secret=s3cret",
            "https://api.example/v1?apikey=ak1value",
        ] {
            let m = describe_failure(url, 400, "text/plain", b"ok", "unused-key-here");
            assert!(!m.contains("tok123"), "{m}");
            assert!(!m.contains("fragsecret"), "{m}");
            assert!(!m.contains("s3cret"), "{m}");
            assert!(!m.contains("ak1value"), "{m}");
            assert!(!m.contains("tOk"), "{m}");
            assert!(m.contains("***"), "{m}");
        }
    }
}

#[cfg(test)]
mod responses_tests {
    use super::*;

    #[test]
    fn error_messages_are_masked_before_truncation() {
        let key = "sk-test-secret-value-1234567890";
        let long = format!("{}{key} and api_key=abc123secret", "x".repeat(190));
        let err = parse_openai_responses_masked(
            &json!({"status": "failed", "error": {"code": "bad", "message": long}}),
            key,
        )
        .unwrap_err();
        assert!(!err.contains("sk-test-secret"), "{err}");
        assert!(!err.contains("sk-test"), "partial key leaked: {err}");
        assert!(err.contains("***"), "{err}");
        let err = parse_openai_responses(&json!({
            "error": {"code": "bad", "message": "see https://x/?api_key=abc123secret"}
        }))
        .unwrap_err();
        assert!(!err.contains("abc123secret"), "{err}");
    }

    #[tokio::test]
    async fn responses_http_errors_are_reported_and_masked() {
        use axum::{http::StatusCode, routing::post, Router};
        let key = "sk-test-value";
        for (status, retry) in [
            (400u16, false),
            (401, false),
            (404, false),
            (429, true),
            (500, true),
            (503, true),
        ] {
            let body = format!("{{\"error\":{{\"message\":\"bad key {key}\"}}}}");
            let app = Router::new().route(
                "/v1/responses",
                post(move || {
                    let body = body.clone();
                    async move { (StatusCode::from_u16(status).unwrap(), body) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let model = "gpt-5.1-2026-01-01";
            let mut config = responses_config(model);
            config.base_url = format!("http://{addr}/v1");
            config.max_retries = 0;
            let adapter = HttpAdapter::new(config).unwrap();
            let request = LlmRequest {
                system: "s".into(),
                input: "i".into(),
                model: model.into(),
                seed: 7,
                timeout: Duration::from_secs(5),
            };
            let err = adapter.compile(&request).await.unwrap_err();
            assert!(err.contains(&status.to_string()), "{status}: {err}");
            assert!(!err.contains(key), "{status}: {err}");
            let _ = retry;
        }
    }

    fn responses_config(model: &str) -> LlmConfig {
        LlmConfig {
            provider: OPENAI.into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: "sk-test-value".into(),
            model: model.into(),
            timeout: Duration::from_secs(5),
            max_retries: 0,
            seed: 7,
            endpoint_host: "api.openai.com".into(),
            endpoint_private: false,
            api_style: "responses".into(),
        }
    }

    #[test]
    fn stamp_and_body_share_one_capability_map() {
        for model in [
            "gpt-5.1-2026-01-01",
            "gpt-5.2-2026-01-01",
            "gpt-4o-2024-08-06",
        ] {
            let config = responses_config(model);
            let params = sent_params(&config, model);
            assert!(params.seed.is_none(), "responses never sends seed");
            let request = LlmRequest {
                system: "s".into(),
                input: "i".into(),
                model: model.into(),
                seed: config.seed,
                timeout: config.timeout,
            };
            let (_, body, _) = openai_responses_call(&config, &request);
            let stamp = params.to_map();
            for (key, field) in [
                ("temperature", "temperature"),
                ("top_p", "top_p"),
                ("seed", "seed"),
            ] {
                assert_eq!(
                    stamp.contains_key(key),
                    body.get(field).is_some(),
                    "{model} {key}"
                );
            }
            assert_eq!(
                stamp.contains_key("reasoning_effort"),
                body.get("reasoning").is_some(),
                "{model}"
            );
        }
        let mut chat = responses_config("gpt-5.1-2026-01-01");
        chat.api_style = "chat".into();
        assert_eq!(sent_params(&chat, &chat.model).seed.as_deref(), Some("7"));
    }

    #[test]
    fn lock_provider_records_the_api_style() {
        let mut config = responses_config("gpt-5.1-2026-01-01");
        assert_eq!(lock_provider(&config), "openai-responses");
        config.api_style = "chat".into();
        assert_eq!(lock_provider(&config), "openai");
    }

    #[test]
    fn incomplete_and_error_statuses_are_errors() {
        let err = parse_openai_responses(&json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output_text": "## partial"
        }))
        .unwrap_err();
        assert!(
            err.contains("incomplete") && err.contains("max_output_tokens"),
            "{err}"
        );
        for value in [
            json!({"status": "failed", "error": {"code": "server_error", "message": "boom"}}),
            json!({"status": "error", "error": {"code": "server_error", "message": "boom"}}),
            json!({"error": {"code": "server_error", "message": "boom"}, "output_text": "x"}),
        ] {
            let err = parse_openai_responses(&value).unwrap_err();
            assert!(
                err.contains("server_error") && err.contains("boom"),
                "{err}"
            );
        }
        assert!(parse_openai_responses(
            &json!({"status": "completed", "error": null, "output_text": "ok"})
        )
        .is_ok());
    }

    #[tokio::test]
    async fn responses_adapter_against_a_mock_server() {
        use axum::{routing::post, Json, Router};
        let reply = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let app = {
            let reply = reply.clone();
            let seen = seen.clone();
            Router::new().route(
                "/v1/responses",
                post(move |Json(body): Json<Value>| {
                    let reply = reply.clone();
                    let seen = seen.clone();
                    async move {
                        seen.lock().unwrap().push(body);
                        Json(reply.lock().unwrap().clone())
                    }
                }),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let model = "gpt-5.1-2026-01-01";
        let mut config = responses_config(model);
        config.base_url = format!("http://{addr}/v1");
        let adapter = HttpAdapter::new(config).unwrap();
        let request = LlmRequest {
            system: "sys".into(),
            input: "in".into(),
            model: model.into(),
            seed: 7,
            timeout: Duration::from_secs(5),
        };
        *reply.lock().unwrap() =
            json!({"status": "completed", "model": model, "output_text": "## A"});
        assert_eq!(adapter.compile(&request).await.unwrap().text, "## A");
        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["instructions"], "sys");
        assert!(body.get("seed").is_none());
        *reply.lock().unwrap() = json!({
            "status": "incomplete", "model": model,
            "incomplete_details": {"reason": "max_output_tokens"}, "output_text": "## A"
        });
        let err = adapter.compile(&request).await.unwrap_err();
        assert!(err.contains("incomplete"), "{err}");
        *reply.lock().unwrap() = json!({
            "status": "failed", "model": model,
            "error": {"code": "server_error", "message": "boom"}
        });
        let err = adapter.compile(&request).await.unwrap_err();
        assert!(err.contains("server_error"), "{err}");
    }

    #[test]
    fn responses_body_and_parse() {
        let request = LlmRequest {
            system: "sys".into(),
            input: "in".into(),
            model: "gpt-5.1-2026-01-01".into(),
            seed: 0,
            timeout: Duration::from_secs(1),
        };
        let body = openai_responses_body(&request, &model_capabilities(OPENAI, "gpt-5.1", 0));
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["input"], "in");
        assert_eq!(body["reasoning"]["effort"], "none");
        assert!(body.get("messages").is_none());
        let parsed = parse_openai_responses(&json!({
            "model": "m",
            "output": [{"type": "reasoning"}, {"type": "message", "content": [
                {"type": "output_text", "text": "## A"}, {"type": "output_text", "text": "\n- k: v"}
            ]}]
        }))
        .unwrap();
        assert_eq!(parsed.text, "## A\n- k: v");
        assert_eq!(
            parse_openai_responses(&json!({"output_text": "x"}))
                .unwrap()
                .text,
            "x"
        );
        assert!(parse_openai_responses(&json!({"output": []})).is_err());
    }

    #[test]
    fn api_style_is_validated() {
        let env = |style: &str, provider: &str| {
            HashMap::from([
                ("LLM_PROVIDER".to_string(), provider.to_string()),
                ("LLM_API_KEY".to_string(), "sk-test-value".to_string()),
                ("LLM_MODEL".to_string(), "gpt-test-2026-01-01".to_string()),
                ("LLM_API_STYLE".to_string(), style.to_string()),
            ])
        };
        assert_eq!(
            load_llm_config(&env("responses", "openai"), false)
                .unwrap()
                .api_style,
            "responses"
        );
        assert!(load_llm_config(&env("bogus", "openai"), false).is_err());
        assert!(load_llm_config(&env("responses", "gemini"), false).is_err());
    }
}

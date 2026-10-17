use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::diag::{Diag, Report};

pub const OPENAI: &str = "openai";
const USER_AGENT: &str = concat!("vibe-sdk/", env!("CARGO_PKG_VERSION"));
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
    /// When false (`LLM_SEND_SAMPLING=0`), omit temperature/seed/top_* (#35).
    pub send_sampling: bool,
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
    /// Shared per-source / per-run cap on attempts and time (#83).
    /// `None` shares the process-wide environment-configured bound, including retries.
    pub budget: Option<std::sync::Arc<crate::llm_budget::CallBudget>>,
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

/// Which sampling parameters a registered model family accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SamplingCaps {
    temperature: bool,
    top_p: bool,
    top_k: bool,
    seed: bool,
    reasoning_none: bool,
}

/// Sampling parameters for one config and model, by API style. This is the
/// single map both the request body and the lock's `sent-params` stamp use.
/// The Responses API has no `seed`, so it is dropped for `responses`.
pub fn sent_params(config: &LlmConfig, model: &str) -> SentParams {
    let mut params =
        model_capabilities_with(&config.provider, model, config.seed, config.send_sampling);
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

const GEMINI_2: SamplingCaps = SamplingCaps {
    temperature: true,
    top_p: true,
    top_k: true,
    seed: true,
    reasoning_none: false,
};
const GPT_5_NONE: SamplingCaps = SamplingCaps {
    temperature: true,
    top_p: true,
    top_k: false,
    seed: true,
    reasoning_none: true,
};
const GPT_4: SamplingCaps = SamplingCaps {
    temperature: true,
    top_p: true,
    top_k: false,
    seed: true,
    reasoning_none: false,
};
const CLAUDE_TEMP: SamplingCaps = SamplingCaps {
    temperature: true,
    top_p: false,
    top_k: false,
    seed: false,
    reasoning_none: false,
};

/// Variants that share a registered prefix but reject sampling parameters
/// (or `reasoning_effort: none`).
const OPENAI_EXCLUDED: &[&str] = &[
    "-pro",
    "-codex",
    "-chat",
    "-search",
    "-audio",
    "-realtime",
    "-transcribe",
    "-tts",
];

/// Gemini variants that share a registered family but are not text models
/// (speech, image generation, Live API) and must not get sampling parameters.
const GEMINI_EXCLUDED: &[&str] = &["-tts", "-image", "-live", "-audio"];

/// What may follow a registered id.
#[derive(Clone, Copy)]
enum Suffix {
    /// Nothing, or `-` and any variant (`gemini-2.5-flash-lite`,
    /// `gpt-4o-mini-2024-07-18`), minus the excluded substrings.
    Variant,
    /// Nothing, `-YYYYMMDD`, `-YYYY-MM-DD` or `-latest` only
    /// (`claude-opus-4-20250514`, `claude-3-5-sonnet-latest`).
    Dated,
}

use Suffix::{Dated, Variant};

/// Explicit capability registry: `(provider, model id, allowed suffix,
/// excluded substrings, caps)`, shipped with this SDK version (spec 2A.8.6,
/// decision 58, #35). A model gets sampling parameters only when its
/// lower-cased name is a listed id, optionally followed by an allowed suffix
/// (there is always a `-` or the end of the name right after the id, so
/// `gpt-4.10`, `gpt-4oops` and `claude-opus-4-10` do not match), and contains
/// none of the excluded substrings. Unknown models and providers get none.
///
/// - Gemini 1.5 / 2.0 / 2.5 text models (not tts, image, live or audio
///   variants): temperature 0, topP 1, topK 1, seed. Gemini 3 and later are
///   not listed: Google recommends the default temperature 1.0 (lower values
///   can loop) and newer models deprecate sampling knobs, so they get none.
/// - OpenAI gpt-5.1 / gpt-5.2 (not -pro, -codex, -chat): temperature 0,
///   top_p 1, seed, `reasoning_effort: none`. gpt-4.1 / gpt-4o (not search,
///   audio, realtime, transcribe, tts): temperature 0, top_p 1, seed.
/// - Anthropic: `temperature: 0` only (never with top_p) for exactly these
///   models: Claude Opus 4, 4.1, 4.5, 4.6; Sonnet 4, 4.5, 4.6; Haiku 4.5;
///   Claude 3.7 Sonnet, 3.5 Sonnet, 3.5 Haiku, 3 Opus and 3 Haiku. Every
///   other Claude model (Opus 4.7 and later, Sonnet 5 and later, any newer
///   Haiku, Claude 2.x) is not listed and gets none.
const SAMPLING_REGISTRY: &[(&str, &str, Suffix, &[&str], SamplingCaps)] = &[
    (GEMINI, "gemini-1.5", Variant, GEMINI_EXCLUDED, GEMINI_2),
    (GEMINI, "gemini-2.0", Variant, GEMINI_EXCLUDED, GEMINI_2),
    (GEMINI, "gemini-2.5", Variant, GEMINI_EXCLUDED, GEMINI_2),
    (OPENAI, "gpt-5.1", Variant, OPENAI_EXCLUDED, GPT_5_NONE),
    (OPENAI, "gpt-5.2", Variant, OPENAI_EXCLUDED, GPT_5_NONE),
    (OPENAI, "gpt-4.1", Variant, OPENAI_EXCLUDED, GPT_4),
    (OPENAI, "gpt-4o", Variant, OPENAI_EXCLUDED, GPT_4),
    (ANTHROPIC, "claude-opus-4-6", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-opus-4-5", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-opus-4-1", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-opus-4-0", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-opus-4", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-sonnet-4-6", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-sonnet-4-5", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-sonnet-4-0", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-sonnet-4", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-haiku-4-5", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-3-7-sonnet", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-3-5-sonnet", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-3-5-haiku", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-3-opus", Dated, &[], CLAUDE_TEMP),
    (ANTHROPIC, "claude-3-haiku", Dated, &[], CLAUDE_TEMP),
];

/// `-YYYYMMDD`, `-YYYY-MM-DD` or `-latest`.
fn is_date_or_latest(rest: &str) -> bool {
    let Some(rest) = rest.strip_prefix('-') else {
        return false;
    };
    if rest == "latest" {
        return true;
    }
    let b = rest.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    match b.len() {
        8 => digits(0..8),
        10 => digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10),
        _ => false,
    }
}

// Known diagnostic values. Display matching deliberately never uses Variant:
// arbitrary variant suffixes can contain provider-controlled free text.
fn known_model(provider: &str, model: &str) -> String {
    const OTHER_MODELS: &[(&str, &str)] = &[
        (OPENAI, "gpt-5"),
        (OPENAI, "gpt-5-mini"),
        (OPENAI, "gpt-5-nano"),
        (OPENAI, "gpt-4.1-mini"),
        (OPENAI, "gpt-4.1-nano"),
        (OPENAI, "gpt-4o-mini"),
        (OPENAI, "o3"),
        (OPENAI, "o3-mini"),
        (OPENAI, "o4-mini"),
        (GEMINI, "gemini-2.5-pro"),
        (GEMINI, "gemini-2.5-flash"),
        (GEMINI, "gemini-2.5-flash-lite"),
        (GEMINI, "gemini-2.0-flash"),
        (GEMINI, "gemini-3-pro-preview"),
        (ANTHROPIC, "claude-opus-4-7"),
    ];
    let model = model.to_ascii_lowercase();
    if model.len() <= 64
        && model
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
        && (SAMPLING_REGISTRY.iter().any(|(p, id, _, _, _)| {
            *p == provider
                && model
                    .strip_prefix(id)
                    .is_some_and(|rest| rest.is_empty() || is_date_or_latest(rest))
        }) || OTHER_MODELS
            .iter()
            .any(|(p, id)| *p == provider && *id == model))
    {
        model
    } else {
        "unknown".into()
    }
}

// Documented OpenAI codes/types, Anthropic types and Gemini statuses only.
fn known_error_code(value: &Value) -> &'static str {
    const CODES: &[&str] = &[
        "invalid_api_key",
        "invalid_request_error",
        "authentication_error",
        "permission_error",
        "insufficient_quota",
        "rate_limit_exceeded",
        "model_not_found",
        "context_length_exceeded",
        "server_error",
        "unsupported_parameter",
        "unsupported_value",
        "invalid_value",
        "billing_hard_limit_reached",
        "tokens",
        "requests",
        "not_found_error",
        "api_error",
        "overloaded_error",
        "request_too_large",
        "rate_limit_error",
        "billing_error",
        "timeout_error",
        "INVALID_ARGUMENT",
        "FAILED_PRECONDITION",
        "PERMISSION_DENIED",
        "NOT_FOUND",
        "RESOURCE_EXHAUSTED",
        "INTERNAL",
        "UNAVAILABLE",
        "DEADLINE_EXCEEDED",
        "UNAUTHENTICATED",
        "CANCELLED",
    ];
    CODES
        .iter()
        .copied()
        .find(|code| Some(*code) == value.as_str())
        .unwrap_or("unknown")
}

fn error_code(value: &Value) -> Option<&Value> {
    let error = value.get("error")?;
    let fields = ["code", "type", "status"].map(|field| error.get(field));
    fields
        .iter()
        .flatten()
        .copied()
        .find(|value| value.is_string())
        .or_else(|| fields.into_iter().flatten().find(|value| !value.is_null()))
}

fn known_incomplete_reason(value: &Value) -> &'static str {
    match value.as_str() {
        Some("max_output_tokens") => "max_output_tokens",
        Some("content_filter") => "content_filter",
        _ => "unknown",
    }
}

fn id_matches(model: &str, id: &str, suffix: Suffix) -> bool {
    let Some(rest) = model.strip_prefix(id) else {
        return false;
    };
    match suffix {
        Variant => rest.is_empty() || rest.starts_with('-'),
        Dated => rest.is_empty() || is_date_or_latest(rest),
    }
}

fn registered_caps(provider: &str, model: &str) -> Option<SamplingCaps> {
    let model = model.to_ascii_lowercase();
    SAMPLING_REGISTRY
        .iter()
        .find(|(p, id, suffix, excluded, _)| {
            *p == provider
                && id_matches(&model, id, *suffix)
                && !excluded.iter().any(|x| model.contains(x))
        })
        .map(|(_, _, _, _, caps)| *caps)
}

/// Sampling parameters for a model with sampling enabled (the default).
pub fn model_capabilities(provider: &str, model: &str, seed: u64) -> SentParams {
    model_capabilities_with(provider, model, seed, true)
}

/// Sampling parameters to send. `send_sampling = false` (`LLM_SEND_SAMPLING=0`)
/// sends none even for registered models.
pub fn model_capabilities_with(
    provider: &str,
    model: &str,
    seed: u64,
    send_sampling: bool,
) -> SentParams {
    if !send_sampling {
        return SentParams::default();
    }
    let Some(caps) = registered_caps(provider, model) else {
        return SentParams::default();
    };
    SentParams {
        temperature: caps.temperature.then(|| "0".into()),
        top_p: caps.top_p.then(|| "1".into()),
        top_k: caps.top_k.then(|| "1".into()),
        seed: caps.seed.then(|| seed.to_string()),
        reasoning_effort: caps.reasoning_none.then(|| "none".into()),
    }
}

/// True when the model is in the capability registry.
pub fn sampling_registered(provider: &str, model: &str) -> bool {
    registered_caps(provider, model).is_some()
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
    /// True when `compile` reserves from `request.budget` per HTTP attempt
    /// itself. Other adapters are charged one call by `call_budgeted`.
    fn enforces_budget(&self) -> bool {
        false
    }
}

/// Calls without an explicit run budget share one process-wide budget.
fn default_budget() -> Result<std::sync::Arc<crate::llm_budget::CallBudget>, String> {
    static BUDGET: std::sync::OnceLock<
        Result<std::sync::Arc<crate::llm_budget::CallBudget>, String>,
    > = std::sync::OnceLock::new();
    BUDGET
        .get_or_init(|| crate::llm_budget::CallBudget::from_env().map(std::sync::Arc::new))
        .clone()
}

/// Every LLM call goes through here so the budget holds for any adapter.
pub async fn call_budgeted(
    adapter: &dyn LlmAdapter,
    request: &LlmRequest,
) -> Result<LlmResponse, String> {
    let mut bounded = request.clone();
    if bounded.budget.is_none() {
        bounded.budget = Some(default_budget()?);
    }
    let request = &bounded;
    if !adapter.enforces_budget() {
        if let Some(budget) = &request.budget {
            budget.reserve()?;
        }
    }
    let left = request.budget.as_ref().expect("default budget").remaining();
    tokio::time::timeout(left, adapter.compile(request))
        .await
        .map_err(|_| "LLM time budget spent".to_string())?
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
        if !model.is_empty() && !model_id_is_valid(&provider, model) {
            report.push(Diag::error(
                "env",
                "LLM_MODEL contains characters that are not allowed for this provider",
            ));
        }
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
    // Default on: pin temperature/seed for deterministic NL compiles (#35).
    let send_sampling = !matches!(
        env.get("LLM_SEND_SAMPLING")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    );
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
        send_sampling,
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

/// Only a conservative character set is accepted for model ids. Gemini
/// places the id in the request URL path, so ':' and '/' are rejected there;
/// other providers send it in the JSON body and may use them (for example
/// `qwen2.5-coder:32b` or `org/model`).
pub fn model_id_is_valid(provider: &str, model: &str) -> bool {
    let path_safe = provider == GEMINI;
    !model.is_empty()
        && model.len() <= 256
        && !model.starts_with('.')
        && !model.contains("..")
        && model.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'.' | b'_' | b'-')
                || (!path_safe && matches!(b, b':' | b'/'))
        })
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

/// LLM hosts a release build may send a key to. The default endpoint
/// (api.openai.com when LLM_BASE_URL is unset) is on the list.
pub const RELEASE_LLM_HOSTS: &[&str] = &[
    "api.openai.com",
    "api.anthropic.com",
    "generativelanguage.googleapis.com",
];

/// Release builds only talk to allowlisted hosts, over https.
pub fn check_release_endpoint(config: &LlmConfig) -> Result<(), String> {
    let host = config.endpoint_host.to_ascii_lowercase();
    if config.endpoint_private || !RELEASE_LLM_HOSTS.contains(&host.as_str()) {
        return Err(format!(
            "release builds only send the LLM key to an allowlisted host ({}); LLM_BASE_URL points at {}",
            RELEASE_LLM_HOSTS.join(", "),
            if host.is_empty() { "(none)" } else { &host }
        ));
    }
    Ok(())
}

pub fn default_base_url(provider: &str) -> &'static str {
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
    Ok((host, private || allow_private))
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
    let mut known: Vec<String> = std::env::vars()
        .filter(|(name, _)| {
            !matches!(name.as_str(), "PWD" | "OLDPWD")
                && [
                    "KEY",
                    "TOKEN",
                    "SECRET",
                    "PASSWORD",
                    "PASSWD",
                    "PWD",
                    "DATABASE_URL",
                ]
                .iter()
                .any(|part| name.to_ascii_uppercase().contains(part))
        })
        .map(|(_, value)| value)
        .filter(|v| !v.is_empty())
        .collect();
    known.extend(secrets.iter().map(|v| v.to_string()));
    let mut text = text.to_string();
    let mut ranges = Vec::new();
    static KEYS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(?:\b(?:[a-z0-9]+[_-])*(?:password|passwd|pwd|secret|token|api[ _-]?key|database_url|private_key|credentials?|passphrase|authorization|auth|set-cookie|cookie|session)(?:[_-][a-z0-9]+)*\b|密碼|密码|口令|憑證)[ 	"']*[:=][ 	]*|\bbearer[ 	]+"#).expect("redaction keys")
    });
    static TOKENS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"[a-zA-Z][a-zA-Z0-9+.-]*://[^\r\n"'<>]*@[^\s"'<>]*|\b(?:sk-|xai-|github_pat_|gh[pousr]_)[A-Za-z0-9_-]+|\b(?:AKIA|ASIA)[A-Z0-9]{16}\b|\bxox[abprs]-[A-Za-z0-9-]+|\beyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"#).expect("redaction tokens")
    });
    static URL_VALUE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"[a-zA-Z][a-zA-Z0-9+.-]*://[^/\r\n"'<>]*:[^\r\n"'<>]*"#)
            .expect("URL credentials")
    });
    static PEM: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"-----BEGIN ([A-Z0-9 ]*PRIVATE KEY)-----").expect("private key marker")
    });
    let mut view = text.as_bytes().to_vec();
    let mut spans: Vec<_> = (0..view.len()).map(|i| (i, i + 1)).collect();
    for secret in &known {
        if !secret.is_empty() {
            ranges.extend(
                text.match_indices(secret)
                    .map(|(at, value)| (at, at + value.len())),
            );
        }
    }
    for iteration in 0..=16 {
        let (readable, mapped) = readable_redaction_view(&view, &spans);
        for secret in &known {
            if !secret.is_empty() {
                for (start, value) in readable.match_indices(secret) {
                    ranges.push((mapped[start].0, mapped[start + value.len() - 1].1));
                }
            }
        }
        let mut cursor = 0;
        while let Some(found) = KEYS.find_at(&readable, cursor) {
            let key = readable[found.start()..found.end()]
                .trim_end_matches([' ', '\t', ':', '=', '\'', '"']);
            let value_text = readable[found.end()..].trim_start();
            let auth_setting = key.eq_ignore_ascii_case("auth")
                && [
                    "required", "optional", "enabled", "disabled", "true", "false",
                ]
                .iter()
                .any(|setting| {
                    value_text.strip_prefix(setting).is_some_and(|rest| {
                        rest.is_empty()
                            || rest.starts_with(|c: char| {
                                c.is_whitespace() || matches!(c, ';' | ',' | '}' | ']')
                            })
                    })
                });
            if matches!(key, "PWD" | "OLDPWD") || auth_setting {
                cursor = found.end();
                continue;
            }
            let start = mapped[found.start()].0;
            let value = mapped[found.end() - 1].1;
            let header =
                key.eq_ignore_ascii_case("cookie") || key.eq_ignore_ascii_case("set-cookie");
            let end = redaction_end(&text, start, value, header);
            ranges.push((start, end));
            cursor = found.end();
            while cursor < mapped.len() && mapped[cursor].0 < end {
                cursor += 1;
            }
        }
        for found in TOKENS.find_iter(&readable) {
            ranges.push((mapped[found.start()].0, mapped[found.end() - 1].1));
        }
        for found in URL_VALUE.find_iter(&readable) {
            if !readable[found.start()..found.end()].contains('@') {
                ranges.push((mapped[found.start()].0, mapped[found.end() - 1].1));
            }
        }
        let mut cursor = 0;
        while let Some(capture) = PEM.captures_at(&readable, cursor) {
            let found = capture.get(0).expect("marker");
            let closing = format!("-----END {}-----", &capture[1]);
            let end = readable[found.end()..]
                .find(&closing)
                .map_or(readable.len(), |at| found.end() + at + closing.len());
            ranges.push((mapped[found.start()].0, mapped[end - 1].1));
            cursor = end;
        }
        let (next, next_spans) = decode_redaction_view(readable.as_bytes(), &mapped);
        if next == view {
            break;
        }
        if iteration == 16 {
            ranges.push((0, text.len()));
            break;
        }
        view = next;
        spans = next_spans;
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    for (start, end) in merged.into_iter().rev() {
        text.replace_range(start..end, "[redacted]");
    }
    text
}

// Original offsets keep encoded delimiters from shortening a redaction.
fn redaction_end(text: &str, start: usize, value: usize, header: bool) -> usize {
    let line_end = text[value..].find('\n').map_or(text.len(), |n| value + n);
    let tail = &text[value..];
    if header {
        return line_end;
    }
    let first_line = text[value..line_end]
        .split_once('#')
        .map_or(&text[value..line_end], |(value, _)| value)
        .trim();
    {
        let line_start = text[..start].rfind('\n').map_or(0, |n| n + 1);
        let indent = text[line_start..]
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .count();
        let mut end = line_end;
        while end < text.len() {
            let next = end + 1;
            let next_end = text[next..].find('\n').map_or(text.len(), |n| next + n);
            let line = &text[next..next_end];
            if !line.trim().is_empty()
                && line.chars().take_while(|c| matches!(c, ' ' | '\t')).count() <= indent
            {
                break;
            }
            end = next_end;
        }
        if end > line_end || first_line.is_empty() || first_line.starts_with(['|', '>']) {
            return end;
        }
    }
    if tail.starts_with(['\\', '%', '&']) {
        return text.len();
    }
    let mut quote = None;
    let mut escaped = false;
    let mut entity_end = 0;
    for (index, ch) in tail.char_indices() {
        if index < entity_end {
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if let Some(open) = quote {
            if ch == open {
                // Repeated decoding can change which quote closes a value.
                if tail[..index].contains('\\') {
                    return text.len();
                }
                return value + index + ch.len_utf8();
            }
        } else if index == 0 && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch == '&' && html_entity_len(&tail[index..]).is_some() {
            entity_end = index + html_entity_len(&tail[index..]).expect("entity length");
        } else if ch == '\n' || matches!(ch, ';' | ',' | '}' | ']' | '&') {
            return value + index;
        }
    }
    text.len()
}

fn html_entity_len(text: &str) -> Option<usize> {
    let end = text.find(';')?;
    let entity = text.get(1..end)?;
    let valid = if let Some(number) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        !number.is_empty() && number.bytes().all(|b| b.is_ascii_hexdigit())
    } else if let Some(number) = entity.strip_prefix('#') {
        !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
    } else {
        entity.starts_with(|c: char| c.is_ascii_alphabetic())
            && entity.bytes().all(|b| b.is_ascii_alphanumeric())
    };
    valid.then_some(end + 1)
}

fn readable_redaction_view(
    bytes: &[u8],
    spans: &[(usize, usize)],
) -> (String, Vec<(usize, usize)>) {
    use unicode_normalization::UnicodeNormalization;
    let mut readable = String::new();
    let mut mapped = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let width = match bytes[cursor] {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        let width = width.min(bytes.len() - cursor);
        let length = if std::str::from_utf8(&bytes[cursor..cursor + width]).is_ok() {
            width
        } else {
            1
        };
        let decoded = String::from_utf8_lossy(&bytes[cursor..cursor + length]);
        let normalized: String = decoded.nfkc().collect();
        mapped.extend(std::iter::repeat_n(
            (spans[cursor].0, spans[cursor + length - 1].1),
            normalized.len(),
        ));
        readable.push_str(&normalized);
        cursor += length;
    }
    let normalized: String = readable.nfkc().collect();
    if normalized != readable {
        // Composition across characters uses a conservative original span.
        let span = (spans[0].0, spans[spans.len() - 1].1);
        mapped = vec![span; normalized.len()];
        readable = normalized;
    }
    (readable, mapped)
}

fn decode_redaction_view(bytes: &[u8], spans: &[(usize, usize)]) -> (Vec<u8>, Vec<(usize, usize)>) {
    let mut next = Vec::new();
    let mut mapped = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let mut replacement = None;
        if bytes[i] == b'&' {
            if let Some(end) = bytes[i..]
                .iter()
                .take(33)
                .position(|b| *b == b';')
                .filter(|end| *end <= 32)
            {
                let entity = std::str::from_utf8(&bytes[i + 1..i + end]).unwrap_or("");
                let ch = match entity {
                    "equals" => Some('='),
                    "colon" => Some(':'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "Tab" => Some('\t'),
                    "NewLine" => Some('\n'),
                    "nbsp" => Some(' '),
                    "lowbar" => Some('_'),
                    "sol" => Some('/'),
                    "bsol" => Some('\\'),
                    "semi" => Some(';'),
                    "percnt" => Some('%'),
                    "commat" => Some('@'),
                    "period" => Some('.'),
                    "num" => Some('#'),
                    "hyphen" => Some('-'),
                    _ => entity
                        .strip_prefix("#x")
                        .or_else(|| entity.strip_prefix("#X"))
                        .and_then(|n| u32::from_str_radix(n, 16).ok())
                        .or_else(|| entity.strip_prefix('#').and_then(|n| n.parse().ok()))
                        .and_then(char::from_u32),
                };
                replacement = ch.map(|ch| (ch.to_string().into_bytes(), end + 1));
            }
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                replacement = Some((vec![(a * 16 + b) as u8], 3));
            }
        } else if bytes[i] == b'\\' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'x' && i + 3 < bytes.len() {
                if let Ok(hex) = std::str::from_utf8(&bytes[i + 2..i + 4]) {
                    replacement = u8::from_str_radix(hex, 16).ok().map(|ch| (vec![ch], 4));
                }
            } else if bytes[i + 1] == b'u' && i + 5 < bytes.len() {
                if let Ok(hex) = std::str::from_utf8(&bytes[i + 2..i + 6]) {
                    if let Ok(code) = u32::from_str_radix(hex, 16) {
                        let mut scalar = code;
                        let mut consumed = 6;
                        if (0xd800..=0xdbff).contains(&code)
                            && bytes.get(i + 6..i + 8) == Some(b"\\u")
                        {
                            if let Some(low) = bytes
                                .get(i + 8..i + 12)
                                .and_then(|hex| std::str::from_utf8(hex).ok())
                                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                                .filter(|low| (0xdc00..=0xdfff).contains(low))
                            {
                                scalar = 0x10000 + ((code - 0xd800) << 10) + low - 0xdc00;
                                consumed = 12;
                            }
                        }
                        if let Some(ch) = char::from_u32(scalar) {
                            replacement = Some((ch.to_string().into_bytes(), consumed));
                        }
                    }
                }
            } else {
                let ch = match bytes[i + 1] {
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'b' => 8,
                    b'f' => 12,
                    other => other,
                };
                replacement = Some((vec![ch], 2));
            }
        }
        if let Some((decoded, consumed)) = replacement {
            mapped.extend(std::iter::repeat_n(
                (spans[i].0, spans[i + consumed - 1].1),
                decoded.len(),
            ));
            next.extend(decoded);
            i += consumed;
        } else {
            next.push(bytes[i]);
            mapped.push(spans[i]);
            i += 1;
        }
    }
    (next, mapped)
}

pub struct HttpAdapter {
    pub config: LlmConfig,
    client: Client,
}

/// Masks API keys and common secret query/fragment params. Shared by
/// `describe_failure` and transport-error paths so secrets are never printed.
pub fn mask_secrets(text: &str, api_key: &str) -> String {
    let mut out = text.to_string();
    if !api_key.is_empty() {
        out = out.replace(api_key, "***");
    }
    // Query, fragment, and `;`-separated params: key / api_key / token / …
    static KEY_PARAM: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)([?&#;](?:key|api[ _-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|private[_-]?key|token|secret|password|auth)=)[^&\s#;]*",
        )
        .expect("regex")
    });
    KEY_PARAM.replace_all(&out, "${1}***").into_owned()
}

/// Formats transport causes before masking secrets in the complete message.
fn error_chain(error: &(dyn std::error::Error + 'static), api_key: &str) -> String {
    let mut parts = Vec::new();
    let mut current = Some(error);
    while let Some(cause) = current {
        let text = cause.to_string();
        if !parts.contains(&text) {
            parts.push(text);
        }
        current = cause.source();
    }
    let mut message = parts.join(": ");
    if message.contains("resolves to non-public address") {
        message.push_str("; if this host is a local proxy (for example a fake-IP DNS setup), set LLM_ALLOW_PRIVATE_HOST=true for local development; it is refused in CI and production");
    }
    mask_secrets(&message, api_key)
}

/// Explains a failed or non-JSON LLM response. JSON or empty 401/403 responses blame the key;
/// HTML (or a 404) hints at the base URL. Only the masked URL and response
/// classification and allowlisted error codes are included; response body text is never included.
pub fn describe_failure(
    url: &str,
    status: u16,
    content_type: &str,
    body: &[u8],
    api_key: &str,
) -> String {
    let shown_url = mask_secrets(url, api_key);
    let raw = String::from_utf8_lossy(body);
    let html = content_type.to_ascii_lowercase().contains("text/html")
        || raw
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<!doctype html")
        || raw.trim_start().to_ascii_lowercase().starts_with("<html");
    let lead = match status {
        401 | 403 if body.is_empty() || serde_json::from_slice::<Value>(body).is_ok() => {
            format!("LLM_API_KEY was rejected (http {status})")
        }
        401 | 403 => format!("LLM endpoint or a proxy in front of it refused the request (http {status}); the response is not JSON, so this is probably not an API key problem"),
        404 => format!("LLM endpoint not found (http {status})"),
        429 => format!("LLM rate limit or quota exceeded (http {status})"),
        400 => format!("LLM request rejected (http {status}); check LLM_MODEL and the provider"),
        s if s >= 500 => format!("LLM provider error (http {s})"),
        s if (200..300).contains(&s) => format!("LLM response was not JSON (http {s})"),
        s => format!("LLM request failed (http {s})"),
    };
    let hint = if html || status == 404 {
        "; the response looks like a web page, not an API: check LLM_BASE_URL (OpenAI-compatible endpoints usually end in /v1)"
    } else {
        ""
    };
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| error_code(&value).map(known_error_code))
        .map(|code| format!("; provider error code: {code}"))
        .unwrap_or_default();
    mask_secrets(&format!("{lead}{hint}. URL: {shown_url}{code}"), api_key)
}

impl HttpAdapter {
    pub fn new(config: LlmConfig) -> Result<Self, String> {
        let mut builder = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(config.timeout)
            // A redirect could lead to a host the endpoint check never saw.
            .redirect(reqwest::redirect::Policy::none());
        if !config.endpoint_private {
            // A public endpoint must not resolve to a private address (DNS
            // rebinding); every resolved address is checked before connecting.
            builder = crate::netguard::guard_client(builder);
        }
        let client = builder.build().map_err(|err| {
            mask_secrets(
                &format!("http client: {}", error_chain(&err, &config.api_key)),
                &config.api_key,
            )
        })?;
        Ok(Self { config, client })
    }
}

#[async_trait]
impl LlmAdapter for HttpAdapter {
    fn enforces_budget(&self) -> bool {
        true
    }

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
        let mut bounded = request.clone();
        if bounded.budget.is_none() {
            bounded.budget = Some(default_budget()?);
        }
        let request = &bounded;
        let mut attempt = 0u32;
        let max = self.config.max_retries;
        let started = std::time::Instant::now();
        let deadline = retry_deadline();
        loop {
            // Every attempt reserves from the shared budget and may not run
            // past the retry deadline or the budget's time left.
            let mut window = deadline.saturating_sub(started.elapsed());
            if let Some(budget) = &request.budget {
                match budget.reserve() {
                    Ok(left) => window = window.min(left),
                    Err(why) => return Err(why),
                }
            }
            if window.is_zero() {
                return Err(format!(
                    "LLM retry deadline {}s reached",
                    deadline.as_secs()
                ));
            }
            let attempt_result =
                match tokio::time::timeout(window, self.compile_once(request)).await {
                    Ok(result) => result,
                    Err(_) => Err(CallError {
                        message: format!(
                            "LLM call stopped after {}ms: deadline or budget reached",
                            window.as_millis()
                        ),
                        retryable: false,
                        retry_after: None,
                    }),
                };
            match attempt_result {
                Ok(response) => return Ok(response),
                Err(err) if err.retryable && attempt < max => {
                    // 408/429/5xx/network/timeouts: honor Retry-After, else
                    // exponential backoff with jitter (#83); never past the
                    // overall deadline or the budget.
                    let delay = err.retry_after.unwrap_or_else(|| retry_delay(attempt));
                    let budget_left = request
                        .budget
                        .as_ref()
                        .map(|b| b.remaining())
                        .unwrap_or(Duration::MAX);
                    if started.elapsed() + delay >= deadline || delay >= budget_left {
                        return Err(redact(
                            &format!(
                                "{} (gave up: retry deadline {}s or LLM budget reached)",
                                err.message,
                                deadline.as_secs()
                            ),
                            &[&self.config.api_key],
                        ));
                    }
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(err) => {
                    return Err(redact(&err.message, &[&self.config.api_key]));
                }
            }
        }
    }
}

/// Delay before retry `attempt` (0-based): 1s, 2s, 4s, ... capped at 16s,
/// plus up to 25% jitter so parallel runs do not retry in lockstep.
pub fn retry_delay(attempt: u32) -> std::time::Duration {
    let base_ms = 1_000u64.saturating_mul(1 << attempt.min(4));
    let jitter = (uuid::Uuid::new_v4().as_u128() % (base_ms as u128 / 4 + 1)) as u64;
    std::time::Duration::from_millis(base_ms + jitter)
}

struct CallError {
    message: String,
    retryable: bool,
    retry_after: Option<std::time::Duration>,
}

/// Total time one LLM call may spend retrying (`LLM_RETRY_DEADLINE`
/// seconds, default 120, max 600).
pub fn retry_deadline() -> std::time::Duration {
    let secs = std::env::var("LLM_RETRY_DEADLINE")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|n| n.min(600))
        .unwrap_or(120);
    std::time::Duration::from_secs(secs)
}

/// `Retry-After` in delta-seconds (HTTP-dates are ignored), capped at 60s.
pub fn parse_retry_after(value: &str) -> Option<std::time::Duration> {
    let secs: u64 = value.trim().parse().ok()?;
    Some(std::time::Duration::from_secs(secs.min(60)))
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
            message: mask_secrets(
                &format!(
                    "LLM request failed: {}",
                    error_chain(&err, &self.config.api_key)
                ),
                &self.config.api_key,
            ),
            retryable: true,
            retry_after: None,
        })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response.bytes().await.map_err(|err| CallError {
            message: mask_secrets(
                &format!(
                    "LLM response body: {}",
                    error_chain(&err, &self.config.api_key)
                ),
                &self.config.api_key,
            ),
            retryable: true,
            retry_after: None,
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
                retryable: status.is_server_error() || matches!(status.as_u16(), 408 | 429),
                retry_after,
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
            retry_after: None,
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
            retry_after: None,
        })?;
        if let Some(reported) = &parsed.model_reported {
            if reported != &request.model && !reported.is_empty() {
                return Err(CallError {
                    message: mask_secrets(
                        &format!(
                            "provider reported an unexpected model (expected {}, got {})",
                            known_model(&self.config.provider, &request.model),
                            known_model(&self.config.provider, reported),
                        ),
                        &self.config.api_key,
                    ),
                    retryable: false,
                    retry_after: None,
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
    let model = encode_path_segment(&request.model);
    let url = join_url(&config.base_url, &format!("models/{model}:generateContent"));
    let params = sent_params(config, &request.model);
    let body = gemini_body(request, &params);
    let headers = vec![("x-goog-api-key", config.api_key.clone())];
    (url, body, headers)
}

/// Percent-encode a single URL path segment (RFC 3986 unreserved left as-is).
fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
        }
    }
    out
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

/// Like [`parse_openai_responses`]; diagnostics never include provider body text.
pub fn parse_openai_responses_masked(value: &Value, api_key: &str) -> Result<LlmResponse, String> {
    let status = value["status"].as_str().unwrap_or("");
    if status == "incomplete" {
        return Err(mask_secrets(
            &format!(
                "openai responses output is incomplete (reason: {})",
                known_incomplete_reason(&value["incomplete_details"]["reason"])
            ),
            api_key,
        ));
    }
    if matches!(status, "failed" | "error") || !value["error"].is_null() {
        let code = error_code(value)
            .map(|code| format!(" (code: {})", known_error_code(code)))
            .unwrap_or_default();
        return Err(mask_secrets(
            &format!("openai responses returned an error{code}"),
            api_key,
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

/// Names only, including empty and non-Unicode values.
pub fn runtime_llm_env_names(
    keys: impl IntoIterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = keys
        .into_iter()
        .filter(|key| {
            let key = key.to_string_lossy();
            key.starts_with("LLM_") || key.starts_with("OPENAI_")
        })
        .collect();
    names.sort();
    names
}

/// Remove compile-time credentials before building runtime state or spawning tasks.
pub fn strip_runtime_llm_env() -> Vec<std::ffi::OsString> {
    let names = runtime_llm_env_names(std::env::vars_os().map(|(key, _)| key));
    for name in &names {
        std::env::remove_var(name);
    }
    names
}

pub fn runtime_llm_env_present() -> Option<String> {
    runtime_llm_env_names(std::env::vars_os().map(|(key, _)| key))
        .first()
        .map(|key| key.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_value_after_comment_only_line() {
        for first in ["", "# comment", "first", "\"first\""] {
            let input = format!("  password: {first}\n    second\n    third\n  message: retained");
            assert_eq!(redact(&input, &[]), "  [redacted]\n  message: retained");
        }
    }

    #[test]
    fn html_entity_inside_value() {
        for entity in ["&amp;", "&#61;", "&#x3d;", "&copy;", "&semi;"] {
            let input = format!("password=abc{entity}def&message=retained");
            assert_eq!(redact(&input, &[]), "[redacted]&message=retained");
        }
        assert_eq!(
            redact("password=abc&message=retained", &[]),
            "[redacted]&message=retained"
        );
    }

    #[test]
    fn key_word_boundaries() {
        for input in [
            "author: name",
            "max_tokens: 100",
            "auth: required",
            "auth: optional",
            "tokenizer: value",
        ] {
            assert_eq!(redact(input, &[]), input);
        }
        for key in [
            "auth_token",
            "x-auth",
            "api_key",
            "APP_DATABASE_URL",
            "password",
            "authorization",
        ] {
            assert_eq!(redact(&format!("{key}: value"), &[]), "[redacted]");
        }
    }

    #[test]
    fn redact_fullwidth_colon() {
        assert_eq!(redact("password：value", &[]), "[redacted]");
    }

    #[test]
    fn redact_fullwidth_equals() {
        assert_eq!(redact("password＝value", &[]), "[redacted]");
    }

    #[test]
    fn redact_html_separator() {
        assert_eq!(redact("password&#61;value", &[]), "[redacted]");
    }

    #[test]
    fn redact_hex_separator() {
        assert_eq!(redact("password\\x3dvalue", &[]), "[redacted]");
    }

    #[test]
    fn redact_yaml_block_scalar() {
        assert_eq!(redact("password: |-\n  first\n  second", &[]), "[redacted]");
    }

    #[test]
    fn redact_multiline_quote() {
        assert_eq!(redact("password: \"first\nsecond\"", &[]), "[redacted]");
    }

    #[test]
    fn redact_encoded_known_secret() {
        assert_eq!(redact("value%2Dhidden", &["value-hidden"]), "[redacted]");
    }

    #[test]
    fn redact_url_userinfo_variant() {
        assert_eq!(redact("https://user:pa ss@host", &[]), "[redacted]");
    }

    #[test]
    fn redact_pem_private_key() {
        assert_eq!(
            redact(
                "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----",
                &[]
            ),
            "[redacted]"
        );
    }

    #[test]
    fn redact_cookie_header() {
        assert_eq!(redact("Cookie: a=value; b=other", &[]), "[redacted]");
    }

    #[test]
    fn redact_json_auth() {
        assert_eq!(redact("{\"auth\":\"value\"}", &[]), "{\"[redacted]}");
    }

    #[test]
    fn redact_credentials_setting() {
        assert_eq!(redact("credentials=value", &[]), "[redacted]");
    }

    #[test]
    fn redact_chinese_password() {
        assert_eq!(redact("密碼：value", &[]), "[redacted]");
    }

    #[test]
    fn redact_aws_access_key() {
        assert_eq!(redact("AKIA1234567890ABCDEF", &[]), "[redacted]");
    }

    #[test]
    fn redact_github_pat() {
        assert_eq!(redact("github_pat_abcdef123456", &[]), "[redacted]");
    }

    #[test]
    fn redact_slack_token() {
        assert_eq!(redact("xoxb-123456-abcdef", &[]), "[redacted]");
    }

    #[test]
    fn redact_compact_token_format() {
        assert_eq!(
            redact("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.signature", &[]),
            "[redacted]"
        );
    }

    #[test]
    fn redact_pwd_path() {
        assert_eq!(
            redact("PWD=/tmp/project\nOLDPWD=/tmp/other", &[]),
            "PWD=/tmp/project\nOLDPWD=/tmp/other"
        );
    }

    #[test]
    fn redact_line_after_multiline_value() {
        assert_eq!(
            redact("password: >\n  first\n  second\nmessage: retained", &[]),
            "[redacted]\nmessage: retained"
        );
    }

    #[test]
    fn redact_json_escaped_opening_quote() {
        assert_eq!(
            redact(r#"{"password": \"a\"b tail\"}"#, &[]),
            r#"{"[redacted]"#
        );
    }

    #[test]
    fn redact_nested_json_escaped_opening_quote() {
        let original = r#"{"password": \"a\"b tail\"}"#;
        let nested = serde_json::to_string(original).unwrap();
        let output = redact(&nested, &[]);
        assert!(output.ends_with("[redacted]"), "{output}");
        assert!(
            !output.contains("tail") && !output.contains("b"),
            "{output}"
        );
    }

    #[test]
    fn redact_invalid_percent_byte_with_encoded_separator() {
        assert_eq!(redact("%FF password%3Dsecret", &[]), "%FF [redacted]");
    }

    #[test]
    fn redact_many_percent_encoding_layers() {
        assert_eq!(redact("password%252525253Dsecret", &[]), "[redacted]");
    }

    #[test]
    fn redact_nested_json_password() {
        assert_eq!(
            redact(r#"{"outer":{"password":"secret"}}"#, &[]),
            r#"{"outer":{"[redacted]}}"#
        );
    }

    #[test]
    fn redact_percent_encoding_iteration_limit() {
        let encoded = format!("password%{}3Dsecret; remainder", "25".repeat(20));
        assert_eq!(redact(&encoded, &[]), "[redacted]");
    }

    #[test]
    fn redact_preserves_other_lines() {
        assert_eq!(
            redact("plain text\npassword=secret\nmore text", &[]),
            "plain text\n[redacted]\nmore text"
        );
    }

    #[test]
    fn redact_multilevel_escaped_quoted_values_including_tail() {
        for raw in [
            r"password='a\\'b tail'",
            r"password='a\\\\'b tail'",
            r"database_url='a\\'b tail'",
            r#"password="a\\"b tail""#,
        ] {
            assert_eq!(redact(raw, &[]), "[redacted]", "{raw}");
        }
    }

    #[test]
    fn redact_iteratively_percent_encoded_settings_in_original_text() {
        for raw in [
            "password%3Dsecret",
            "pwd%3Dsecret%26x%3D1",
            "password%253Dsecret",
            "p%2561ssword%253Dsecret",
            "password%25253Dsecret",
        ] {
            assert_eq!(
                redact(&format!("before {raw}; after"), &[]),
                "before [redacted]; after",
                "{raw}"
            );
        }
    }

    #[test]
    fn redact_password_with_escaped_single_quote() {
        assert_eq!(redact(r"password='a\'b'", &[]), "[redacted]");
    }

    #[test]
    fn redact_password_with_escaped_double_quote_and_space() {
        assert_eq!(redact(r#"password="a\"b c""#, &[]), "[redacted]");
    }

    #[test]
    fn redact_password_with_escaped_space() {
        assert_eq!(redact(r"password=a\ b", &[]), "[redacted]");
    }

    #[test]
    fn redact_dsn_with_percent_encoded_password() {
        assert_eq!(
            redact("postgres://user:a%40b%3Ac@host/db", &[]),
            "[redacted]"
        );
    }

    #[test]
    fn redact_fully_url_encoded_connection_string() {
        assert_eq!(
            redact("postgres%3A%2F%2Fuser%3Apass%40host", &[]),
            "[redacted]"
        );
    }

    #[test]
    fn redact_dsn_with_escaped_space() {
        assert_eq!(redact(r"postgres://user:a\ b@host/db", &[]), "[redacted]");
    }

    #[test]
    fn redact_connection_strings_and_settings() {
        for scheme in [
            "postgres",
            "postgresql",
            "mysql",
            "mongodb+srv",
            "redis",
            "amqp",
        ] {
            let raw = format!("before {scheme}://user:credential@host/db after");
            let text = redact(&raw, &[]);
            assert!(!text.contains("credential"), "{scheme}: {text}");
            assert!(text.contains("before") && text.contains("after"));
        }
        for raw in [
            "Server=host;Password=ado-value;Database=db;",
            "Server=host;Pwd=ado-value;Database=db;",
            "Password=\"ado;value\";",
            "DATABASE_URL=private-connection",
            r#"{"DATABASE_URL": "private-connection"}"#,
            "APP_DATABASE_URL='private-connection'",
        ] {
            let text = redact(raw, &[]);
            assert!(!text.contains("ado-value"), "{text}");
            assert!(!text.contains("ado;value"), "{text}");
            assert!(!text.contains("private-connection"), "{text}");
            assert!(text.contains("[redacted]"), "{text}");
        }
        assert_eq!(redact("short abc", &["abc"]), "short [redacted]");
    }

    #[tokio::test]
    async fn default_budget_is_shared_and_configured() {
        if std::env::var_os("VIBE_TEST_DEFAULT_BUDGET").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "llm::tests::default_budget_is_shared_and_configured",
                ])
                .env("VIBE_TEST_DEFAULT_BUDGET", "1")
                .env("LLM_MAX_CALLS_PER_SOURCE", "2")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let adapter = FakeAdapter {
            responder: |_: &LlmRequest| Ok("done".into()),
        };
        let request = LlmRequest {
            system: String::new(),
            input: String::new(),
            model: "test".into(),
            seed: 0,
            timeout: Duration::from_secs(1),
            budget: None,
        };
        assert!(call_budgeted(&adapter, &request).await.is_ok());
        assert!(call_budgeted(&adapter, &request).await.is_ok());
        assert!(call_budgeted(&adapter, &request)
            .await
            .unwrap_err()
            .contains("2 calls"));
    }

    #[tokio::test]
    async fn explicit_budget_is_enforced() {
        let adapter = FakeAdapter {
            responder: |request: &LlmRequest| {
                let budget = request.budget.as_ref().expect("default bound");
                let mut available = 0;
                while budget.reserve().is_ok() {
                    available += 1;
                }
                assert!(available < crate::llm_budget::Limits::default().source_calls);
                Ok("done".into())
            },
        };
        let request = LlmRequest {
            system: String::new(),
            input: String::new(),
            model: "test".into(),
            seed: 0,
            timeout: Duration::from_secs(1),
            budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                crate::llm_budget::Limits::default(),
            ))),
        };
        assert!(call_budgeted(&adapter, &request).await.is_ok());
    }

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
    fn model_id_charset() {
        for ok in ["gpt-4o-2024-08-06", "gemini-2.5-pro-001", "a_b.c-1"] {
            assert!(model_id_is_valid(GEMINI, ok), "{ok}");
            assert!(model_id_is_valid(OPENAI, ok), "{ok}");
        }
        for bad in [
            "", "../x", "a/b", "a:b", "a?b", "a%2Fb", "a b", ".x", "a..b", "a#b",
        ] {
            assert!(!model_id_is_valid(GEMINI, bad), "{bad}");
        }
        // #131 negative cases: backslash, controls, non-ASCII, '@', over 256 bytes
        for bad in [
            "a\\b",
            "a\nb",
            "a\0b",
            "a\tb",
            "caf\u{e9}",
            "\u{4e2d}",
            "a@b",
        ] {
            assert!(!model_id_is_valid(GEMINI, bad), "{bad:?}");
            assert!(!model_id_is_valid(OPENAI, bad), "{bad:?}");
        }
        let over_256 = "a".repeat(257);
        assert!(!model_id_is_valid(GEMINI, &over_256));
        assert!(!model_id_is_valid(OPENAI, &over_256));
        assert!(model_id_is_valid(GEMINI, &"a".repeat(256)));
        assert!(model_id_is_valid(OPENAI, &"a".repeat(256)));
        assert!(model_id_is_valid(OPENAI, "qwen2.5-coder:32b-instruct-q8_0"));
        assert!(!model_id_is_valid(OPENAI, "a?b"));
        let report = load_llm_config(
            &env(&[
                ("LLM_PROVIDER", "gemini"),
                ("LLM_API_KEY", "test-value"),
                ("LLM_MODEL", "gemini-2.5-pro-001:x"),
            ]),
            false,
        );
        assert!(report.is_err());
    }

    #[test]
    fn gemini_model_is_percent_encoded_in_path() {
        let config = LlmConfig {
            provider: GEMINI.into(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            api_key: "test-value".into(),
            model: "unused".into(),
            timeout: Duration::from_secs(5),
            max_retries: 0,
            seed: 0,
            endpoint_host: "generativelanguage.googleapis.com".into(),
            endpoint_private: false,
            send_sampling: true,
            api_style: "chat".into(),
        };
        for (model, encoded) in [
            ("a/b", "a%2Fb"),
            ("a?b", "a%3Fb"),
            ("a#b", "a%23b"),
            ("a%b", "a%25b"),
            ("../x", "..%2Fx"),
            ("a..b", "a..b"),
            ("a b", "a%20b"),
        ] {
            let request = LlmRequest {
                system: "s".into(),
                input: "i".into(),
                model: model.into(),
                seed: 0,
                timeout: Duration::from_secs(5),
                budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                    crate::llm_budget::Limits::default(),
                ))),
            };
            let (url, _, _) = gemini_call(&config, &request);
            let expected = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{encoded}:generateContent"
            );
            assert_eq!(url, expected, "{model}");
            let parsed = url::Url::parse(&url).unwrap();
            assert!(parsed.query().is_none(), "{model} must not inject a query");
            assert!(
                parsed.fragment().is_none(),
                "{model} must not inject a fragment"
            );
            let segs: Vec<_> = parsed.path_segments().unwrap().collect();
            assert_eq!(
                segs,
                ["v1beta", "models", &format!("{encoded}:generateContent")],
                "{model}"
            );
        }
        let request = LlmRequest {
            system: "s".into(),
            input: "i".into(),
            model: "gemini-2.5-pro-001".into(),
            seed: 0,
            timeout: Duration::from_secs(5),
            budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                crate::llm_budget::Limits::default(),
            ))),
        };
        let (url, _, _) = gemini_call(&config, &request);
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro-001:generateContent"
        );
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
            budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                crate::llm_budget::Limits::default(),
            ))),
        };
        let unknown = model_capabilities(OPENAI, &request.model, 0);
        assert!(
            unknown.is_empty(),
            "unregistered models get no sampling params"
        );
        let openai = openai_body(&request, &unknown);
        assert!(openai.get("temperature").is_none());
        assert!(openai.get("seed").is_none());
        assert!(openai.get("tools").is_none());
        assert!(openai.get("functions").is_none());
        assert!(openai.get("tool_choice").is_none());
        let off = model_capabilities_with(OPENAI, &request.model, 0, false);
        assert!(off.is_empty());
        let pinned = model_capabilities(OPENAI, "gpt-5.1", 0);
        let openai_pinned = openai_body(&request, &pinned);
        assert_eq!(openai_pinned["temperature"], 0);
        assert_eq!(openai_pinned["top_p"], 1);
        assert_eq!(openai_pinned["seed"], 0);
        assert_eq!(openai_pinned["reasoning_effort"], "none");
        let anthropic = anthropic_body(
            &request,
            &model_capabilities(ANTHROPIC, "claude-sonnet-4-6", 0),
        );
        assert_eq!(anthropic["temperature"], 0);
        assert!(anthropic.get("top_p").is_none(), "anthropic: not both");
        assert!(anthropic.get("tools").is_none());
        let gemini = gemini_body(&request, &model_capabilities(GEMINI, "gemini-2.5-pro", 0));
        assert_eq!(gemini["generationConfig"]["temperature"], 0);
        assert_eq!(gemini["generationConfig"]["topK"], 1);
        assert!(gemini.get("tools").is_none());
        assert!(gemini.get("toolConfig").is_none());
    }

    #[test]
    fn only_registered_models_get_sampling() {
        for (provider, model) in [
            (OPENAI, "gpt-5.5-2026-09-01"),
            (OPENAI, "gpt-5-2025-08-07"),
            (OPENAI, "o3-2025-04-16"),
            (OPENAI, "qwen2.5-coder:32b"),
            (GEMINI, "vendor-model"),
            (GEMINI, "gemini-3-pro-preview"),
            (GEMINI, "gemini-3.5-flash"),
            ("mystery", "gpt-4o-2024-08-06"),
            (OPENAI, "gpt-5.2-pro-2025-12-11"),
            (OPENAI, "gpt-5.1-codex"),
            (OPENAI, "gpt-5.1-codex-mini"),
            (OPENAI, "gpt-5.1-chat-latest"),
            (OPENAI, "gpt-4o-search-preview-2025-03-11"),
            (OPENAI, "gpt-4o-mini-search-preview"),
            (OPENAI, "gpt-4o-audio-preview"),
            (OPENAI, "gpt-4o-realtime-preview"),
            (ANTHROPIC, "claude-opus-4-7"),
            (ANTHROPIC, "claude-opus-4-7-20260416"),
            (ANTHROPIC, "claude-opus-4-8"),
            (ANTHROPIC, "claude-opus-5"),
            (ANTHROPIC, "claude-opus-5-5"),
            (ANTHROPIC, "claude-sonnet-5"),
            (ANTHROPIC, "claude-sonnet-5-5"),
            (ANTHROPIC, "claude-haiku-5-5"),
            (ANTHROPIC, "claude-fable-5-1"),
            // No version boundary after the registered id.
            (OPENAI, "gpt-5.10"),
            (OPENAI, "gpt-5.10-2026-01-01"),
            (OPENAI, "gpt-4.10"),
            (OPENAI, "gpt-4oops"),
            (OPENAI, "gpt-5.25"),
            (ANTHROPIC, "claude-opus-4-10"),
            (ANTHROPIC, "claude-opus-4-10-20270101"),
            (ANTHROPIC, "claude-sonnet-4-50"),
            (ANTHROPIC, "claude-haiku-4-55"),
            (ANTHROPIC, "claude-opus-4-7-latest"),
            (ANTHROPIC, "claude-opus-4-20250514-extra"),
            (ANTHROPIC, "claude-opus-4-2025051"),
            (ANTHROPIC, "claude-2.1"),
            (ANTHROPIC, "claude-2-1"),
            (GEMINI, "gemini-2.50-pro"),
            // Gemini speech, image and Live variants.
            (GEMINI, "gemini-2.5-flash-preview-tts"),
            (GEMINI, "gemini-2.5-pro-preview-tts"),
            (GEMINI, "gemini-2.5-flash-image"),
            (GEMINI, "gemini-2.5-flash-image-preview"),
            (GEMINI, "gemini-2.0-flash-preview-image-generation"),
            (GEMINI, "gemini-2.0-flash-exp-image-generation"),
            (GEMINI, "gemini-2.0-flash-live-001"),
            (GEMINI, "gemini-live-2.5-flash-preview"),
            (GEMINI, "gemini-2.5-flash-live-preview"),
            (GEMINI, "gemini-2.5-flash-native-audio-preview-09-2025"),
        ] {
            assert!(
                model_capabilities(provider, model, 1).is_empty(),
                "{provider} {model}"
            );
            assert!(!sampling_registered(provider, model), "{provider} {model}");
        }
        for (provider, model) in [
            (OPENAI, "gpt-5.1-2025-11-13"),
            (OPENAI, "GPT-4o-2024-08-06"),
            (ANTHROPIC, "claude-opus-4-6"),
            (ANTHROPIC, "claude-sonnet-4-5-20250929"),
            (ANTHROPIC, "claude-haiku-4-5-20251001"),
            (OPENAI, "gpt-4.1-mini-2025-04-14"),
            (OPENAI, "gpt-5.2-2025-12-11"),
            (GEMINI, "gemini-2.5-pro"),
            (GEMINI, "gemini-2.5-flash-lite"),
            (GEMINI, "gemini-2.0-flash-001"),
            (GEMINI, "gemini-1.5-pro-002"),
            (OPENAI, "gpt-5.1"),
            (OPENAI, "gpt-4o"),
            (OPENAI, "gpt-4o-mini"),
            (OPENAI, "gpt-4.1"),
            (ANTHROPIC, "claude-opus-4-1"),
            (ANTHROPIC, "claude-opus-4-1-20250805"),
            (ANTHROPIC, "claude-opus-4-20250514"),
            (ANTHROPIC, "claude-sonnet-4-20250514"),
            (ANTHROPIC, "claude-sonnet-4-5-2025-09-29"),
            (ANTHROPIC, "claude-sonnet-4-0"),
            (ANTHROPIC, "claude-3-5-sonnet-latest"),
            (ANTHROPIC, "claude-3-7-sonnet-20250219"),
            (ANTHROPIC, "claude-3-opus-20240229"),
            (ANTHROPIC, "claude-3-haiku-20240307"),
            (ANTHROPIC, "claude-3-5-haiku-latest"),
        ] {
            assert!(sampling_registered(provider, model), "{provider} {model}");
            let p = model_capabilities(provider, model, 1);
            assert_eq!(p.temperature.as_deref(), Some("0"), "{provider} {model}");
            assert!(model_capabilities_with(provider, model, 1, false).is_empty());
        }
        let claude = model_capabilities(ANTHROPIC, "claude-opus-4-6", 1);
        assert!(claude.top_p.is_none() && claude.seed.is_none());
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
        assert!(!m.contains(&filler), "{m}");
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
            &json!({"status": "failed", "error": {"code": "bad", "message": &long}}),
            key,
        )
        .unwrap_err();
        assert!(!err.contains("sk-test-secret"), "{err}");
        assert!(!err.contains("sk-test"), "partial key leaked: {err}");
        assert!(!err.contains(&long), "{err}");
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
                post(move |headers: axum::http::HeaderMap| {
                    assert_eq!(
                        headers.get("user-agent").unwrap(),
                        concat!("vibe-sdk/", env!("CARGO_PKG_VERSION"))
                    );
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
                budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                    crate::llm_budget::Limits::default(),
                ))),
            };
            let err = adapter.compile(&request).await.unwrap_err();
            assert!(err.contains(&status.to_string()), "{status}: {err}");
            assert!(!err.contains(key), "{status}: {err}");
            let _ = retry;
        }
    }

    #[tokio::test]
    async fn all_provider_failures_omit_headers_and_body() {
        use axum::{http::StatusCode, routing::post, Router};
        let key = "sk-private-test-key";
        let marker = "BODY-MARKER-7f3a";
        for (provider, style, code_field) in [
            (OPENAI, "chat", "code"),
            (OPENAI, "responses", "code"),
            (ANTHROPIC, "chat", "type"),
            (GEMINI, "chat", "status"),
        ] {
            let structured = json!({"type": "error", "error": {
                code_field: marker,
                "message": format!("{marker} Authorization: Bearer {key} x-api-key: {key} x-goog-api-key: {key}")
            }}).to_string();
            for (status, body) in [
                (400, structured.clone()),
                (200, structured),
                (401, format!(r#"{{"error":"{marker} Bearer {key}"}}"#)),
                (403, format!("<html>{marker} Bearer {key}</html>")),
                (500, format!("{marker} Bearer {key}")),
                (200, format!("{marker} malformed Bearer {key}")),
                (
                    200,
                    format!(r#"{{"error":{{"code":"{marker}","message":"{key}"}}}}"#),
                ),
            ] {
                let app = Router::new().fallback(post(move || {
                    let body = body.clone();
                    async move { (StatusCode::from_u16(status).unwrap(), body) }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
                let mut config = responses_config("test-model");
                config.provider = provider.into();
                config.api_style = style.into();
                config.api_key = key.into();
                config.base_url = format!("http://{addr}/v1");
                config.endpoint_private = true;
                let adapter = HttpAdapter::new(config).unwrap();
                let request = LlmRequest {
                    system: "sys".into(),
                    input: "in".into(),
                    model: "test-model".into(),
                    seed: 0,
                    timeout: Duration::from_secs(2),
                    budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                        crate::llm_budget::Limits::default(),
                    ))),
                };
                let err = adapter.compile(&request).await.unwrap_err();
                for secret in [
                    key,
                    "Bearer",
                    "Authorization",
                    "x-api-key",
                    "x-goog-api-key",
                    marker,
                ] {
                    assert!(!err.contains(secret), "{err}");
                }
                server.abort();
            }
        }
    }

    #[tokio::test]
    async fn model_mismatch_displays_only_known_models() {
        use axum::{routing::post, Router};
        for (reported, expected) in [
            ("gpt-4o", "expected gpt-4.1, got gpt-4o"),
            ("gpt-4o-SECRETMARKER", "expected gpt-4.1, got unknown"),
        ] {
            let body = json!({"status": "completed", "model": reported, "output_text": "ok"});
            let app = Router::new().route(
                "/v1/responses",
                post(move || {
                    let body = body.clone();
                    async move { axum::Json(body) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut config = responses_config("gpt-4.1");
            config.base_url = format!("http://{addr}/v1");
            let adapter = HttpAdapter::new(config).unwrap();
            let request = LlmRequest {
                system: "s".into(),
                input: "i".into(),
                model: "gpt-4.1".into(),
                seed: 7,
                timeout: Duration::from_secs(5),
                budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                    crate::llm_budget::Limits::default(),
                ))),
            };
            let message = adapter.compile(&request).await.unwrap_err();
            server.abort();
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("SECRETMARKER"), "{message}");
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
            send_sampling: true,
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
                budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                    crate::llm_budget::Limits::default(),
                ))),
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
                err.contains("returned an error") && !err.contains("boom"),
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
            budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                crate::llm_budget::Limits::default(),
            ))),
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
        assert!(err.contains("returned an error"), "{err}");
    }

    #[test]
    fn responses_body_and_parse() {
        let request = LlmRequest {
            system: "sys".into(),
            input: "in".into(),
            model: "gpt-5.1-2026-01-01".into(),
            seed: 0,
            timeout: Duration::from_secs(1),
            budget: Some(std::sync::Arc::new(crate::llm_budget::CallBudget::new(
                crate::llm_budget::Limits::default(),
            ))),
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

#[cfg(test)]
mod transport_policy_tests {
    use super::*;

    #[test]
    fn user_agent_identifies_sdk_version() {
        assert_eq!(
            USER_AGENT,
            format!("vibe-sdk/{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[tokio::test]
    async fn reqwest_dns_error_exposes_the_refusal_chain() {
        let resolver = crate::netguard::PublicOnlyResolver::with_lookup(|_| {
            Box::pin(async { Ok(vec!["198.18.0.5:0".parse().unwrap()]) })
        });
        let client = crate::netguard::guard_client_with(Client::builder(), resolver)
            .build()
            .unwrap();
        let error = client
            .post("https://proxy.example.com/v1?key=test-key")
            .header("Authorization", "Bearer test-key")
            .header("x-api-key", "test-key")
            .header("x-goog-api-key", "test-key")
            .body("BODY-MARKER-7f3a")
            .send()
            .await
            .unwrap_err();
        let message = error_chain(&error, "test-key");
        assert!(message.contains("198.18.0.5"), "{message}");
        assert!(message.contains("198.18.0.0/15"), "{message}");
        assert!(
            message.contains("; if this host is a local proxy (for example a fake-IP DNS setup), set LLM_ALLOW_PRIVATE_HOST=true for local development; it is refused in CI and production"),
            "{message}"
        );
        assert!(!message.contains("test-key"));
        assert!(!message.contains("Bearer test-key"));
        assert!(!message.contains("BODY-MARKER-7f3a"));
    }

    #[test]
    fn error_chain_includes_causes_and_masks_keys() {
        #[derive(Debug)]
        struct Cause(&'static str, Option<Box<Cause>>);
        impl std::fmt::Display for Cause {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for Cause {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1.as_deref().map(|e| e as _)
            }
        }
        let cause = Cause(
            "request failed",
            Some(Box::new(Cause(
                "request failed",
                Some(Box::new(Cause("DNS failed sk-secret", None))),
            ))),
        );
        assert_eq!(
            error_chain(&cause, "sk-secret"),
            "request failed: DNS failed ***"
        );
    }

    #[test]
    fn private_opt_in_marks_public_names_and_remains_local_only() {
        for restricted in [false, true] {
            let env = HashMap::from([
                ("LLM_API_KEY".into(), "test-key".into()),
                ("LLM_MODEL".into(), "test-model".into()),
                ("LLM_BASE_URL".into(), "https://proxy.example.com/v1".into()),
                ("LLM_ALLOW_PRIVATE_HOST".into(), "true".into()),
            ]);
            let config = load_llm_config(&env, restricted);
            if restricted {
                assert!(config.is_err());
            } else {
                assert!(config.unwrap().endpoint_private);
            }
        }
        assert!(
            !classify_endpoint("https://proxy.example.com", false, false)
                .unwrap()
                .1
        );
        assert!(classify_endpoint("http://proxy.example.com", true, false).is_err());
    }

    #[test]
    fn auth_errors_distinguish_non_json_responses() {
        for body in [b"{}".as_slice(), b""] {
            let message = describe_failure("https://api.example.com", 403, "", body, "test-key");
            assert!(message.contains("LLM_API_KEY was rejected"));
        }
        let message = describe_failure(
            "https://api.example.com",
            403,
            "text/html",
            b"<html>test-key</html>",
            "test-key",
        );
        assert!(message.contains("probably not an API key problem"));
        assert!(message.contains("LLM_BASE_URL"));
        assert!(!message.contains("test-key"));
        assert!(!message.contains("Bearer test-key"));
        assert!(!message.contains("BODY-MARKER-7f3a"));
    }
}

#[cfg(test)]
mod known_value_tests {
    use super::*;

    #[test]
    fn models_use_strict_display_allowlist() {
        for (provider, model) in [
            (OPENAI, "GPT-4.1"),
            (OPENAI, "gpt-4.1-2026-01-01"),
            (ANTHROPIC, "claude-sonnet-4-6-latest"),
            (GEMINI, "gemini-2.5-pro"),
        ] {
            assert_eq!(known_model(provider, model), model.to_ascii_lowercase());
        }
        for model in [
            "gpt-4.1-SECRETMARKER",
            "gpt-4.1-hello world",
            "gpt-4.1-ä",
            &"a".repeat(65),
        ] {
            assert_eq!(known_model(OPENAI, model), "unknown");
        }
        assert_eq!(known_model(GEMINI, "gpt-4.1"), "unknown");
    }

    #[test]
    fn provider_codes_are_known_or_unknown_without_body_text() {
        let key = "sk-private-test-key";
        let marker = "BODY-SECRETMARKER";
        // OpenAI chat and Responses, Anthropic and Gemini error envelopes.
        for (field, code) in [
            ("code", "invalid_api_key"),
            ("code", "server_error"),
            ("type", "authentication_error"),
            ("status", "PERMISSION_DENIED"),
        ] {
            for value in [
                json!(code),
                json!(marker),
                json!("custom_code_xyz"),
                json!(42),
                Value::Null,
            ] {
                let body = json!({"type": "error", "error": {field: value,
                    "message": format!("{marker} Authorization: Bearer {key} x-api-key: private-header x-goog-api-key: private-google")}});
                let expected = if value == json!(code) {
                    code
                } else {
                    "unknown"
                };
                let message = describe_failure(
                    "https://example.com/v1",
                    400,
                    "application/json",
                    &serde_json::to_vec(&body).unwrap(),
                    key,
                );
                let response_message = parse_openai_responses_masked(&body, key).unwrap_err();
                if value.is_null() {
                    assert!(!message.contains("provider error code"), "{message}");
                    assert!(!response_message.contains("code:"), "{response_message}");
                } else {
                    assert!(
                        message.contains(&format!("provider error code: {expected}")),
                        "{message}"
                    );
                    assert!(response_message.contains(&format!("code: {expected}")));
                }
                for message in [message, response_message] {
                    for forbidden in [
                        marker,
                        key,
                        "Bearer",
                        "Authorization",
                        "x-api-key",
                        "x-goog-api-key",
                        "private-header",
                        "private-google",
                        "custom_code_xyz",
                    ] {
                        assert!(!message.contains(forbidden), "{message}");
                    }
                }
            }
        }
        for (body, expected) in [
            (
                json!({"error": {"code": 429, "message": "quota exceeded", "status": "RESOURCE_EXHAUSTED"}}),
                "RESOURCE_EXHAUSTED",
            ),
            (
                json!({"error": {"code": null, "type": "insufficient_quota", "message": "quota exceeded"}}),
                "insufficient_quota",
            ),
        ] {
            assert_eq!(error_code(&body).map(known_error_code), Some(expected));
            let message = describe_failure(
                "https://example.com",
                429,
                "application/json",
                &serde_json::to_vec(&body).unwrap(),
                key,
            );
            assert!(
                message.contains(&format!("provider error code: {expected}")),
                "{message}"
            );
            let message = parse_openai_responses_masked(&body, key).unwrap_err();
            assert!(message.contains(&format!("code: {expected}")), "{message}");
        }
        let fallback = json!({"error": {"type": "rate_limit_error"}});
        assert_eq!(
            error_code(&fallback).map(known_error_code),
            Some("rate_limit_error")
        );
        assert!(
            !describe_failure("https://example.com", 400, "", b"{}", key)
                .contains("provider error code")
        );
        assert_eq!(known_error_code(&json!("Server_Error")), "unknown");
    }

    #[test]
    fn incomplete_reasons_and_final_masking() {
        for (reason, expected) in [
            ("max_output_tokens", "max_output_tokens"),
            ("content_filter", "content_filter"),
            ("SECRETMARKER", "unknown"),
        ] {
            let body = json!({"status": "incomplete", "incomplete_details": {"reason": reason}});
            let message = parse_openai_responses_masked(&body, "test-key").unwrap_err();
            assert_eq!(
                message,
                format!("openai responses output is incomplete (reason: {expected})")
            );
        }
        let body = json!({"status": "failed", "error": {"code": "server_error"}});
        assert!(!parse_openai_responses_masked(&body, "server_error")
            .unwrap_err()
            .contains("server_error"));
    }
}

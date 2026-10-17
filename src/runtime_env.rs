use std::collections::HashMap;

/// Merge dotenv lines without replacing existing values. Empty LLM/OpenAI
/// settings are missing; other keys retain first-occurrence semantics.
pub fn merge_dotenv(text: &str, env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut merged = env.clone();
    merged.retain(|key, value| !is_llm_setting(key) || !value.trim().is_empty());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if is_llm_setting(key) && value.trim().is_empty() {
                continue;
            }
            merged
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    merged
}

fn is_llm_setting(key: &str) -> bool {
    key.starts_with("LLM_") || key.starts_with("OPENAI_")
}

/// Collect process environment entries, skipping keys or values that are not valid UTF-8.
pub fn env_map_from_process() -> HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
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

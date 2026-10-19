//! Reviewed, network-free responses (decision 189).
use crate::llm::{Capabilities, LlmAdapter, LlmConfig, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Lowercase SHA-256 of VIBE_VERSION, decimal LOCK_VERSION, system, input,
/// each encoded as an eight-byte big-endian UTF-8 byte length followed by bytes.
/// Connection, model, sampling and timing settings are deliberately excluded.
pub fn key(request: &LlmRequest) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for value in [
        crate::lock::VIBE_VERSION,
        &crate::lock::LOCK_VERSION.to_string(),
        &request.system,
        &request.input,
    ] {
        crate::hashutil::hash_length_prefixed(&mut hash, value);
    }
    hex::encode(hash.finalize())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    key: String,
    source: String,
    purpose: String,
}
fn index(dir: &Path) -> Result<BTreeMap<String, Entry>, String> {
    let path = dir.join("index.json");
    match fs::read_to_string(&path) {
        Ok(text) => {
            let entries: Vec<Entry> =
                serde_json::from_str(&text).map_err(|_| format!("invalid {}", path.display()))?;
            if entries.iter().any(|e| !valid_key(&e.key)) {
                return Err(format!("invalid fixture key in {}", path.display()));
            }
            Ok(entries.into_iter().map(|e| (e.key.clone(), e)).collect())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(_) => Err(format!("cannot read {}", path.display())),
    }
}
fn valid_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn settings(root: &Path) -> Result<HashMap<String, String>, String> {
    let text = fs::read_to_string(root.join(".env")).unwrap_or_default();
    let mut env = crate::runtime_env::merge_dotenv(&text, &crate::llm::env_map_from_process());
    crate::llm::resolve_api_key_file(&mut env)?;
    Ok(env)
}
fn dir(root: &Path, env: &HashMap<String, String>) -> PathBuf {
    root.join(
        env.get("VIBE_LLM_FIXTURES")
            .map(String::as_str)
            .unwrap_or("fixtures/llm"),
    )
}
#[derive(Default)]
struct Secrets {
    key: String,
    base: String,
    host: String,
}
impl Secrets {
    fn from_env(env: &HashMap<String, String>) -> Self {
        let base = env
            .get("LLM_BASE_URL")
            .or_else(|| env.get("OPENAI_BASE_URL"))
            .cloned()
            .unwrap_or_default();
        let host = url::Url::parse(&base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        Self {
            key: env
                .get("LLM_API_KEY")
                .or_else(|| env.get("OPENAI_API_KEY"))
                .cloned()
                .unwrap_or_default(),
            base,
            host,
        }
    }
    fn scan(&self, file: &Path, text: &str) -> Result<(), String> {
        for (kind, value, insensitive) in [
            ("API key", &self.key, false),
            ("base URL", &self.base, false),
            ("endpoint host", &self.host, true),
        ] {
            if !value.is_empty()
                && if insensitive {
                    text.to_lowercase().contains(&value.to_lowercase())
                } else {
                    text.contains(value)
                }
            {
                return Err(format!(
                    "{}: fixture contains configured {kind}",
                    file.display()
                ));
            }
        }
        Ok(())
    }
    fn directory(&self, dir: &Path) -> Result<(), String> {
        if !dir.exists() {
            return Ok(());
        }
        for item in fs::read_dir(dir).map_err(|_| "cannot read fixtures directory")? {
            let path = item.map_err(|_| "cannot read fixture entry")?.path();
            if path.is_dir() {
                self.directory(&path)?;
            } else {
                let text = fs::read_to_string(&path)
                    .map_err(|_| format!("cannot read {}", path.display()))?;
                self.scan(&path, &text)?;
            }
        }
        Ok(())
    }
}
// Match decision 185's CI truthiness, including the explicit CI run environment.
fn is_ci(env: &HashMap<String, String>) -> bool {
    env.get("CI")
        .is_some_and(|s| !s.is_empty() && s != "false" && s != "0")
        || env.get("GITHUB_ACTIONS").is_some_and(|s| s == "true")
        || matches!(
            crate::runtime::RunEnv::from_var(env.get("VIBE_ENV").map(String::as_str), true),
            crate::runtime::RunEnv::Ci
        )
}
pub fn scan_project(root: &Path) -> Result<(), String> {
    let text = fs::read_to_string(root.join(".env")).unwrap_or_default();
    let env = crate::runtime_env::merge_dotenv(&text, &crate::llm::env_map_from_process());
    if env.get("VIBE_LLM_RECORD").is_some_and(|s| !s.is_empty()) && is_ci(&env) {
        return Err("VIBE_LLM_RECORD is refused in CI".into());
    }
    let fixtures = dir(root, &env);
    if !fixtures.exists() {
        return Ok(());
    }
    Secrets::from_env(&settings(root)?).directory(&fixtures)
}

pub(crate) fn wrap(config: &LlmConfig, root: &Path) -> Result<Box<dyn LlmAdapter>, String> {
    let env = settings(root)?;
    let record = env.get("VIBE_LLM_RECORD").filter(|s| !s.is_empty());
    if record.is_some() && is_ci(&env) {
        return Err("VIBE_LLM_RECORD is refused in CI".into());
    }
    if config.provider != "replay" && record.is_none() {
        return crate::llm::raw_adapter(config);
    }
    if config.provider == "replay" && record.is_some() {
        return Err("VIBE_LLM_RECORD requires a real or fake provider".into());
    }
    let dir = record
        .map(|p| root.join(p))
        .unwrap_or_else(|| dir(root, &env));
    let mut secrets = Secrets::from_env(&env);
    if config.provider != "fake" && config.provider != "replay" {
        secrets = Secrets {
            key: config.api_key.clone(),
            base: config.base_url.clone(),
            host: config.endpoint_host.clone(),
        };
    }
    secrets.directory(&dir)?;
    let entries = index(&dir)?;
    Ok(Box::new(Fixtures {
        inner: if config.provider == "replay" {
            None
        } else {
            Some(crate::llm::raw_adapter(config)?)
        },
        dir,
        secrets,
        state: Mutex::new((entries, BTreeSet::new())),
    }))
}
struct Fixtures {
    inner: Option<Box<dyn LlmAdapter>>,
    dir: PathBuf,
    secrets: Secrets,
    state: Mutex<(BTreeMap<String, Entry>, BTreeSet<String>)>,
}
#[async_trait]
impl LlmAdapter for Fixtures {
    fn provider(&self) -> &'static str {
        self.inner.as_ref().map_or("replay", |a| a.provider())
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.as_ref().map_or(
            Capabilities {
                seed: true,
                tools: false,
            },
            |a| a.capabilities(),
        )
    }
    fn enforces_budget(&self) -> bool {
        self.inner.as_ref().is_some_and(|a| a.enforces_budget())
    }
    async fn compile(&self, request: &LlmRequest) -> Result<LlmResponse, String> {
        let key = key(request);
        let path = self.dir.join(format!("{key}.md"));
        if let Some(inner) = &self.inner {
            let response = inner.compile(request).await?;
            let mut state = self.state.lock().map_err(|_| "fixture lock poisoned")?;
            let mut entries = state.0.clone();
            entries.insert(
                key.clone(),
                Entry {
                    key,
                    source: request.source.clone(),
                    purpose: request.purpose.clone(),
                },
            );
            let json = format!(
                "{}\n",
                serde_json::to_string_pretty(&entries.values().collect::<Vec<_>>())
                    .map_err(|_| "cannot serialize fixtures index")?
            );
            // Validate both payloads before creating even the directory.
            self.secrets
                .scan(Path::new(&request.source), &response.text)?;
            self.secrets.scan(Path::new(&request.source), &json)?;
            fs::create_dir_all(&self.dir).map_err(|_| "cannot create fixtures directory")?;
            fs::write(&path, &response.text).map_err(|_| "cannot write fixture")?;
            fs::write(self.dir.join("index.json"), json)
                .map_err(|_| "cannot write fixture index")?;
            state.0 = entries;
            Ok(response)
        } else {
            self.state
                .lock()
                .map_err(|_| "fixture lock poisoned")?
                .1
                .insert(key.clone());
            let text = fs::read_to_string(&path).map_err(|_| format!("replay miss: {} ({}) key {key}; record with: VIBE_LLM_RECORD=fixtures/llm LLM_PROVIDER=<provider> vibe build",request.source,request.purpose))?;
            self.secrets.scan(&path, &text)?;
            Ok(LlmResponse {
                text,
                model_reported: None,
                system_fingerprint: None,
            })
        }
    }
}
impl Drop for Fixtures {
    fn drop(&mut self) {
        if self.inner.is_some() {
            return;
        }
        if let Ok(state) = self.state.lock() {
            let mut entries = state.0.clone();
            if let Ok(files) = fs::read_dir(&self.dir) {
                for file in files.flatten() {
                    if let Some(key) = file.path().file_stem().and_then(|s| s.to_str()) {
                        if valid_key(key) {
                            entries.entry(key.into()).or_insert(Entry {
                                key: key.into(),
                                source: "unknown".into(),
                                purpose: "unknown".into(),
                            });
                        }
                    }
                }
            }
            for (key, entry) in entries {
                if !state.1.contains(&key) {
                    eprintln!(
                        "warning: unused LLM fixture {key} {} {}",
                        entry.source, entry.purpose
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn prewrite_scan_all_kinds_and_index() {
        for (secrets, text, kind, source) in [
            (
                Secrets {
                    key: "private-token".into(),
                    ..Default::default()
                },
                "private-token",
                "API key",
                "source.md",
            ),
            (
                Secrets {
                    base: "https://fixture.invalid/v1".into(),
                    ..Default::default()
                },
                "https://fixture.invalid/v1",
                "base URL",
                "source.md",
            ),
            (
                Secrets {
                    host: "fixture.invalid".into(),
                    ..Default::default()
                },
                "FIXTURE.INVALID",
                "endpoint host",
                "source.md",
            ),
            (
                Secrets {
                    key: "private-token".into(),
                    ..Default::default()
                },
                "safe response",
                "API key",
                "index-source.md",
            ),
        ] {
            let dir = std::env::temp_dir().join(format!("vibe-prewrite-{}", uuid::Uuid::new_v4()));
            let adapter = Fixtures {
                inner: Some(Box::new(crate::llm::FakeAdapter {
                    responder: move |_: &LlmRequest| Ok(text.into()),
                })),
                dir: dir.clone(),
                secrets,
                state: Mutex::new((BTreeMap::new(), BTreeSet::new())),
            };
            let request = LlmRequest {
                source: source.into(),
                purpose: if source == "index-source.md" {
                    "private-token".into()
                } else {
                    "compile".into()
                },
                system: "system".into(),
                input: "input".into(),
                model: "fake".into(),
                seed: 0,
                timeout: std::time::Duration::from_secs(1),
                budget: None,
            };
            let error = adapter.compile(&request).await.unwrap_err();
            assert!(error.contains(kind) && error.contains(source));
            assert!(!error.contains("private-token"));
            if source == "source.md" {
                assert!(!error.contains(text));
            }
            assert!(!dir.exists());
        }
    }
}

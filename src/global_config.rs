//! Development-only global LLM settings. Both files must be private (0600).
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
const KEYS: &[&str] = &[
    "LLM_PROVIDER",
    "LLM_BASE_URL",
    "LLM_MODEL",
    "LLM_API_KEY_FILE",
    "LLM_API_STYLE",
];
pub fn config_dir(env: &HashMap<String, String>) -> Result<PathBuf, String> {
    if let Some(path) = env
        .get("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(path.join("vibe"));
    }
    env.get("HOME")
        .filter(|s| !s.is_empty())
        .map(|p| Path::new(p).join(".config/vibe"))
        .ok_or_else(|| "HOME is required for global LLM configuration".into())
}
pub fn enabled(env: &HashMap<String, String>, release: bool) -> bool {
    !release
        && !env
            .get("CI")
            .is_some_and(|s| !s.is_empty() && s != "false" && s != "0")
        && env.get("GITHUB_ACTIONS").is_none_or(|s| s != "true")
        && crate::runtime::RunEnv::from_var(env.get("VIBE_ENV").map(String::as_str), true).is_dev()
}
fn read_private(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|e| format!("cannot open {}: {:?}", path.display(), e.kind()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(format!(
                "unsafe permissions on {}; run chmod 600 {}",
                path.display(),
                path.display()
            ));
        }
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|_| format!("cannot read {}", path.display()))?;
    Ok(text)
}
/// Resolve settings without consulting process state. Debug lines contain names and sources only.
pub fn load(
    env: &HashMap<String, String>,
    project: &str,
    dir: &Path,
    release: bool,
) -> Result<(HashMap<String, String>, Vec<String>), String> {
    let mut merged = crate::runtime_env::merge_dotenv(project, env);
    let higher = merged.clone();
    if enabled(&merged, release) {
        let path = dir.join("llm.env");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
            Ok(_) => {
                let text = read_private(&path)?;
                let mut global = HashMap::new();
                for line in text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                {
                    let (key, value) = line.split_once('=').ok_or("invalid global LLM setting")?;
                    if matches!(key, "LLM_API_KEY" | "OPENAI_API_KEY") {
                        return Err(format!(
                            "{key} must be in a separate file referenced by LLM_API_KEY_FILE"
                        ));
                    }
                    if !KEYS.contains(&key) {
                        return Err(format!("unknown global LLM key: {key}"));
                    }
                    if !value.trim().is_empty() {
                        global.entry(key.to_string()).or_insert(value.to_string());
                    }
                }
                if !["LLM_API_KEY", "LLM_API_KEY_FILE", "OPENAI_API_KEY"]
                    .iter()
                    .any(|k| higher.get(*k).is_some_and(|v| !v.trim().is_empty()))
                {
                    if let Some(key_path) = global.get_mut("LLM_API_KEY_FILE") {
                        let path = dir.join(&*key_path);
                        let key = read_private(&path)?;
                        if key.trim().is_empty() {
                            return Err(format!("empty key file {}", path.display()));
                        }
                        *key_path = path.to_string_lossy().into_owned();
                        global.insert("LLM_API_KEY".into(), key.trim().into());
                    }
                } else {
                    global.remove("LLM_API_KEY_FILE");
                }
                for (k, v) in global {
                    merged.entry(k).or_insert(v);
                }
            }
        }
    }
    let mut debug: Vec<_> = merged
        .keys()
        .filter(|k| k.starts_with("LLM_"))
        .map(|k| {
            let source = if env.get(k).is_some_and(|v| !v.trim().is_empty()) {
                "env"
            } else if higher.contains_key(k) {
                "project"
            } else {
                "global"
            };
            format!("{k}: {source}")
        })
        .collect();
    debug.sort();
    Ok((merged, debug))
}
pub fn load_process(project: &Path) -> Result<(), String> {
    let env = crate::runtime_env::env_map_from_process();
    let dir = config_dir(&env).ok();
    let text = fs::read_to_string(project.join(".env")).unwrap_or_default();
    let (merged, debug) = load(
        &env,
        &text,
        dir.as_deref().unwrap_or(Path::new("")),
        dir.is_none(),
    )?;
    if env.get("VIBE_DEBUG_LLM_CONFIG").is_some_and(|v| v == "1") {
        for line in debug {
            eprintln!("{line}");
        }
    }
    for (k, v) in merged {
        std::env::set_var(k, v);
    }
    Ok(())
}
pub fn write_initial(
    dir: &Path,
    values: &HashMap<String, String>,
    key: &str,
) -> Result<(PathBuf, PathBuf), String> {
    if !dir.is_absolute() {
        return Err("global configuration directory must be absolute".into());
    }
    if key.contains(['\n', '\r'])
        || values
            .iter()
            .any(|(k, v)| !KEYS.contains(&k.as_str()) || v.contains(['\n', '\r']))
    {
        return Err("invalid global LLM settings".into());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| e.to_string())?;
    let config_path = dir.join("llm.env");
    let key_path = dir.join("llm-api-key");
    if config_path.exists() || key_path.exists() {
        return Err("global LLM files already exist; refusing to overwrite".into());
    }
    let create = |path: &Path, text: &str| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|_| format!("cannot write {}", path.display()))
    };
    create(&key_path, key)?;
    let mut lines: Vec<_> = values
        .iter()
        .filter(|(k, _)| k.as_str() != "LLM_API_KEY_FILE")
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect();
    lines.sort();
    lines.push(format!("LLM_API_KEY_FILE={}\n", key_path.display()));
    create(&config_path, &lines.concat())?;
    Ok((config_path, key_path))
}

//! `vibe serve` refuses unsafe configurations before loading anything.

use std::process::Command;

fn serve(envs: &[(&str, &str)], memory: bool) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("vibe-serve-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.arg("serve")
        .arg("--project")
        .arg(&dir)
        .arg("--manifest")
        .arg(dir.join("missing.json"))
        .arg("--addr")
        .arg("127.0.0.1:0");
    if memory {
        cmd.arg("--memory");
    }
    for key in [
        "VIBE_ENV",
        "VIBE_API_KEY_PEPPER",
        "VIBE_DATABASE_URL",
        "DATABASE_URL",
        "LLM_API_KEY",
        "LLM_MODEL",
        "LLM_PROVIDER",
        "LLM_BASE_URL",
        "OPENAI_API_KEY",
        "VIBE_TURNSTILE_SECRET",
        "VIBE_CHALLENGE_HOSTNAMES",
        "VIBE_CHALLENGE_PROVIDER",
        "VIBE_CHALLENGE_REPLAY_STORE",
    ] {
        cmd.env_remove(key);
    }
    cmd.current_dir(&dir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn memory_is_refused_outside_development() {
    for env in [
        "production",
        "Production",
        "PROD",
        "live",
        "Live",
        "ci",
        "CI",
        "staging",
    ] {
        let (ok, err) = serve(&[("VIBE_ENV", env)], true);
        assert!(!ok, "{env}");
        assert!(
            err.contains("--memory is a development executor"),
            "{env}: {err}"
        );
    }
}

#[test]
fn real_database_without_pepper_is_refused_even_when_env_unset() {
    for envs in [
        vec![],
        vec![("VIBE_ENV", "Production")],
        vec![("VIBE_ENV", "prod")],
    ] {
        let (ok, err) = serve(&envs, false);
        assert!(!ok);
        assert!(
            err.contains("VIBE_API_KEY_PEPPER is required"),
            "{envs:?}: {err}"
        );
    }
    let (ok, err) = serve(
        &[("VIBE_ENV", "live"), ("VIBE_API_KEY_PEPPER", "short")],
        false,
    );
    assert!(!ok);
    assert!(err.contains("at least 32 bytes"), "{err}");
}

const TURNSTILE: [(&str, &str); 2] = [
    ("VIBE_TURNSTILE_SECRET", "secret"),
    ("VIBE_CHALLENGE_HOSTNAMES", "shop.example.com"),
];

#[test]
fn production_refuses_the_memory_replay_store() {
    for with_verifier in [true, false] {
        let mut envs = vec![
            ("VIBE_ENV", "production"),
            ("VIBE_CHALLENGE_REPLAY_STORE", "memory"),
        ];
        if with_verifier {
            envs.extend(TURNSTILE);
        }
        let (ok, err) = serve(&envs, false);
        assert!(!ok);
        assert!(err.contains("refused in production"), "{envs:?}: {err}");
    }
}

#[test]
fn development_needs_an_explicit_replay_store() {
    let mut envs = vec![("VIBE_ENV", "dev")];
    envs.extend(TURNSTILE);
    let (ok, err) = serve(&envs, true);
    assert!(!ok);
    assert!(err.contains("needs an explicit replay store"), "{err}");
    // With the explicit switch, startup gets past the replay check (and stops
    // later at the missing manifest).
    envs.push(("VIBE_CHALLENGE_REPLAY_STORE", "memory"));
    let (ok, err) = serve(&envs, true);
    assert!(!ok);
    assert!(!err.contains("replay store"), "{err}");
}

/// Minimal source + lock with `when: after-failures` so `cmd_serve` hits the
/// #199 unverified gate via the real CLI startup path.
fn project_with_after_failures_lock() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vibe-serve-af-{}", uuid::Uuid::new_v4()));
    let feature = dir.join("api/auth/v1/login");
    std::fs::create_dir_all(&feature).unwrap();
    std::fs::write(
        feature.join("login.md"),
        "# login\n\n## Route\n\n- method: POST\n- path: /auth/v1/{slug}/login\n\n## Access\n\n- public: true\n- challenge: { type: captcha, when: after-failures, after-failures: 3 }\n",
    )
    .unwrap();
    std::fs::write(
        feature.join("login.lock.md"),
        r#"---
lock-version: 1
kind: api
scope: feature
id: auth/v1/login
source: api/auth/v1/login/login.md
source-hash: sha256:00
source-type: structured
dependencies: []
compiler: { name: md-logic, version: 0.1.0, model-snapshot: none, llm-provider: none, endpoint-host: none, endpoint-private: false, llm-seed: 0, sent-params: {} }
compiler-hash: sha256:00
status: pending
approval-mode: manual
approved-by: []
approved-at: (none)
approval-ref: (none)
approved-commit: (none)
security-items: []
signature: (none)
---

## Access
- public: true
- challenge: { type: captcha, when: after-failures, after-failures: 3 }
"#,
    )
    .unwrap();
    dir
}

fn serve_project(project: &std::path::Path, envs: &[(&str, &str)], memory: bool) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.arg("serve")
        .arg("--project")
        .arg(project)
        .arg("--manifest")
        .arg(project.join("missing.json"))
        .arg("--addr")
        .arg("127.0.0.1:0");
    if memory {
        cmd.arg("--memory");
    }
    for key in [
        "VIBE_ENV",
        "VIBE_API_KEY_PEPPER",
        "VIBE_DATABASE_URL",
        "DATABASE_URL",
        "LLM_API_KEY",
        "LLM_MODEL",
        "LLM_PROVIDER",
        "LLM_BASE_URL",
        "OPENAI_API_KEY",
        "VIBE_TURNSTILE_SECRET",
        "VIBE_CHALLENGE_HOSTNAMES",
        "VIBE_CHALLENGE_PROVIDER",
        "VIBE_CHALLENGE_REPLAY_STORE",
    ] {
        cmd.env_remove(key);
    }
    cmd.current_dir(project);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn production_refuses_after_failures_challenge_lock() {
    let dir = project_with_after_failures_lock();
    let (ok, err) = serve_project(&dir, &[("VIBE_ENV", "production")], false);
    assert!(!ok, "{err}");
    assert!(
        err.contains("temporarily unsupported in production"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn development_allows_after_failures_challenge_lock() {
    let dir = project_with_after_failures_lock();
    let (ok, err) = serve_project(&dir, &[("VIBE_ENV", "dev")], true);
    // Startup continues past the #199 gate and stops later (missing signing key
    // / manifest), matching other serve_env wiring checks.
    assert!(!ok, "{err}");
    assert!(
        !err.contains("after-failures") && !err.contains("not verified yet"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

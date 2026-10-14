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

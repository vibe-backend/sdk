#![cfg(feature = "sdk")]

//! `vibe run` refuses unsafe configurations before loading anything.

use std::process::Command;

fn serve(envs: &[(&str, &str)], memory: bool) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("vibe-serve-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.arg("run")
        .arg("--project")
        .arg(&dir)
        .arg("--artifact")
        .arg(dir.join("missing.json"))
        .arg("--addr")
        .arg("127.0.0.1:0");
    cmd.args(["--db", if memory { "memory" } else { "postgres" }]);
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
    for (key, _) in std::env::vars() {
        if key.starts_with("LLM_") || key.starts_with("OPENAI_") || key == "VIBE_LLM_API_KEY" {
            cmd.env_remove(key);
        }
    }
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
            err.contains("--db memory is a development executor"),
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

#[test]
fn static_jwt_key_must_match_the_configured_alg() {
    // Decision 139: checked by `vibe run` before anything else is loaded.
    let ec = include_str!("../src/runtime/testdata/ec-p256-p.pem");
    let p384 = include_str!("../src/runtime/testdata/ec-p384-p.pem");
    let (ok, err) = serve(
        &[
            ("VIBE_ENV", "development"),
            ("VIBE_JWT_PUBLIC_KEY_PEM", ec),
            ("VIBE_JWT_ALG", "RS256"),
        ],
        true,
    );
    assert!(!ok);
    assert!(err.contains("not an RSA public key"), "{err}");
    let (ok, err) = serve(
        &[
            ("VIBE_ENV", "development"),
            ("VIBE_JWT_PUBLIC_KEY_PEM", p384),
        ],
        true,
    );
    assert!(!ok);
    assert!(err.contains("P-256"), "{err}");
    assert!(
        !err.contains("MH"),
        "key material must not be printed: {err}"
    );
}

/// Minimal source + lock with `when: after-failures` so `cmd_serve` hits the
/// #199 unverified gate via the real CLI startup path.
fn project_with_after_failures_lock() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vibe-serve-af-{}", uuid::Uuid::new_v4()));
    let feature = dir.join("api/auth/v1");
    std::fs::create_dir_all(&feature).unwrap();
    std::fs::write(
        dir.join("api/_project.md"),
        "# project\n\n## Routes\n\n- prefix: (none)\n- default-slug: (none)\n",
    )
    .unwrap();
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
source: api/auth/v1/login.md
source-hash: sha256:00
source-type: structured
dependencies: []
compiler: { name: md-logic, version: 0.1.0 }
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
    cmd.arg("run")
        .arg("--project")
        .arg(project)
        .arg("--addr")
        .arg("127.0.0.1:0");
    cmd.args(["--db", if memory { "memory" } else { "postgres" }]);
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
    for (key, _) in std::env::vars() {
        if key.starts_with("LLM_") || key.starts_with("OPENAI_") || key == "VIBE_LLM_API_KEY" {
            cmd.env_remove(key);
        }
    }
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

#[test]
fn removed_dev_flag_is_refused() {
    let output = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .args(["build", "--dev"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--dev'"));
}

#[test]
fn signed_dev_manifest_starts_with_memory_in_development() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let root = std::env::temp_dir().join(format!("vibe-dev-serve-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(".gitignore"), ".env\n").unwrap();
    let compiled = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .args(["build", "--project"])
        .arg(&root)
        .arg("--output")
        .arg(root.join("built.json"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let key = vibe::sign::load_or_create(&root).unwrap();
    let manifest = vibe::manifest::manifest_from_project_with_mode(&root, &key, true).unwrap();
    let path = root.join("dev.json");
    vibe::manifest::write_manifest(&path, &manifest).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    std::fs::write(root.join(".env"), "OPENAI_API_KEY=dotenv-private-value\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .env("VIBE_ENV", "development")
        .env("LLM_API_KEY", "private-key-value")
        .env("LLM_BASE_URL", "https://private-url-value.example")
        .args(["run", "--db", "memory", "--addr", &addr, "--project"])
        .arg(&root)
        .arg("--artifact")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        sender.send(BufReader::new(stdout).lines().next()).unwrap();
    });
    let line = receiver.recv_timeout(std::time::Duration::from_secs(10));
    let served = if line.is_ok() {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(&addr).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response.starts_with("HTTP/1.1 ")
    } else {
        false
    };
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    reader.join().unwrap();
    assert!(served);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning: removing compile-time variables"));
    for name in ["LLM_API_KEY", "LLM_BASE_URL", "OPENAI_API_KEY"] {
        assert!(stderr.contains(name), "{stderr}");
    }
    for value in [
        "private-key-value",
        "private-url-value",
        "dotenv-private-value",
    ] {
        assert!(!stderr.contains(value), "{stderr}");
    }

    assert!(
        matches!(line, Ok(Some(Ok(ref line))) if line.contains("listening on")),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn production_and_unset_postgres_refuse_llm_env() {
    for envs in [
        vec![
            ("VIBE_ENV", "production"),
            ("LLM_API_KEY", "private-key-value"),
        ],
        vec![("LLM_API_KEY", "private-key-value")],
    ] {
        let (ok, err) = serve(&envs, false);
        assert!(!ok);
        assert!(
            err.contains("runtime environment must not contain LLM_API_KEY"),
            "{err}"
        );
        assert!(!err.contains("private-key-value"));
    }
}

// Run environment mutation in a dedicated process, isolated from parallel tests.
#[test]
fn strip_removes_runtime_credentials() {
    if std::env::var_os("VIBE_STRIP_TEST_CHILD").is_some() {
        let names = vibe::llm::strip_runtime_llm_env();
        assert_eq!(names.len(), 3);
        for name in ["LLM_API_KEY", "LLM_BASE_URL", "OPENAI_API_KEY"] {
            assert!(std::env::var_os(name).is_none());
        }
        assert_eq!(std::env::var("KEEP_ME").unwrap(), "retained");
        return;
    }
    let status = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .env("VIBE_STRIP_TEST_CHILD", "1")
        .env("LLM_API_KEY", "secret")
        .env("LLM_BASE_URL", "")
        .env("OPENAI_API_KEY", "secret")
        .env("KEEP_ME", "retained")
        .args(["--exact", "strip_removes_runtime_credentials"])
        .status()
        .unwrap();
    assert!(status.success());
}

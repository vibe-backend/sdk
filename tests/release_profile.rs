//! Release-profile behaviour of `vibe serve`. Run with `cargo test --release`:
//! a release binary only serves a manifest verified through its Sigstore
//! bundle, whatever `VIBE_ENV` or `--memory` say, and has no `--insecure-dev`.
#![cfg(not(debug_assertions))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn vibe(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.args(args).current_dir(root);
    for (key, _) in std::env::vars() {
        if key.starts_with("VIBE_")
            || key.starts_with("LLM_")
            || key.starts_with("OPENAI_")
            || key == "DATABASE_URL"
            || key == "GITHUB_SHA"
        {
            cmd.env_remove(key);
        }
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

/// A shop project with a manifest signed by the project development key.
fn dev_signed_project() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-release-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop"),
        &root,
    );
    let out = vibe(&root, &["compile", "--project", "."], &[])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    vibe::sign::create_dev_key(&root).unwrap();
    let out = vibe(&root, &["manifest", "--project", "."], &[])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(root.join("manifest.json").exists());
    assert!(root.join(".vibe/signing.pub").exists());
    root
}

/// Runs `vibe serve --memory`; a process still running after the timeout is
/// serving, which counts as success.
fn serve_memory(root: &Path, envs: &[(&str, &str)]) -> (bool, String) {
    let mut child = vibe(
        root,
        &[
            "serve",
            "--project",
            ".",
            "--memory",
            "--addr",
            "127.0.0.1:0",
            "--expect-version",
            "1.8.0",
            "--expect-commit",
            "0123456789abcdef0123456789abcdef01234567",
        ],
        envs,
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            let out = child.wait_with_output().unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return (out.status.success(), text);
        }
        if start.elapsed() > Duration::from_secs(20) {
            child.kill().unwrap();
            let _ = child.wait();
            return (true, "still serving after 20s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn assert_requires_bundle(root: &Path, envs: &[(&str, &str)]) {
    let (ok, out) = serve_memory(root, envs);
    assert!(!ok, "{envs:?}: {out}");
    assert!(out.contains("Sigstore bundle"), "{envs:?}: {out}");
    assert!(!out.contains("development key"), "{envs:?}: {out}");
}

#[test]
fn release_serve_with_development_env_requires_a_bundle() {
    let root = dev_signed_project();
    assert_requires_bundle(&root, &[("VIBE_ENV", "development")]);
    assert_requires_bundle(&root, &[("VIBE_ENV", "dev")]);
}

#[test]
fn release_serve_memory_without_env_requires_a_bundle() {
    let root = dev_signed_project();
    assert_requires_bundle(&root, &[]);
    assert_requires_bundle(&root, &[("VIBE_ENV", "")]);
}

#[test]
fn release_serve_ignores_a_pinned_development_key() {
    let root = dev_signed_project();
    let public = fs::read_to_string(root.join(".vibe/signing.pub")).unwrap();
    for envs in [
        vec![("VIBE_MANIFEST_PUBKEY", public.trim())],
        vec![
            ("VIBE_ENV", "development"),
            ("VIBE_MANIFEST_PUBKEY", public.trim()),
            ("VIBE_INSECURE_DEV", "1"),
        ],
    ] {
        assert_requires_bundle(&root, &envs);
    }
}

#[test]
fn release_binary_has_no_insecure_dev_flag() {
    let root = dev_signed_project();
    for cmd in ["serve", "migrate"] {
        let out = vibe(&root, &[cmd, "--insecure-dev", "--help"], &[])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{cmd}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--insecure-dev"), "{cmd}: {err}");
        let help = vibe(&root, &[cmd, "--help"], &[]).output().unwrap();
        assert!(!String::from_utf8_lossy(&help.stdout).contains("insecure-dev"));
    }
}

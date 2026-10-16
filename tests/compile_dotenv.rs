//! `vibe compile` reads the project `.env` only in development; CI and
//! production take every input from the process environment.

use std::fs;
use std::path::{Path, PathBuf};

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

/// The shop fixture (with one prose source) and a `.env` that sets an invalid LLM provider, so
/// whether `.env` was read shows up in the error.
fn shop_with_dotenv() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-dotenv-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop"),
        &root,
    );
    fs::write(root.join(".env"), "LLM_PROVIDER=not-a-provider\n").unwrap();
    // One prose line makes the project need an LLM config.
    let source = root.join("api/catalog/v1/export-products/export-products.md");
    let mut text = fs::read_to_string(&source).unwrap();
    text.push_str("\nReturn products sorted by name.\n");
    fs::write(&source, text).unwrap();
    root
}

fn compile(root: &Path, envs: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.args(["compile", "--check", "--project", "."])
        .current_dir(root);
    for (key, _) in std::env::vars() {
        if key.starts_with("LLM_")
            || key.starts_with("OPENAI_")
            || key.starts_with("VIBE_")
            || key == "CI"
            || key == "GITHUB_ACTIONS"
        {
            cmd.env_remove(key);
        }
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "compile without an LLM must fail");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const FROM_DOTENV: &str = "LLM_PROVIDER must be";

#[test]
fn compile_reads_dotenv_in_development() {
    let out = compile(&shop_with_dotenv(), &[]);
    assert!(out.contains(FROM_DOTENV), "{out}");
}

#[test]
fn compile_ignores_dotenv_when_ci_is_set() {
    let out = compile(&shop_with_dotenv(), &[("CI", "true")]);
    assert!(!out.contains(FROM_DOTENV), "{out}");
}

#[test]
fn compile_ignores_dotenv_under_github_actions() {
    let out = compile(&shop_with_dotenv(), &[("GITHUB_ACTIONS", "true")]);
    assert!(!out.contains(FROM_DOTENV), "{out}");
}

#[test]
fn compile_ignores_dotenv_in_production() {
    let out = compile(&shop_with_dotenv(), &[("VIBE_ENV", "production")]);
    assert!(!out.contains(FROM_DOTENV), "{out}");
}

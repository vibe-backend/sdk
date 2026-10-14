//! Fail-closed tests for the product baselines (spec v1.8 decisions 89-94).

use std::fs;
use std::path::{Path, PathBuf};

use vibe::sign;

fn copy_shop() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-baseline-{}", uuid::Uuid::new_v4()));
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop");
    copy_dir(&src, &root);
    root
}

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

fn vibe(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.args(args).current_dir(root);
    for (key, _) in std::env::vars() {
        if key.starts_with("LLM_")
            || key.starts_with("OPENAI_")
            || key.starts_with("VIBE_")
            || key == "GITHUB_SHA"
            || key == "DATABASE_URL"
        {
            cmd.env_remove(key);
        }
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn release_key() -> (String, String) {
    let key = sign::generate_key();
    (
        hex::encode(key.to_bytes()),
        hex::encode(key.verifying_key().to_bytes()),
    )
}

/// Builds a release manifest at `<root>/release.json`; returns the public key hex.
fn release_manifest(root: &Path) -> String {
    let (secret, public) = release_key();
    let (ok, out) = vibe(
        root,
        &["build", "--release-approval", "pr:1", "-o", "release.json"],
        &[("VIBE_RELEASE_SIGNING_KEY", &secret)],
    );
    assert!(ok, "{out}");
    public
}

const SERVE_MEMORY: &[&str] = &[
    "serve",
    "--memory",
    "--manifest",
    "release.json",
    "--addr",
    "127.0.0.1:0",
];

// Decision 91: production refuses the in-memory database, with no override.

#[test]
fn d91_production_env_refuses_in_memory() {
    let root = copy_shop();
    let public = release_manifest(&root);
    let (ok, out) = vibe(
        &root,
        SERVE_MEMORY,
        &[
            ("VIBE_ENV", "production"),
            ("VIBE_MANIFEST_PUBKEY", &public),
        ],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("--memory"), "{out}");
}

#[test]
fn d91_release_manifest_refuses_in_memory_even_in_development() {
    let root = copy_shop();
    let public = release_manifest(&root);
    let (ok, out) = vibe(
        &root,
        SERVE_MEMORY,
        &[
            ("VIBE_ENV", "development"),
            ("VIBE_MANIFEST_PUBKEY", &public),
        ],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("build-mode: release"), "{out}");
}

#[test]
fn d91_production_refuses_a_dev_build() {
    let root = copy_shop();
    let key = sign::create_dev_key(&root).unwrap();
    let (ok, out) = vibe(&root, &["build", "--dev", "-o", "dev.json"], &[]);
    assert!(ok, "{out}");
    let public = hex::encode(key.verifying_key().to_bytes());
    let (ok, out) = vibe(
        &root,
        &["serve", "--manifest", "dev.json", "--addr", "127.0.0.1:0"],
        &[
            ("VIBE_ENV", "production"),
            ("VIBE_MANIFEST_PUBKEY", &public),
            ("VIBE_API_KEY_PEPPER", &"p".repeat(48)),
        ],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("only release manifests"), "{out}");
}

// Decision 92: a release build without the KMS key fails, never using a dev key.

#[test]
fn d92_missing_release_key_fails_the_build() {
    let root = copy_shop();
    let (ok, out) = vibe(
        &root,
        &["build", "--release-approval", "pr:1", "-o", "release.json"],
        &[],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("VIBE_RELEASE_SIGNING_KEY"), "{out}");
    assert!(!root.join("release.json").exists());
}

#[test]
fn d92_missing_release_key_does_not_fall_back_to_the_dev_key() {
    let root = copy_shop();
    sign::create_dev_key(&root).unwrap();
    let (ok, out) = vibe(
        &root,
        &["build", "--release-approval", "pr:1", "-o", "release.json"],
        &[],
    );
    assert!(!ok, "{out}");
    assert!(!root.join("release.json").exists());
}

#[test]
fn d92_empty_release_key_fails_the_build() {
    let root = copy_shop();
    let (ok, out) = vibe(
        &root,
        &["build", "--release-approval", "pr:1", "-o", "release.json"],
        &[("VIBE_RELEASE_SIGNING_KEY", "")],
    );
    assert!(!ok, "{out}");
    assert!(!root.join("release.json").exists());
}

// Decision 90: high-risk security items fail a release build without an approval.

#[test]
fn d90_high_risk_items_fail_the_build() {
    let root = copy_shop();
    let (secret, _) = release_key();
    let (ok, out) = vibe(
        &root,
        &["build", "-o", "release.json"],
        &[("VIBE_RELEASE_SIGNING_KEY", &secret)],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("high-risk"), "{out}");
    assert!(!root.join("release.json").exists());
}

#[test]
fn d90_blank_release_approval_does_not_count() {
    let root = copy_shop();
    let (secret, _) = release_key();
    let (ok, out) = vibe(
        &root,
        &["build", "--release-approval", " ", "-o", "release.json"],
        &[("VIBE_RELEASE_SIGNING_KEY", &secret)],
    );
    assert!(!ok, "{out}");
    assert!(!root.join("release.json").exists());
}

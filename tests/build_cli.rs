//! `vibe build` (spec 2C.3, issue #34).

use std::fs;
use std::path::{Path, PathBuf};

use vibe::sign;

fn copy_shop() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-build-{}", uuid::Uuid::new_v4()));
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop");
    copy_dir(&src, &root);
    root
}

fn copy_plain() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-plain-{}", uuid::Uuid::new_v4()));
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plain");
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

fn edit(root: &Path, rel: &str, from: &str, to: &str) {
    let path = root.join(rel);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "{rel} lacks {from:?}");
    fs::write(&path, text.replacen(from, to, 1)).unwrap();
}

/// A source whose compiled lock fails validation (TRUNCATE is forbidden).
fn break_migration(root: &Path) {
    edit(
        root,
        "migrations/main/0001-products-name.md",
        "- sql: CREATE INDEX CONCURRENTLY IF NOT EXISTS products_name_active ON products (name)",
        "- sql: TRUNCATE products",
    );
}

fn release_key() -> (String, ed25519_dalek::VerifyingKey) {
    let key = sign::generate_key();
    (hex::encode(key.to_bytes()), key.verifying_key())
}

fn vibe(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.args(args).current_dir(root);
    for (key, _) in std::env::vars() {
        if key.starts_with("LLM_")
            || key.starts_with("OPENAI_")
            || key.starts_with("VIBE_")
            || key == "GITHUB_SHA"
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

/// Every file under `root`, relative, sorted.
fn tree(root: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(path.strip_prefix(base).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

const BUILD: &[&str] = &[
    "build",
    "--version",
    "1.8.0",
    "--commit",
    "0123456789abcdef0123456789abcdef01234567",
    "-o",
    "out/manifest.json",
];

fn read_out(root: &Path) -> vibe::Manifest {
    serde_json::from_slice(&fs::read(root.join("out/manifest.json")).unwrap()).unwrap()
}

#[test]
fn release_build_writes_a_keyless_manifest_when_there_are_no_high_risk_items() {
    let root = copy_plain();
    let (ok, out) = vibe(&root, BUILD, &[]);
    assert!(ok, "{out}");
    let manifest: vibe::Manifest =
        serde_json::from_slice(&fs::read(root.join("out/manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.signature, vibe::attest::SIGSTORE_SIGNATURE);
    assert_eq!(manifest.provenance.build_mode, "release");
    assert_eq!(manifest.provenance.version, "1.8.0");
    assert_eq!(
        manifest.provenance.commit,
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert!(
        manifest.provenance.high_risk().is_empty(),
        "{:?}",
        manifest.provenance.security_items
    );
}

#[test]
fn release_build_lists_high_risk_items_and_writes_a_keyless_manifest() {
    // Decision 90 is withdrawn: high-risk items are reported, and the human
    // gate is the `release` environment approval.
    let root = copy_shop();
    let (ok, out) = vibe(&root, BUILD, &[]);
    assert!(ok, "{out}");
    assert!(out.contains("high-risk items"), "{out}");
    let manifest = read_out(&root);
    assert_eq!(manifest.signature, vibe::attest::SIGSTORE_SIGNATURE);
    assert!(!manifest.provenance.high_risk().is_empty());
}

#[test]
fn release_approval_flag_is_gone() {
    let root = copy_shop();
    for args in [
        &["build", "--release-approval", "pr:1"][..],
        &["manifest", "--release", "--release-approval", "pr:1"][..],
    ] {
        let (ok, out) = vibe(&root, args, &[]);
        assert!(!ok, "{out}");
        assert!(out.contains("--release-approval"), "{out}");
    }
    let (ok, out) = vibe(&root, &["approve"], &[]);
    assert!(!ok, "{out}");
    assert!(out.contains("removed"), "{out}");
}

#[test]
fn release_build_requires_a_full_commit_and_a_release_version() {
    let root = copy_plain();
    let before = tree(&root);
    for args in [
        &["build", "--version", "1.8.0", "-o", "out/manifest.json"][..],
        &[
            "build",
            "--version",
            "1.8.0",
            "--commit",
            "0123abc",
            "-o",
            "out/manifest.json",
        ][..],
        &[
            "build",
            "--version",
            "latest",
            "--commit",
            "0123456789abcdef0123456789abcdef01234567",
            "-o",
            "out/manifest.json",
        ][..],
    ] {
        let (ok, out) = vibe(&root, args, &[]);
        assert!(!ok, "{args:?}: {out}");
        assert!(
            out.contains("full 40-character") || out.contains("invalid release version"),
            "{args:?}: {out}"
        );
    }
    assert_eq!(tree(&root), before);
}

#[test]
fn build_with_a_validation_error_writes_nothing() {
    let root = copy_shop();
    break_migration(&root);
    let before = tree(&root);
    let (ok, out) = vibe(&root, BUILD, &[]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("0001-products-name"),
        "the error is reported: {out}"
    );
    assert!(out.contains("nothing was written"), "{out}");
    assert_eq!(tree(&root), before, "no partial output");
}

#[test]
fn build_validates_before_it_needs_a_key() {
    let root = copy_shop();
    break_migration(&root);
    let (ok, out) = vibe(&root, BUILD, &[]);
    assert!(!ok, "{out}");
    assert!(out.contains("validation failed"), "{out}");
    assert!(!out.contains("VIBE_RELEASE_SIGNING_KEY"), "{out}");
}

#[test]
fn release_build_shows_warnings() {
    let root = copy_shop();
    let (ok, out) = vibe(&root, BUILD, &[]);
    assert!(ok, "{out}");
    assert!(
        out.contains("warning: models/product"),
        "warnings are displayed: {out}"
    );
}

#[tokio::test]
async fn build_signs_the_validated_content_not_a_later_edit() {
    // Read once: a source edited after loading cannot reach the manifest.
    let root = copy_shop();
    let (loaded, _) = vibe::compile::compile_in_memory(&root, None, None)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let (result, _) = vibe::validate::validate_loaded(
        &vibe::validate::ValidateOptions {
            root: &root,
            deploy: true,
        },
        &loaded,
    );
    result.unwrap_or_else(|err| panic!("{err}"));
    edit(
        &root,
        "models/product/product.md",
        "max: 120",
        "max: 100000",
    );
    let key = sign::generate_key();
    let manifest = vibe::manifest::build_signed(
        &root,
        &loaded,
        &key,
        &vibe::manifest::BuildInfo {
            release: false,
            version: String::new(),
            commit: String::new(),
        },
    )
    .unwrap_or_else(|err| panic!("{err}"));
    let json = serde_json::to_string(&manifest.models).unwrap();
    assert!(!json.contains("100000"), "later edit leaked");
}

#[test]
fn release_build_takes_no_key_from_the_environment() {
    let root = copy_plain();
    let (secret, _) = release_key();
    let (ok, out) = vibe(&root, BUILD, &[("VIBE_RELEASE_SIGNING_KEY", &secret)]);
    assert!(ok, "{out}");
    let manifest = read_out(&root);
    assert_eq!(manifest.signature, vibe::attest::SIGSTORE_SIGNATURE);
}

#[test]
fn dev_build_signs_with_the_dev_key_and_marks_dev_mode() {
    let root = copy_shop();
    let key = sign::create_dev_key(&root).unwrap();
    let (ok, out) = vibe(&root, &["build", "--dev", "-o", "dev.json"], &[]);
    assert!(ok, "{out}");
    let manifest = vibe::manifest::read_manifest(&root.join("dev.json"), &key.verifying_key())
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(manifest.provenance.build_mode, "dev");
}

#[test]
fn dev_build_without_a_dev_key_fails_and_creates_none() {
    let root = copy_shop();
    let (secret, _) = release_key();
    let (ok, out) = vibe(
        &root,
        &["build", "--dev", "-o", "dev.json"],
        &[("VIBE_RELEASE_SIGNING_KEY", &secret)],
    );
    assert!(!ok, "{out}");
    assert!(!root.join(".vibe/signing.key").exists());
    assert!(!root.join("dev.json").exists());
}

#[test]
fn release_build_never_uses_the_dev_key() {
    let root = copy_shop();
    sign::create_dev_key(&root).unwrap();
    let (ok, out) = vibe(&root, BUILD, &[]);
    assert!(ok, "{out}");
    let manifest = read_out(&root);
    assert_eq!(manifest.signature, vibe::attest::SIGSTORE_SIGNATURE);
    assert_eq!(manifest.provenance.build_mode, "release");
}

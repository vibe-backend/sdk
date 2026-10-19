use base64::Engine;
use std::{fs, path::Path, process::Command};
use vibe::sigstore::{Pin, Signer, ISSUER};

fn pin() -> Pin {
    Pin::parse(
        "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v*",
        ISSUER,
        None,
    )
    .unwrap()
}
fn manifest(root: &Path, test_build: bool) {
    manifest_with_commit(root, test_build, &"a".repeat(40));
}
fn manifest_with_commit(root: &Path, test_build: bool, commit: &str) {
    fs::write(root.join("manifest.json"), serde_json::to_vec(&serde_json::json!({
        "manifest_version": 1, "build_mode": "release", "routes": [], "models": [],
        "methods": [], "migrations": [], "signature": "",
        "provenance": { "version": "1.8.0", "commit": commit, "locks": [], "security_items": [], "test_build": test_build }
    })).unwrap()).unwrap();
}
fn bundle(root: &Path, repo: &str, digest: &str) {
    bundle_with_commit(root, repo, digest, &"a".repeat(40));
}
fn bundle_with_commit(root: &Path, repo: &str, digest: &str, commit: &str) {
    let signer = "https://github.com/vibe-backend/sdk/.github/workflows/vibe-project-release.yml@refs/tags/v1.8.0";
    let mut params = rcgen::CertificateParams::default();
    params.subject_alt_names = vec![rcgen::SanType::URI(signer.try_into().unwrap())];
    for (suffix, value) in [
        (8, ISSUER.to_owned()),
        (9, signer.to_owned()),
        (10, digest.to_owned()),
        (11, "github-hosted".into()),
        (12, format!("https://github.com/{repo}")),
        (13, commit.into()),
        (14, "refs/tags/v1.8.0".into()),
        (
            18,
            format!("https://github.com/{repo}/.github/workflows/release.yml@refs/tags/v1.8.0"),
        ),
        (20, "push".into()),
    ] {
        use der::Encode;
        let bytes = der::asn1::Utf8StringRef::new(&value)
            .unwrap()
            .to_der()
            .unwrap();
        params
            .custom_extensions
            .push(rcgen::CustomExtension::from_oid_content(
                &[1, 3, 6, 1, 4, 1, 57264, 1, suffix],
                bytes,
            ));
    }
    let cert = params
        .self_signed(&rcgen::KeyPair::generate().unwrap())
        .unwrap();
    fs::write(root.join("bundle.json"), serde_json::to_vec(&serde_json::json!({
        "verificationMaterial": { "certificate": { "rawBytes": base64::engine::general_purpose::STANDARD.encode(cert.der()) },
        "tlogEntries": [{"integratedTime": "1", "inclusionProof": {}}] }
    })).unwrap()).unwrap();
}
fn root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("release-pins-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    root
}
#[test]
fn caller_and_signer_are_independently_bound() {
    let root = root();
    manifest(&root, false);
    let signer = Signer::new("1.8.0", &"b".repeat(40)).unwrap();
    for (repo, digest, expected) in [
        ("acme/shop-b", "b".repeat(40), "source repository"),
        ("acme/shop-a", "c".repeat(40), "build signer digest"),
    ] {
        bundle(&root, repo, &digest);
        let err = vibe::manifest::read_release_manifest(
            &root.join("manifest.json"),
            &root.join("bundle.json"),
            "1.8.0",
            &pin(),
            &signer,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(expected), "{err}");
    }
    manifest(&root, true);
    let err = vibe::manifest::read_release_manifest(
        &root.join("manifest.json"),
        &root.join("missing"),
        "1.8.0",
        &pin(),
        &signer,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("release manifest is marked test_build"),
        "{err}"
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn matching_project_reaches_cosign_and_other_project_is_refused() {
    let root = root();
    let signer = Signer::new("1.8.0", &"b".repeat(40)).unwrap();
    for (repo, commit, matches) in [
        ("acme/shop-a", "a".repeat(40), true),
        ("acme/shop-b", "c".repeat(40), false),
    ] {
        manifest_with_commit(&root, false, &commit);
        bundle_with_commit(&root, repo, &"b".repeat(40), &commit);
        let err = vibe::manifest::read_release_manifest(
            &root.join("manifest.json"),
            &root.join("bundle.json"),
            "1.8.0",
            &pin(),
            &signer,
        )
        .unwrap_err()
        .to_string();
        if matches {
            let err = err.strip_prefix("error: manifest: ").expect(&err);
            assert!(
                err.starts_with("cannot run cosign")
                    || err.starts_with("Sigstore verification of the release manifest failed"),
                "{err}"
            );
            assert!(!err.starts_with("certificate "), "{err}");
        } else {
            assert!(err.contains("source repository"), "{err}");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

fn runtime(root: &Path, env: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vibe-runtime"));
    c.env_clear()
        .env("VIBE_ENV", env)
        .env("VIBE_DATABASE_URL", "postgres://invalid.invalid/x")
        .env("VIBE_API_KEY_PEPPER", "a".repeat(36))
        .env("VIBE_RELEASE_VERSION", "1.8.0")
        .arg("--artifact")
        .arg(root)
        .args(["--db", "postgres", "--addr", "127.0.0.1:0"]);
    c
}
fn fails(c: &mut Command, expected: &[&str]) {
    let out = c.output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    for text in expected {
        assert!(err.contains(text), "expected {text}: {err}");
    }
}
#[test]
fn runtime_fails_before_database_connect() {
    let root = root();
    manifest(&root, false);
    for env in ["production", "development"] {
        fails(
            &mut runtime(&root, env),
            &[
                "--expect-identity / VIBE_EXPECT_IDENTITY",
                "--expect-issuer / VIBE_EXPECT_ISSUER",
                "no globs or regex",
            ],
        );
    }
    fails(
        runtime(&root, "production")
            .args(["--expect-identity", "X"])
            .env("VIBE_EXPECT_IDENTITY", "Y"),
        &["--expect-identity and VIBE_EXPECT_IDENTITY disagree; set only one"],
    );
    fails(
        runtime(&root, "production").args(["--expect-identity", "X"]),
        &["VIBE_EXPECT_IDENTITY is invalid"],
    );
    let identity = "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v*";
    if option_env!("VIBE_SDK_COMMIT").is_none() {
        fails(
            runtime(&root, "production").args([
                "--expect-identity",
                identity,
                "--expect-issuer",
                ISSUER,
            ]),
            &["no compiled-in SDK signer commit"],
        );
    }
    manifest(&root, true);
    fails(
        runtime(&root, "production").arg("--allow-test-build"),
        &["release manifest is marked test_build; release manifests can never be test builds"],
    );
    fs::remove_dir_all(root).unwrap();
}

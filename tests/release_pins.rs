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
    bundle_fields(root, repo, digest, commit, None);
}
fn bundle_fields(
    root: &Path,
    repo: &str,
    digest: &str,
    commit: &str,
    replacement: Option<(u64, &str)>,
) {
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
        (15, "123".into()),
        (
            18,
            format!("https://github.com/{repo}/.github/workflows/release.yml@refs/tags/v1.8.0"),
        ),
        (20, "push".into()),
    ] {
        let value = replacement
            .filter(|(oid, _)| *oid == suffix)
            .map(|(_, value)| value.to_owned())
            .unwrap_or(value);
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

const IDENTITY: &str = "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v*";
fn verify(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vibe-runtime"));
    command
        .env_clear()
        .env("VIBE_ENV", "production")
        .env("VIBE_RELEASE_VERSION", "1.8.0")
        .args(["verify", "--artifact"])
        .arg(root);
    command
}
fn pinned(command: &mut Command) -> &mut Command {
    command.args(["--expect-identity", IDENTITY, "--expect-issuer", ISSUER])
}
#[test]
fn verify_binary_errors_and_help() {
    let root = root();
    manifest(&root, false);
    fails(
        &mut verify(&root),
        &[
            "--expect-identity / VIBE_EXPECT_IDENTITY",
            "--expect-issuer / VIBE_EXPECT_ISSUER",
            "no globs or regex",
        ],
    );
    fails(
        verify(&root)
            .args(["--expect-identity", "X"])
            .env("VIBE_EXPECT_IDENTITY", "Y"),
        &["--expect-identity and VIBE_EXPECT_IDENTITY disagree; set only one"],
    );
    fails(
        verify(&root).args(["--expect-identity", "X"]),
        &["VIBE_EXPECT_IDENTITY is invalid"],
    );
    fails(
        verify(&root).args(["--expect-issuer", "other"]),
        &["not supported"],
    );
    fails(
        verify(&root).args(["--expect-repository-id", "abc"]),
        &["repository"],
    );
    fails(
        pinned(&mut verify(&root)).env_remove("VIBE_RELEASE_VERSION"),
        &["VIBE_RELEASE_VERSION is required"],
    );
    fails(
        pinned(&mut verify(&root)).env("LLM_API_KEY", "test"),
        &["runtime environment must not contain LLM_API_KEY"],
    );
    for option in ["--addr", "--db"] {
        fails(verify(&root).args([option, "x"]), &["unexpected argument"]);
    }
    if option_env!("VIBE_SDK_COMMIT").is_none() {
        let out = pinned(&mut verify(&root)).output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(err.trim(), "this vibe-runtime build has no compiled-in SDK signer commit (VIBE_SDK_COMMIT at build time); only official SDK release binaries can verify release manifests");
    }
    // Even the right claim cannot make another project's certificate trusted.
    bundle(&root, "acme/shop-b", &"b".repeat(40));
    fails(
        pinned(&mut verify(&root)).env("VIBE_MANIFEST_BUNDLE", root.join("bundle.json")),
        &[if option_env!("VIBE_SDK_COMMIT").is_none() {
            "no compiled-in SDK signer commit"
        } else {
            "certificate"
        }],
    );
    manifest(&root, true);
    fails(
        pinned(&mut verify(&root)).arg("--allow-test-build"),
        &["release manifest is marked test_build; release manifests can never be test builds"],
    );
    let help = Command::new(env!("CARGO_BIN_EXE_vibe-runtime"))
        .env_clear()
        .args(["verify", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--expect-identity"));
    fs::remove_dir_all(root).unwrap();
}

// Invoke the shared library path in a clean child process to avoid mutating the
// test runner's environment while its other tests run concurrently.
#[test]
fn shared_verify_child() {
    let Ok(root) = std::env::var("VERIFY_TEST_ROOT") else {
        return;
    };
    let signer = Signer::new("1.8.0", &"b".repeat(40)).unwrap();
    let result = vibe::run::verify_artifact_with_signer(
        &vibe::run::VerifyOptions {
            artifact: root.clone().into(),
            project: root.into(),
            pins: Default::default(),
            allow_test_build: true,
        },
        Some(&signer),
    );
    let error = result.err().expect("synthetic certificate must not verify");
    println!("shared verification error: {error}");
    let expected = std::env::var("VERIFY_TEST_EXPECTED").unwrap();
    assert!(error.contains(&expected), "expected {expected}: {error}");
}
fn shared(root: &Path, expected: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("PATH", "")
        .args(["--exact", "shared_verify_child", "--nocapture"])
        .env("VERIFY_TEST_ROOT", root)
        .env("VERIFY_TEST_EXPECTED", expected)
        .env("VIBE_ENV", "production")
        .env("VIBE_RELEASE_VERSION", "1.8.0")
        .env("VIBE_EXPECT_IDENTITY", IDENTITY)
        .env("VIBE_EXPECT_ISSUER", ISSUER)
        .env("VIBE_EXPECT_REPOSITORY_ID", "123")
        .env("VIBE_MANIFEST_BUNDLE", root.join("bundle.json"));
    command
}
fn shared_ok(command: &mut Command) -> String {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stderr).unwrap()
}
#[test]
fn shared_verification_binds_every_field_and_never_trusts_claims() {
    let root = root();
    manifest(&root, false);
    for (oid, value, expected) in [
        (12, "https://github.com/acme/shop-b", "source repository"),
        (18, "wrong", "build config"),
        (14, "refs/tags/v1.8.1", "source ref"),
        (13, &"c".repeat(40), "source commit"),
        (15, "456", "source repository id"),
        (20, "pull_request", "build trigger"),
        (11, "self-hosted", "runner environment"),
        (8, "wrong", "issuer"),
        (9, "wrong", "signer workflow"),
        (10, &"c".repeat(40), "build signer digest"),
    ] {
        bundle_fields(
            &root,
            "acme/shop-a",
            &"b".repeat(40),
            &"a".repeat(40),
            Some((oid, value)),
        );
        shared_ok(&mut shared(&root, expected));
    }
    bundle(&root, "acme/shop-a", &"b".repeat(40));
    // Empty PATH guarantees cosign cannot execute or use the network.
    shared_ok(&mut shared(&root, "cannot run cosign"));
    shared_ok(
        shared(&root, "cannot read Sigstore bundle")
            .env("VIBE_MANIFEST_BUNDLE", root.join("missing")),
    );
    shared_ok(
        shared(&root, "does not match VIBE_RELEASE_VERSION")
            .env("VIBE_EXPECT_IDENTITY", IDENTITY.replace("v*", "v1.8.1")),
    );
    let file = root.join("manifest.json");
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    json["provenance"]["version"] = "1.8.1".into();
    fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
    shared_ok(&mut shared(&root, "not the deployed version"));
    json["provenance"]["version"] = "1.8.0".into();
    json["provenance"]["signing"] = serde_json::json!({
        "issuer": ISSUER, "source_identity": IDENTITY.replace("v*", "v1.8.0"),
        "source_repository": "acme/shop-a", "signer_workflow": "not trusted"
    });
    fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
    bundle(&root, "acme/shop-b", &"b".repeat(40));
    let hints = shared_ok(&mut shared(&root, "source repository"));
    assert!(!hints.contains("manifest claims"), "{hints}");
    json["provenance"]["signing"]["source_identity"] = IDENTITY
        .replace("shop-a", "shop-b")
        .replace("v*", "v1.8.0")
        .into();
    json["provenance"]["signing"]["issuer"] = "wrong".into();
    fs::write(&file, serde_json::to_vec(&json).unwrap()).unwrap();
    let hints = shared_ok(&mut shared(&root, "source repository"));
    assert!(
        hints.contains("note: manifest claims source identity"),
        "{hints}"
    );
    assert!(
        hints.contains("note: manifest claims issuer wrong; pinned issuer is"),
        "{hints}"
    );
    let out = pinned(&mut verify(&root)).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("manifest claims source identity"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verify_development_requires_explicit_development_and_checks_signature() {
    let root = root();
    let key = vibe::sign::load_or_create(&root).unwrap();
    let mut manifest = vibe::manifest::Manifest {
        manifest_version: 1,
        build_mode: Some(vibe::manifest::BuildMode::Dev),
        routes: vec![],
        models: vec![],
        methods: vec![],
        migrations: vec![],
        provenance: Default::default(),
        signature: String::new(),
    };
    manifest.signature = vibe::sign::sign_bytes(&serde_json::to_vec(&manifest).unwrap(), &key);
    vibe::manifest::write_manifest(&root.join("manifest.json"), &manifest).unwrap();
    fails(&mut verify(&root), &["development manifest", "refused"]);
    fails(
        verify(&root).env_remove("VIBE_ENV"),
        &["development manifest", "refused"],
    );
    let out = verify(&root)
        .env("VIBE_ENV", "development")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("(development manifest)"));
    fs::remove_dir_all(root).unwrap();
}

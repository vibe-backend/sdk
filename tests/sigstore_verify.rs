//! Keyless release verification (decisions 113, 118, 119): the bundle must be
//! a Sigstore attestation of this exact manifest, chained to the public-good
//! trust root, from the expected repo, workflow, tag and commit, with a
//! verified Rekor inclusion proof and SET (or checkpoint).

#[path = "support/sigstore_fixture.rs"]
mod sigstore_fixture;

use sigstore_fixture::Fixture;
use vibe::attest::{
    check_identity_claims, verify_bundle, verify_bundle_digest, Expectation, Policy,
    DEFAULT_WORKFLOW, GITHUB_OIDC_ISSUER,
};

const MANIFEST: &[u8] = br#"{"routes":[],"signature":"sigstore-keyless"}"#;

/// Public-good GitHub Actions DSSE bundle (wolfv/sigstore-test).
const GHA_BUNDLE: &str = include_str!("fixtures/sigstore/gha-dsse.sigstore.json");
const GHA_DIGEST: &str = "59ed81ee7a2485c47588ebdbad14764bf722c93438b43fe953a651747bc62ad7";
const GHA_COMMIT: &str = "a778b190311a56f04ac915c3e22ce7891c9eedbf";
const GHA_REPO: &str = "wolfv/sigstore-test";
const GHA_WORKFLOW: &str = ".github/workflows/action.yaml";
const V01_BUNDLE: &str = include_str!("fixtures/sigstore/bundle-v0.1-set-only.sigstore.json");

fn expect(version: &str, commit: &str) -> Expectation {
    Expectation::new(version, commit).unwrap()
}

fn check(fixture: Fixture) -> Result<vibe::attest::Identity, String> {
    verify_bundle(
        MANIFEST,
        &fixture.bundle(MANIFEST),
        &Policy::default(),
        &expect("1.8.0", "abc123"),
    )
}

fn refused(fixture: Fixture, needle: &str) {
    let err = check(fixture).unwrap_err();
    assert!(err.contains(needle), "expected {needle:?} in {err}");
}

fn gha_policy() -> Policy {
    Policy {
        repo: GHA_REPO.into(),
        workflow: GHA_WORKFLOW.into(),
    }
}

#[test]
fn compiled_in_policy_matches_decision_119() {
    let policy = Policy::default();
    assert_eq!(policy.repo, "vibe-backend/sdk");
    assert_eq!(policy.workflow, DEFAULT_WORKFLOW);
    assert_eq!(DEFAULT_WORKFLOW, ".github/workflows/release.yml");
    assert_eq!(
        policy.expected_signer("1.8.0"),
        "https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.8.0"
    );
    let _ = GITHUB_OIDC_ISSUER;
}

#[test]
fn self_signed_bundle_is_refused() {
    let err = check(Fixture::default()).unwrap_err();
    assert!(
        err.contains("sigstore bundle:")
            || err.contains("certificate")
            || err.contains("trust")
            || err.contains("Fulcio")
            || err.contains("chain")
            || err.contains("verify"),
        "{err}"
    );
}

#[test]
fn missing_rekor_entry_is_refused() {
    refused(
        Fixture {
            tlog: false,
            ..Fixture::default()
        },
        "sigstore bundle:",
    );
    refused(
        Fixture {
            inclusion: false,
            ..Fixture::default()
        },
        "sigstore bundle:",
    );
}

#[test]
fn bad_signature_is_refused() {
    refused(
        Fixture {
            tamper_payload: true,
            ..Fixture::default()
        },
        "sigstore bundle:",
    );
}

#[test]
fn other_manifest_is_refused() {
    let bundle = Fixture::default().bundle(b"{\"other\":true}");
    let err = verify_bundle(
        MANIFEST,
        &bundle,
        &Policy::default(),
        &expect("1.8.0", "abc123"),
    )
    .unwrap_err();
    assert!(err.contains("sigstore bundle:"), "{err}");
}

#[test]
fn missing_or_keyed_bundle_is_refused() {
    let err = verify_bundle(
        MANIFEST,
        b"{}",
        &Policy::default(),
        &expect("1.8.0", "abc123"),
    )
    .unwrap_err();
    assert!(err.contains("bundle") || err.contains("sigstore"), "{err}");
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&Fixture::default().bundle(MANIFEST)).unwrap();
    bundle["verificationMaterial"]
        .as_object_mut()
        .unwrap()
        .remove("certificate");
    bundle["verificationMaterial"]["publicKey"] = serde_json::json!({"hint": "k"});
    let err = verify_bundle(
        MANIFEST,
        &serde_json::to_vec(&bundle).unwrap(),
        &Policy::default(),
        &expect("1.8.0", "abc123"),
    )
    .unwrap_err();
    assert!(
        err.contains("sigstore bundle:") || err.contains("keyless"),
        "{err}"
    );
}

#[test]
fn set_only_v0_1_bundle_is_refused() {
    let err = verify_bundle_digest(
        GHA_DIGEST,
        V01_BUNDLE.as_bytes(),
        &gha_policy(),
        &expect("1.0.0", GHA_COMMIT),
        false,
    )
    .unwrap_err();
    assert!(
        err.contains("0.1") || err.contains("media type") || err.contains("inclusion"),
        "{err}"
    );
}

#[test]
fn public_good_bundle_verifies_against_the_trust_root() {
    use sigstore_trust_root::{TrustedRoot, SIGSTORE_PRODUCTION_TRUSTED_ROOT};
    use sigstore_types::{Bundle, Sha256Hash};
    use sigstore_verify::{VerificationPolicy, Verifier};

    let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT).unwrap();
    let verifier = Verifier::new(&root).unwrap();
    let bundle = Bundle::from_json(GHA_BUNDLE).unwrap();
    let digest = Sha256Hash::from_hex(GHA_DIGEST).unwrap();
    let policy = VerificationPolicy::any_identity().require_issuer(GITHUB_OIDC_ISSUER.to_string());
    let result = verifier
        .verify(digest, &bundle, &policy)
        .expect("public-good fixture");
    assert!(result.certificate_verified());
    assert!(result.tlog_verified());
    assert_eq!(
        result.identity().map(|s| s.as_str()),
        Some(
            "https://github.com/wolfv/sigstore-test/.github/workflows/action.yaml@refs/heads/main"
        )
    );
}

#[test]
fn bundle_with_more_than_one_rekor_entry_is_refused() {
    let mut bundle: serde_json::Value = serde_json::from_str(GHA_BUNDLE).unwrap();
    let entries = bundle["verificationMaterial"]["tlogEntries"]
        .as_array_mut()
        .unwrap();
    assert_eq!(entries.len(), 1);
    let first = entries[0].clone();
    entries.push(first);
    let err = verify_bundle_digest(
        GHA_DIGEST,
        &serde_json::to_vec(&bundle).unwrap(),
        &gha_policy(),
        &expect("1.0.0", GHA_COMMIT),
        false,
    )
    .unwrap_err();
    assert!(err.contains("exactly one Rekor"), "{err}");
}

#[test]
fn public_good_bundle_wrong_identity_is_refused() {
    let err = verify_bundle_digest(
        GHA_DIGEST,
        GHA_BUNDLE.as_bytes(),
        &Policy::default(),
        &expect("1.0.0", GHA_COMMIT),
        false,
    )
    .unwrap_err();
    assert!(
        err.contains("repository") || err.contains("is not"),
        "{err}"
    );
}

#[test]
fn public_good_bundle_wrong_ref_is_refused() {
    // Fixture was signed on refs/heads/main; deployer expects a release tag.
    let err = verify_bundle_digest(
        GHA_DIGEST,
        GHA_BUNDLE.as_bytes(),
        &gha_policy(),
        &expect("1.0.0", GHA_COMMIT),
        false,
    )
    .unwrap_err();
    assert!(err.contains("is not the expected"), "{err}");
    assert!(err.contains("refs/tags/v1.0.0"), "{err}");
}

#[test]
fn public_good_bundle_wrong_version_is_refused() {
    let err = verify_bundle_digest(
        GHA_DIGEST,
        GHA_BUNDLE.as_bytes(),
        &gha_policy(),
        &expect("9.9.9", GHA_COMMIT),
        false,
    )
    .unwrap_err();
    assert!(err.contains("is not the expected"), "{err}");
    assert!(err.contains("refs/tags/v9.9.9"), "{err}");
}

#[test]
fn public_good_bundle_wrong_commit_is_refused() {
    // Use the fixture's real commit value in the certificate-claim check while
    // the deployer expects a different SHA (same pattern production uses after
    // Fulcio/Rekor succeed on this bundle).
    let policy = Policy::default();
    let expected = expect("1.8.0", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    let signer = policy.expected_signer("1.8.0");
    let err = check_identity_claims(
        Some(&signer),
        Some(GITHUB_OIDC_ISSUER),
        Some("https://github.com/vibe-backend/sdk"),
        Some("refs/tags/v1.8.0"),
        Some(GHA_COMMIT),
        Some(&signer),
        Some("github-hosted"),
        &policy,
        &expected,
    )
    .unwrap_err();
    assert!(err.contains("is not the expected"), "{err}");
    assert!(err.contains("deadbeef"), "{err}");
    // Confirm the public-good bundle still verifies cryptographically so the
    // claim above is not vacuously about a broken fixture.
    let crypto_err = verify_bundle_digest(
        GHA_DIGEST,
        GHA_BUNDLE.as_bytes(),
        &gha_policy(),
        &expect("1.0.0", GHA_COMMIT),
        false,
    )
    .unwrap_err();
    assert!(
        crypto_err.contains("is not the expected"),
        "fixture should pass crypto and fail only on identity/ref: {crypto_err}"
    );
}

#[test]
fn public_good_bundle_tampered_digest_is_refused() {
    let err = verify_bundle_digest(
        "0000000000000000000000000000000000000000000000000000000000000000",
        GHA_BUNDLE.as_bytes(),
        &gha_policy(),
        &expect("1.0.0", GHA_COMMIT),
        false,
    )
    .unwrap_err();
    assert!(err.contains("sigstore bundle:"), "{err}");
}

#[test]
fn policy_override_outside_development_is_refused() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    let prev_repo = std::env::var_os("VIBE_RELEASE_REPO");
    let prev_wf = std::env::var_os("VIBE_RELEASE_WORKFLOW");
    let prev_env = std::env::var_os("VIBE_ENV");
    let prev_insecure = std::env::var_os("VIBE_INSECURE_DEV");
    std::env::remove_var("VIBE_RELEASE_REPO");
    std::env::remove_var("VIBE_RELEASE_WORKFLOW");
    std::env::remove_var("VIBE_INSECURE_DEV");

    std::env::set_var("VIBE_ENV", "production");
    std::env::set_var("VIBE_RELEASE_REPO", "attacker/sdk");
    let err = Policy::from_env().unwrap_err();
    assert!(err.contains("VIBE_RELEASE_REPO"), "{err}");
    assert!(err.contains("development"), "{err}");

    std::env::remove_var("VIBE_RELEASE_REPO");
    std::env::set_var("VIBE_RELEASE_WORKFLOW", ".github/workflows/other.yml");
    let err = Policy::from_env().unwrap_err();
    assert!(err.contains("VIBE_RELEASE_WORKFLOW"), "{err}");

    std::env::set_var("VIBE_ENV", "development");
    std::env::set_var("VIBE_RELEASE_REPO", "acme/shop");
    if cfg!(debug_assertions) {
        // Debug builds allow overrides when VIBE_ENV=development.
        let policy = Policy::from_env().unwrap();
        assert_eq!(policy.repo, "acme/shop");
    } else {
        // Release builds never do, even with the opt-in variable set.
        std::env::set_var("VIBE_INSECURE_DEV", "1");
        let err = Policy::from_env().unwrap_err();
        assert!(err.contains("VIBE_RELEASE_REPO"), "{err}");
    }

    restore("VIBE_RELEASE_REPO", prev_repo);
    restore("VIBE_RELEASE_WORKFLOW", prev_wf);
    restore("VIBE_ENV", prev_env);
    restore("VIBE_INSECURE_DEV", prev_insecure);
}

fn restore(name: &str, prev: Option<std::ffi::OsString>) {
    match prev {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}

#[test]
fn empty_manifest_commit_is_refused() {
    let dir = tempfile_dir();
    let manifest_path = dir.join("manifest.json");
    let manifest = serde_json::json!({
        "manifest_version": 1,
        "routes": [],
        "models": [],
        "methods": [],
        "migrations": [],
        "signature": "sigstore-keyless",
        "provenance": {
            "build_mode": "release",
            "version": "1.8.0",
            "commit": "",
            "locks": [],
            "security_items": [],
            "release_approval": null
        }
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    std::fs::write(&manifest_path, &bytes).unwrap();
    let bundle_path = vibe::attest::default_bundle_path(&manifest_path);
    std::fs::write(&bundle_path, Fixture::default().bundle(&bytes)).unwrap();
    let err = vibe::manifest::read_release_manifest(
        &manifest_path,
        &bundle_path,
        &Policy::default(),
        &expect("1.8.0", "abc123"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("manifest commit is empty"), "{err}");
}

#[test]
fn empty_manifest_version_is_refused() {
    let dir = tempfile_dir();
    let manifest_path = dir.join("manifest.json");
    let manifest = serde_json::json!({
        "manifest_version": 1,
        "routes": [],
        "models": [],
        "methods": [],
        "migrations": [],
        "signature": "sigstore-keyless",
        "provenance": {
            "build_mode": "release",
            "version": "",
            "commit": "abc123",
            "locks": [],
            "security_items": [],
            "release_approval": null
        }
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    std::fs::write(&manifest_path, &bytes).unwrap();
    let bundle_path = vibe::attest::default_bundle_path(&manifest_path);
    std::fs::write(&bundle_path, Fixture::default().bundle(&bytes)).unwrap();
    let err = vibe::manifest::read_release_manifest(
        &manifest_path,
        &bundle_path,
        &Policy::default(),
        &expect("1.8.0", "abc123"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("manifest version is empty"), "{err}");
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vibe-attest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

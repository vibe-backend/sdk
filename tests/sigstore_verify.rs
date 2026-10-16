//! Keyless release verification (decisions 113, 118, 119): the bundle must be
//! a Sigstore attestation of this exact manifest, chained to the public-good
//! trust root, from vibe-backend/sdk, the release workflow and a
//! `refs/tags/v*` ref, with a verified Rekor entry.

#[path = "support/sigstore_fixture.rs"]
mod sigstore_fixture;

use sigstore_fixture::Fixture;
use vibe::attest::{verify_bundle, Policy, DEFAULT_WORKFLOW, GITHUB_OIDC_ISSUER};

const MANIFEST: &[u8] = br#"{"routes":[],"signature":"sigstore-keyless"}"#;

fn check(fixture: Fixture) -> Result<vibe::attest::Identity, String> {
    verify_bundle(MANIFEST, &fixture.bundle(MANIFEST), &Policy::default())
}

fn refused(fixture: Fixture, needle: &str) {
    let err = check(fixture).unwrap_err();
    assert!(err.contains(needle), "expected {needle:?} in {err}");
}

#[test]
fn compiled_in_policy_matches_decision_119() {
    let policy = Policy::default();
    assert_eq!(policy.repo, "vibe-backend/sdk");
    assert_eq!(policy.workflow, DEFAULT_WORKFLOW);
    assert_eq!(DEFAULT_WORKFLOW, ".github/workflows/release.yml");
    assert_eq!(
        policy.expected_signer("refs/tags/v1.8.0"),
        "https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.8.0"
    );
    let _ = GITHUB_OIDC_ISSUER;
}

#[test]
fn self_signed_bundle_is_refused() {
    // A bundle whose certificate does not chain to the embedded Fulcio trust
    // root must fail closed, even when identity fields look correct.
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
    let err = verify_bundle(MANIFEST, &bundle, &Policy::default()).unwrap_err();
    assert!(err.contains("sigstore bundle:"), "{err}");
}

#[test]
fn missing_or_keyed_bundle_is_refused() {
    let err = verify_bundle(MANIFEST, b"{}", &Policy::default()).unwrap_err();
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
    )
    .unwrap_err();
    assert!(
        err.contains("sigstore bundle:") || err.contains("keyless"),
        "{err}"
    );
}

#[test]
fn policy_override_outside_development_is_refused() {
    // Serialize env mutations across this file's tests.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    let prev_repo = std::env::var_os("VIBE_RELEASE_REPO");
    let prev_wf = std::env::var_os("VIBE_RELEASE_WORKFLOW");
    let prev_env = std::env::var_os("VIBE_ENV");
    std::env::remove_var("VIBE_RELEASE_REPO");
    std::env::remove_var("VIBE_RELEASE_WORKFLOW");

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
    let policy = Policy::from_env().unwrap();
    assert_eq!(policy.repo, "acme/shop");

    restore("VIBE_RELEASE_REPO", prev_repo);
    restore("VIBE_RELEASE_WORKFLOW", prev_wf);
    restore("VIBE_ENV", prev_env);
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
    let err =
        vibe::manifest::read_release_manifest(&manifest_path, &bundle_path, &Policy::default())
            .unwrap_err()
            .to_string();
    assert!(err.contains("manifest commit is empty"), "{err}");
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vibe-attest-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

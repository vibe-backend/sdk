//! Keyless release verification (decisions 113, 118): the bundle must be a
//! valid Sigstore attestation of this exact manifest, from vibe-backend/sdk,
//! the release workflow and a `refs/tags/v*` ref, with a Rekor entry.

#[path = "support/sigstore_fixture.rs"]
mod sigstore_fixture;

use sigstore_fixture::Fixture;
use vibe::attest::{verify_bundle, Policy};

const MANIFEST: &[u8] = br#"{"routes":[],"signature":"sigstore-keyless"}"#;

fn check(fixture: Fixture) -> Result<vibe::attest::Identity, String> {
    verify_bundle(MANIFEST, &fixture.bundle(MANIFEST), &Policy::default())
}

fn refused(fixture: Fixture, needle: &str) {
    let err = check(fixture).unwrap_err();
    assert!(err.contains(needle), "{err}");
}

#[test]
fn valid_bundle_is_accepted() {
    let id = check(Fixture::default()).unwrap();
    assert_eq!(id.repo, "vibe-backend/sdk");
    assert_eq!(id.source_ref, "refs/tags/v1.0.0");
    assert_eq!(id.commit, "abc123");
    assert_eq!(id.rekor_log_index, "123456");
    assert_eq!(
        id.signer_workflow,
        "https://github.com/vibe-backend/sdk/.github/workflows/vibe-release.yml@refs/tags/v1.0.0"
    );
}

#[test]
fn wrong_repo_is_refused() {
    refused(
        Fixture {
            repo: "attacker/sdk".into(),
            ..Fixture::default()
        },
        "repository",
    );
}

#[test]
fn wrong_workflow_is_refused() {
    refused(
        Fixture {
            workflow: ".github/workflows/ci.yml".into(),
            ..Fixture::default()
        },
        "signer workflow",
    );
}

#[test]
fn wrong_ref_is_refused() {
    for r in [
        "refs/heads/main",
        "refs/tags/release-1",
        "refs/tags/v",
        "refs/pull/1/merge",
    ] {
        refused(
            Fixture {
                source_ref: r.into(),
                ..Fixture::default()
            },
            "is not a refs/tags/v* release tag",
        );
    }
}

#[test]
fn missing_rekor_entry_is_refused() {
    refused(
        Fixture {
            tlog: false,
            ..Fixture::default()
        },
        "no Rekor transparency log entry",
    );
    refused(
        Fixture {
            inclusion: false,
            ..Fixture::default()
        },
        "no inclusion proof or promise",
    );
    refused(
        Fixture {
            foreign_rekor_entry: true,
            ..Fixture::default()
        },
        "does not record this attestation",
    );
}

#[test]
fn bad_signature_is_refused() {
    refused(
        Fixture {
            tamper_payload: true,
            ..Fixture::default()
        },
        "DSSE signature does not verify",
    );
}

#[test]
fn other_manifest_is_refused() {
    let bundle = Fixture::default().bundle(b"{\"other\":true}");
    let err = verify_bundle(MANIFEST, &bundle, &Policy::default()).unwrap_err();
    assert!(err.contains("no subject matches"), "{err}");
}

#[test]
fn non_github_identity_or_runner_is_refused() {
    refused(
        Fixture {
            issuer: "https://accounts.google.com".into(),
            ..Fixture::default()
        },
        "not GitHub Actions",
    );
    refused(
        Fixture {
            runner: "self-hosted".into(),
            ..Fixture::default()
        },
        "not github-hosted",
    );
}

#[test]
fn missing_or_keyed_bundle_is_refused() {
    let err = verify_bundle(MANIFEST, b"{}", &Policy::default()).unwrap_err();
    assert!(err.contains("bundle json"), "{err}");
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
    assert!(err.contains("keyless signing required"), "{err}");
}

#[test]
fn policy_follows_runtime_configuration() {
    let fixture = Fixture {
        repo: "acme/shop".into(),
        workflow: ".github/workflows/deploy.yml".into(),
        ..Fixture::default()
    };
    let bundle = fixture.bundle(MANIFEST);
    assert!(verify_bundle(MANIFEST, &bundle, &Policy::default()).is_err());
    let policy = Policy {
        repo: "acme/shop".into(),
        workflow: ".github/workflows/deploy.yml".into(),
    };
    verify_bundle(MANIFEST, &bundle, &policy).unwrap();
}

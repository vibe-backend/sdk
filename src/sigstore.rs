//! Release manifest verification with Sigstore keyless signatures
//! (decisions 79, 92, 113, 119).
//!
//! Release manifests carry no long-lived key signature. The release workflow
//! signs `manifest.json` with its GitHub OIDC identity through Sigstore
//! (Fulcio certificate, Rekor transparency log) and ships the Sigstore bundle
//! next to it. Verification is offline against the trust root compiled into
//! this binary; there is no runtime override and no fallback.
//!
//! Two layers, both required:
//! 1. [`check_bundle_identity`] (pure Rust): the certificate in the bundle must
//!    name exactly the release workflow at `refs/tags/v<version>`, the GitHub
//!    Actions issuer, the expected commit, this repository, and a
//!    GitHub-hosted runner; the bundle must carry a Rekor entry with an
//!    inclusion proof.
//! 2. `cosign verify-blob` with the compiled-in `trusted_root.json`: checks the
//!    certificate chain, the signature over the manifest bytes, the Rekor
//!    inclusion proof / SET and the certificate validity at the Rekor
//!    integrated time, with the same exact identity, issuer, commit and ref.

use base64::Engine;
use std::path::Path;

/// The only accepted OIDC issuer.
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// The only repository whose release workflow may sign manifests.
pub const REPOSITORY: &str = "vibe-backend/sdk";
const REPOSITORY_URI: &str = "https://github.com/vibe-backend/sdk";
const WORKFLOW_PATH: &str = ".github/workflows/release.yml";

/// Sigstore trust root (Fulcio CA, Rekor and CT log keys), compiled in only.
pub const TRUSTED_ROOT: &str = include_str!("../assets/sigstore/trusted_root.json");

// Fulcio certificate extensions (OID 1.3.6.1.4.1.57264.1.*).
const OID_ISSUER_V1: &str = "1.3.6.1.4.1.57264.1.1";
const OID_ISSUER_V2: &str = "1.3.6.1.4.1.57264.1.8";
const OID_BUILD_SIGNER_URI: &str = "1.3.6.1.4.1.57264.1.9";
const OID_RUNNER_ENVIRONMENT: &str = "1.3.6.1.4.1.57264.1.11";
const OID_SOURCE_REPOSITORY_URI: &str = "1.3.6.1.4.1.57264.1.12";
const OID_SOURCE_REPOSITORY_DIGEST: &str = "1.3.6.1.4.1.57264.1.13";
const OID_SOURCE_REPOSITORY_REF: &str = "1.3.6.1.4.1.57264.1.14";

/// What a release manifest must be bound to.
#[derive(Clone, Debug)]
pub struct Expected {
    /// Deployed version without the leading `v`, e.g. `1.8.0`.
    pub version: String,
    /// Full 40-character source commit SHA.
    pub commit: String,
}

/// Checks a version string (`1.8.0`, `1.8.0-rc.1`): digits, dots and a
/// restricted pre-release suffix only, so it can be placed in an identity.
pub fn check_version(version: &str) -> Result<(), String> {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let core_ok = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let pre_ok = pre
        .is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'));
    if core_ok && pre_ok {
        Ok(())
    } else {
        Err(format!(
            "invalid release version `{version}` (expected e.g. 1.8.0, without `v`)"
        ))
    }
}

fn check_commit(commit: &str) -> Result<(), String> {
    if commit.len() == 40
        && commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(format!(
            "release commit `{commit}` must be a full 40-character lowercase hex SHA"
        ))
    }
}

/// The exact certificate identity for a release version.
pub fn release_identity(version: &str) -> String {
    format!("{REPOSITORY_URI}/{WORKFLOW_PATH}@{}", release_ref(version))
}

fn release_ref(version: &str) -> String {
    format!("refs/tags/v{version}")
}

/// Leaf certificate (DER) from a Sigstore bundle (v0.1–v0.3).
fn bundle_certificate(bundle: &serde_json::Value) -> Result<Vec<u8>, String> {
    let material = &bundle["verificationMaterial"];
    let raw = material["certificate"]["rawBytes"]
        .as_str()
        .or_else(|| material["x509CertificateChain"]["certificates"][0]["rawBytes"].as_str())
        .ok_or("Sigstore bundle has no signing certificate")?;
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| "Sigstore bundle certificate is not base64".to_string())
}

fn has_rekor_inclusion(bundle: &serde_json::Value) -> bool {
    bundle["verificationMaterial"]["tlogEntries"]
        .as_array()
        .is_some_and(|entries| {
            !entries.is_empty()
                && entries.iter().all(|e| {
                    e["inclusionProof"].is_object() && e["integratedTime"].as_str().is_some()
                })
        })
}

/// Fulcio v2 extensions are DER UTF8String; v1 extensions are raw bytes.
fn extension_text(value: &[u8], der: bool) -> Option<String> {
    if !der {
        return String::from_utf8(value.to_vec()).ok();
    }
    let (rest, obj) = x509_parser::der_parser::der::parse_der_utf8string(value).ok()?;
    if !rest.is_empty() {
        return None;
    }
    obj.as_str().ok().map(str::to_string)
}

/// Identity checks on the bundle certificate (decision 119). Every value must
/// be equal, not merely match a pattern.
pub fn check_bundle_identity(bundle_json: &[u8], expected: &Expected) -> Result<(), String> {
    check_version(&expected.version)?;
    check_commit(&expected.commit)?;
    let bundle: serde_json::Value = serde_json::from_slice(bundle_json)
        .map_err(|err| format!("Sigstore bundle json: {err}"))?;
    if !has_rekor_inclusion(&bundle) {
        return Err("Sigstore bundle has no Rekor inclusion proof".into());
    }
    let der = bundle_certificate(&bundle)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der)
        .map_err(|err| format!("Sigstore bundle certificate: {err}"))?;

    let identity = release_identity(&expected.version);
    let mut sans = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            match name {
                x509_parser::extensions::GeneralName::URI(uri) => sans.push(uri.to_string()),
                other => sans.push(format!("{other:?}")),
            }
        }
    }
    if sans != [identity.clone()] {
        return Err(format!(
            "certificate identity {sans:?} is not exactly {identity}"
        ));
    }

    let ext = |oid: &str, der: bool| -> Option<String> {
        cert.extensions()
            .iter()
            .find(|e| e.oid.to_id_string() == oid)
            .and_then(|e| extension_text(e.value, der))
    };
    let issuer = ext(OID_ISSUER_V2, true).or_else(|| ext(OID_ISSUER_V1, false));
    let want = [
        ("issuer", issuer, ISSUER.to_string()),
        (
            "build signer",
            ext(OID_BUILD_SIGNER_URI, true),
            identity.clone(),
        ),
        (
            "runner environment",
            ext(OID_RUNNER_ENVIRONMENT, true),
            "github-hosted".into(),
        ),
        (
            "source repository",
            ext(OID_SOURCE_REPOSITORY_URI, true),
            REPOSITORY_URI.into(),
        ),
        (
            "source commit",
            ext(OID_SOURCE_REPOSITORY_DIGEST, true),
            expected.commit.clone(),
        ),
        (
            "source ref",
            ext(OID_SOURCE_REPOSITORY_REF, true),
            release_ref(&expected.version),
        ),
    ];
    for (what, got, want) in want {
        match got {
            Some(got) if got == want => {}
            Some(got) => return Err(format!("certificate {what} is `{got}`, expected `{want}`")),
            None => return Err(format!("certificate has no {what} extension")),
        }
    }
    Ok(())
}

/// The `cosign verify-blob` arguments for a release manifest. Exact identity
/// (no regexp), pinned issuer, commit, repository and ref, the compiled-in
/// trust root, offline, and the transparency log is never skipped.
pub fn cosign_args(
    manifest: &Path,
    bundle: &Path,
    trusted_root: &Path,
    expected: &Expected,
) -> Vec<String> {
    vec![
        "verify-blob".into(),
        manifest.display().to_string(),
        "--bundle".into(),
        bundle.display().to_string(),
        "--new-bundle-format".into(),
        "--trusted-root".into(),
        trusted_root.display().to_string(),
        "--offline".into(),
        "--certificate-identity".into(),
        release_identity(&expected.version),
        "--certificate-oidc-issuer".into(),
        ISSUER.into(),
        "--certificate-github-workflow-sha".into(),
        expected.commit.clone(),
        "--certificate-github-workflow-repository".into(),
        REPOSITORY.into(),
        "--certificate-github-workflow-ref".into(),
        release_ref(&expected.version),
    ]
}

/// Verifies a release manifest file against its Sigstore bundle. Fails closed
/// on any error, including a missing `cosign`.
pub fn verify_release_manifest(
    manifest: &Path,
    bundle: &Path,
    expected: &Expected,
) -> Result<(), String> {
    let bundle_json = std::fs::read(bundle)
        .map_err(|err| format!("cannot read Sigstore bundle {}: {err}", bundle.display()))?;
    check_bundle_identity(&bundle_json, expected)?;
    let root = std::env::temp_dir().join(format!(
        "vibe-trusted-root-{}-{}.json",
        std::process::id(),
        hex::encode(rand::random::<[u8; 8]>())
    ));
    crate::fsutil::write_atomic(&root, TRUSTED_ROOT.as_bytes()).map_err(|err| err.to_string())?;
    let out = std::process::Command::new("cosign")
        .args(cosign_args(manifest, bundle, &root, expected))
        .output();
    let _ = std::fs::remove_file(&root);
    let out =
        out.map_err(|err| format!("cannot run cosign to verify the release manifest: {err}"))?;
    if !out.status.success() {
        return Err(format!(
            "Sigstore verification of the release manifest failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_exact_for_the_version() {
        assert_eq!(
            release_identity("1.8.0"),
            "https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.8.0"
        );
    }

    #[test]
    fn versions_are_restricted() {
        assert!(check_version("1.8.0").is_ok());
        assert!(check_version("1.8.0-rc.1").is_ok());
        for bad in [
            "v1.8.0",
            "1.8",
            "1.8.*",
            "1.8.0/x",
            "",
            "1.8.0-",
            "1.8.0-a/b",
        ] {
            assert!(check_version(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn trusted_root_is_compiled_in() {
        let root: serde_json::Value = serde_json::from_str(TRUSTED_ROOT).unwrap();
        assert!(root["mediaType"]
            .as_str()
            .unwrap()
            .starts_with("application/vnd.dev.sigstore.trustedroot+json"));
        assert!(!root["tlogs"].as_array().unwrap().is_empty());
        assert!(!root["certificateAuthorities"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cosign_args_pin_identity_issuer_commit_and_never_skip_the_tlog() {
        let expected = Expected {
            version: "1.8.0".into(),
            commit: "a".repeat(40),
        };
        let args = cosign_args(Path::new("m"), Path::new("b"), Path::new("r"), &expected);
        let joined = args.join(" ");
        assert!(joined.contains("--certificate-identity https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.8.0"));
        assert!(joined
            .contains("--certificate-oidc-issuer https://token.actions.githubusercontent.com"));
        assert!(joined.contains(&format!(
            "--certificate-github-workflow-sha {}",
            "a".repeat(40)
        )));
        assert!(!joined.contains("regexp"));
        assert!(!joined.contains("insecure-ignore-tlog"));
    }

    fn bundle_with(cert_der: &[u8], tlog: bool) -> Vec<u8> {
        let raw = base64::engine::general_purpose::STANDARD.encode(cert_der);
        let entries = if tlog {
            serde_json::json!([{ "integratedTime": "1", "inclusionProof": {} }])
        } else {
            serde_json::json!([])
        };
        serde_json::to_vec(&serde_json::json!({
            "verificationMaterial": { "certificate": { "rawBytes": raw }, "tlogEntries": entries }
        }))
        .unwrap()
    }

    fn utf8_der(text: &str) -> Vec<u8> {
        let mut out = vec![0x0c, text.len() as u8];
        out.extend_from_slice(text.as_bytes());
        out
    }

    fn oid(id: &str) -> Vec<u64> {
        id.split('.').map(|p| p.parse().unwrap()).collect()
    }

    fn fulcio_cert(overrides: &[(&str, &str)], san: &str) -> Vec<u8> {
        let identity = release_identity("1.8.0");
        let mut values = vec![
            (OID_ISSUER_V2, ISSUER.to_string()),
            (OID_BUILD_SIGNER_URI, identity),
            (OID_RUNNER_ENVIRONMENT, "github-hosted".into()),
            (OID_SOURCE_REPOSITORY_URI, REPOSITORY_URI.into()),
            (OID_SOURCE_REPOSITORY_DIGEST, "a".repeat(40)),
            (OID_SOURCE_REPOSITORY_REF, "refs/tags/v1.8.0".into()),
        ];
        for (o, v) in overrides {
            values.retain(|(k, _)| k != o);
            if !v.is_empty() {
                values.push((o, v.to_string()));
            }
        }
        let mut params = rcgen::CertificateParams::default();
        params.subject_alt_names = vec![rcgen::SanType::URI(san.try_into().unwrap())];
        for (o, v) in values {
            params
                .custom_extensions
                .push(rcgen::CustomExtension::from_oid_content(
                    &oid(o),
                    utf8_der(&v),
                ));
        }
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().der().to_vec()
    }

    fn expected() -> Expected {
        Expected {
            version: "1.8.0".into(),
            commit: "a".repeat(40),
        }
    }

    #[test]
    fn release_certificate_is_accepted() {
        let cert = fulcio_cert(&[], &release_identity("1.8.0"));
        check_bundle_identity(&bundle_with(&cert, true), &expected()).unwrap();
    }

    #[test]
    fn identity_must_be_exact() {
        for san in [
            "https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.8.1",
            "https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/heads/main",
            "https://github.com/vibe-backend/sdk/.github/workflows/other.yml@refs/tags/v1.8.0",
            "https://github.com/other/sdk/.github/workflows/release.yml@refs/tags/v1.8.0",
        ] {
            let cert = fulcio_cert(&[], san);
            let err = check_bundle_identity(&bundle_with(&cert, true), &expected()).unwrap_err();
            assert!(err.contains("not exactly"), "{san}: {err}");
        }
    }

    #[test]
    fn issuer_commit_runner_and_ref_are_checked() {
        let san = release_identity("1.8.0");
        let other_commit = "b".repeat(40);
        for (o, v, what) in [
            (OID_ISSUER_V2, "https://accounts.google.com", "issuer"),
            (OID_RUNNER_ENVIRONMENT, "self-hosted", "runner environment"),
            (OID_RUNNER_ENVIRONMENT, "", "runner environment"),
            (
                OID_SOURCE_REPOSITORY_DIGEST,
                other_commit.as_str(),
                "source commit",
            ),
            (OID_SOURCE_REPOSITORY_REF, "refs/heads/main", "source ref"),
            (
                OID_SOURCE_REPOSITORY_URI,
                "https://github.com/other/sdk",
                "source repository",
            ),
        ] {
            let cert = fulcio_cert(&[(o, v)], &san);
            let err = check_bundle_identity(&bundle_with(&cert, true), &expected()).unwrap_err();
            assert!(err.contains(what), "{o}={v}: {err}");
        }
    }

    #[test]
    fn bundle_without_rekor_inclusion_is_refused() {
        let expected = Expected {
            version: "1.8.0".into(),
            commit: "a".repeat(40),
        };
        let err = check_bundle_identity(&bundle_with(b"x", false), &expected).unwrap_err();
        assert!(err.contains("Rekor"), "{err}");
    }

    #[test]
    fn short_commit_and_bad_version_are_refused() {
        let bundle = bundle_with(b"x", true);
        let err = check_bundle_identity(
            &bundle,
            &Expected {
                version: "1.8.0".into(),
                commit: "abc".into(),
            },
        )
        .unwrap_err();
        assert!(err.contains("40-character"), "{err}");
        let err = check_bundle_identity(
            &bundle,
            &Expected {
                version: "1.*".into(),
                commit: "a".repeat(40),
            },
        )
        .unwrap_err();
        assert!(err.contains("invalid release version"), "{err}");
    }
}

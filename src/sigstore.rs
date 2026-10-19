//! Release manifest verification with Sigstore keyless signatures
//! (decisions 79, 92, 113, 119, 182).
//!
//! Release manifests carry no long-lived key signature. The release workflow
//! signs `manifest.json` with its GitHub OIDC identity through Sigstore
//! (Fulcio certificate, Rekor transparency log) and ships the Sigstore bundle
//! next to it. Verification is offline against the trust root compiled into
//! this binary; there is no runtime override and no fallback.
//!
//! Two layers, both required:
//! 1. [`check_bundle_identity`] (pure Rust): the certificate in the bundle must
//!    name the fixed SDK reusable signer and digest, the deployer-pinned caller
//!    workflow, repository, ref and commit, the GitHub Actions issuer and a
//!    GitHub-hosted runner; the bundle must carry a Rekor entry with an
//!    inclusion proof.
//! 2. `cosign verify-blob` with the compiled-in `trusted_root.json`: checks the
//!    certificate chain, the signature over the manifest bytes, the Rekor
//!    inclusion proof / SET and the certificate validity at the Rekor
//!    integrated time, with the same exact identity, issuer, commit and ref.

use base64::Engine;
use der::Decode;
use std::path::Path;

/// The only accepted OIDC issuer.
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// Sigstore trust root (Fulcio CA, Rekor and CT log keys), compiled in only.
pub const TRUSTED_ROOT: &str = include_str!("../assets/sigstore/trusted_root.json");

// Fulcio certificate extensions (OID 1.3.6.1.4.1.57264.1.*).
const OID_BUILD_TRIGGER: &str = "1.3.6.1.4.1.57264.1.20";
const OID_BUILD_CONFIG_URI: &str = "1.3.6.1.4.1.57264.1.18";
const OID_SOURCE_REPOSITORY_IDENTIFIER: &str = "1.3.6.1.4.1.57264.1.15";
const OID_BUILD_SIGNER_DIGEST: &str = "1.3.6.1.4.1.57264.1.10";
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
    pub pin: Pin,
    pub signer: Signer,
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

pub(crate) fn check_commit(commit: &str) -> Result<(), String> {
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

/// Deployer-owned caller identity. Owner/repo are case-sensitive: use GitHub's exact case.
#[derive(Clone, Debug)]
pub struct Pin {
    owner: String,
    repo: String,
    workflow_file: String,
    ref_spec: RefSpec,
    issuer: String,
    repository_id: Option<String>,
}
#[derive(Clone, Debug)]
pub enum RefSpec {
    TagWildcard,
    Tag(String),
}
impl Pin {
    pub fn parse(
        identity: &str,
        issuer: &str,
        repository_id: Option<&str>,
    ) -> Result<Self, String> {
        let invalid = |reason: &str| {
            format!("VIBE_EXPECT_IDENTITY is invalid: {reason} (expected https://github.com/<owner>/<repo>/.github/workflows/<file>@refs/tags/v* or @refs/tags/v<version>; matching is exact, no globs or regex)")
        };
        let rest = identity
            .strip_prefix("https://github.com/")
            .ok_or_else(|| invalid("scheme and host"))?;
        let (path, reference) = rest.split_once('@').ok_or_else(|| invalid("missing ref"))?;
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() != 5 {
            return Err(invalid("workflow path"));
        }
        let (owner, repo, file) = (parts[0], parts[1], parts[4]);
        let name = |v: &str| {
            v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        if owner.is_empty()
            || owner.len() > 39
            || owner.starts_with('-')
            || owner.ends_with('-')
            || !owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(invalid("owner"));
        }
        if repo.is_empty() || repo.len() > 100 || matches!(repo, "." | "..") || !name(repo) {
            return Err(invalid("repository"));
        }
        if parts[2] != ".github"
            || parts[3] != "workflows"
            || !name(file)
            || !(file.ends_with(".yml") || file.ends_with(".yaml"))
        {
            return Err(invalid("workflow file"));
        }
        let ref_spec = if reference == "refs/tags/v*" {
            RefSpec::TagWildcard
        } else {
            let v = reference
                .strip_prefix("refs/tags/v")
                .ok_or_else(|| invalid("tag ref"))?;
            check_version(v).map_err(|_| invalid("tag version"))?;
            RefSpec::Tag(v.into())
        };
        if issuer != ISSUER {
            return Err(format!("VIBE_EXPECT_ISSUER `{issuer}` is not supported; the only accepted issuer is {ISSUER}"));
        }
        if let Some(id) = repository_id {
            if id.is_empty()
                || id.len() > 20
                || id.starts_with('0')
                || !id.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(format!("VIBE_EXPECT_REPOSITORY_ID `{id}` is invalid: expected the numeric GitHub repository id (gh api repos/<owner>/<repo> --jq .id)"));
            }
        }
        Ok(Self {
            owner: owner.into(),
            repo: repo.into(),
            workflow_file: file.into(),
            ref_spec,
            issuer: issuer.into(),
            repository_id: repository_id.map(str::to_owned),
        })
    }
    pub fn ref_for(&self, version: &str) -> Result<String, String> {
        check_version(version)?;
        if let RefSpec::Tag(v) = &self.ref_spec {
            if v != version {
                return Err(format!(
                    "pin ref `refs/tags/v{v}` does not match VIBE_RELEASE_VERSION `{version}`"
                ));
            }
        }
        Ok(release_ref(version))
    }
    fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
    fn repository_uri(&self) -> String {
        format!("https://github.com/{}", self.repository())
    }
    pub fn identity_for(&self, version: &str) -> Result<String, String> {
        Ok(format!(
            "{}/.github/workflows/{}@{}",
            self.repository_uri(),
            self.workflow_file,
            self.ref_for(version)?
        ))
    }
}
/// SDK signer, never derived from runtime environment or artifact contents.
#[derive(Clone, Debug)]
pub struct Signer {
    version: String,
    identity: String,
    digest: String,
}
impl Signer {
    pub fn new(sdk_version: &str, digest: &str) -> Result<Self, String> {
        check_version(sdk_version)?;
        check_commit(digest)?;
        Ok(Self { version: sdk_version.into(), identity: format!("https://github.com/vibe-backend/sdk/.github/workflows/vibe-project-release.yml@refs/tags/v{sdk_version}"), digest: digest.into() })
    }
    /// SDK release workflow will set VIBE_SDK_COMMIT=${{ github.sha }} at build time
    /// for tag builds. Other binaries refuse release manifests. Use a clean build
    /// when changing the compile-time value; no runtime override exists.
    pub fn compiled() -> Result<Self, String> {
        match option_env!("VIBE_SDK_COMMIT").filter(|digest| !digest.is_empty()) {
            Some(digest) => Self::new(env!("CARGO_PKG_VERSION"), digest)
                .map_err(|reason| format!("compiled-in VIBE_SDK_COMMIT is invalid: {reason}")),
            None => Err("this vibe-runtime build has no compiled-in SDK signer commit (VIBE_SDK_COMMIT at build time); only official SDK release binaries can verify release manifests".into()),
        }
    }

    fn signer_error(&self, got: &str) -> String {
        format!("certificate signer workflow is `{got}`; this runtime accepts only manifests signed by vibe-backend/sdk vibe-project-release.yml at v{}", self.version)
    }
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
    der::asn1::Utf8StringRef::from_der(value)
        .ok()
        .map(|text| text.as_str().to_string())
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
    let cert = x509_cert::Certificate::from_der(&der)
        .map_err(|err| format!("Sigstore bundle certificate: {err}"))?;

    let identity = expected.signer.identity.clone();
    expected.pin.ref_for(&expected.version)?;
    let mut sans = Vec::new();
    if let Ok(Some((_, san))) = cert
        .tbs_certificate
        .get::<x509_cert::ext::pkix::SubjectAltName>()
    {
        for name in &san.0 {
            match name {
                x509_cert::ext::pkix::name::GeneralName::UniformResourceIdentifier(uri) => {
                    sans.push(uri.to_string())
                }
                other => sans.push(format!("{other:?}")),
            }
        }
    }
    if sans != [identity.clone()] {
        return Err(expected.signer.signer_error(&sans.join(",")));
    }

    let ext = |oid: &str, der: bool| -> Option<String> {
        cert.tbs_certificate
            .extensions
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|e| e.extn_id.to_string() == oid)
            .and_then(|e| extension_text(e.extn_value.as_bytes(), der))
    };
    match ext(OID_BUILD_SIGNER_URI, true) {
        Some(got) if got == identity => {}
        Some(got) => return Err(expected.signer.signer_error(&got)),
        None => return Err("certificate has no build signer extension".into()),
    }
    let issuer = ext(OID_ISSUER_V2, true).or_else(|| ext(OID_ISSUER_V1, false));
    let mut want = vec![
        (
            "build signer digest",
            ext(OID_BUILD_SIGNER_DIGEST, true),
            expected.signer.digest.clone(),
        ),
        ("issuer", issuer, expected.pin.issuer.clone()),
        (
            "source repository",
            ext(OID_SOURCE_REPOSITORY_URI, true),
            expected.pin.repository_uri(),
        ),
        (
            "build config",
            ext(OID_BUILD_CONFIG_URI, true),
            expected.pin.identity_for(&expected.version)?,
        ),
        (
            "source ref",
            ext(OID_SOURCE_REPOSITORY_REF, true),
            expected.pin.ref_for(&expected.version)?,
        ),
        (
            "source commit",
            ext(OID_SOURCE_REPOSITORY_DIGEST, true),
            expected.commit.clone(),
        ),
        ("build trigger", ext(OID_BUILD_TRIGGER, true), "push".into()),
        (
            "runner environment",
            ext(OID_RUNNER_ENVIRONMENT, true),
            "github-hosted".into(),
        ),
    ];
    if let Some(id) = &expected.pin.repository_id {
        want.push((
            "source repository id",
            ext(OID_SOURCE_REPOSITORY_IDENTIFIER, true),
            id.clone(),
        ));
    }
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
        expected.signer.identity.clone(),
        "--certificate-oidc-issuer".into(),
        expected.pin.issuer.clone(),
        "--certificate-github-workflow-sha".into(),
        expected.commit.clone(),
        "--certificate-github-workflow-repository".into(),
        expected.pin.repository(),
        "--certificate-github-workflow-ref".into(),
        release_ref(&expected.version),
        "--certificate-github-workflow-trigger".into(),
        "push".into(),
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
            Signer::new("1.8.0", &"b".repeat(40)).unwrap().identity,
            "https://github.com/vibe-backend/sdk/.github/workflows/vibe-project-release.yml@refs/tags/v1.8.0"
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
            pin: Pin::parse(
                "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v*",
                ISSUER,
                None,
            )
            .unwrap(),
            signer: Signer::new("1.8.0", &"b".repeat(40)).unwrap(),
        };
        let args = cosign_args(Path::new("m"), Path::new("b"), Path::new("r"), &expected);
        let joined = args.join(" ");
        assert!(joined.contains("--certificate-identity https://github.com/vibe-backend/sdk/.github/workflows/vibe-project-release.yml@refs/tags/v1.8.0"));
        assert!(joined
            .contains("--certificate-oidc-issuer https://token.actions.githubusercontent.com"));
        assert!(joined.contains(&format!(
            "--certificate-github-workflow-sha {}",
            "a".repeat(40)
        )));
        assert!(joined.contains("--certificate-github-workflow-repository acme/shop-a"));
        assert!(joined.contains("--certificate-github-workflow-ref refs/tags/v1.8.0"));
        assert!(joined.contains("--certificate-github-workflow-trigger push"));
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
        {
            use der::Encode;
            der::asn1::Utf8StringRef::new(text)
                .unwrap()
                .to_der()
                .unwrap()
        }
    }

    fn oid(id: &str) -> Vec<u64> {
        id.split('.').map(|p| p.parse().unwrap()).collect()
    }

    fn fulcio_cert(overrides: &[(&str, &str)], san: &str) -> Vec<u8> {
        let identity = Signer::new("1.8.0", &"b".repeat(40)).unwrap().identity;
        let mut values = vec![
            (OID_ISSUER_V2, ISSUER.to_string()),
            (OID_BUILD_SIGNER_URI, identity),
            (OID_RUNNER_ENVIRONMENT, "github-hosted".into()),
            (
                OID_SOURCE_REPOSITORY_URI,
                "https://github.com/acme/shop-a".into(),
            ),
            (OID_BUILD_SIGNER_DIGEST, "b".repeat(40)),
            (
                OID_BUILD_CONFIG_URI,
                "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v1.8.0"
                    .into(),
            ),
            (OID_BUILD_TRIGGER, "push".into()),
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
        params.subject_alt_names = san
            .split(',')
            .map(|v| rcgen::SanType::URI(v.try_into().unwrap()))
            .collect();
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
            pin: Pin::parse(
                "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v*",
                ISSUER,
                None,
            )
            .unwrap(),
            signer: Signer::new("1.8.0", &"b".repeat(40)).unwrap(),
        }
    }

    #[test]
    fn release_certificate_is_accepted() {
        let cert = fulcio_cert(
            &[],
            &Signer::new("1.8.0", &"b".repeat(40)).unwrap().identity,
        );
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
            assert!(err.contains("signer workflow"), "{san}: {err}");
        }
    }

    #[test]
    fn issuer_commit_runner_and_ref_are_checked() {
        let san = Signer::new("1.8.0", &"b".repeat(40)).unwrap().identity;
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
            pin: Pin::parse(
                "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v*",
                ISSUER,
                None,
            )
            .unwrap(),
            signer: Signer::new("1.8.0", &"b".repeat(40)).unwrap(),
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
                ..expected()
            },
        )
        .unwrap_err();
        assert!(err.contains("40-character"), "{err}");
        let err = check_bundle_identity(
            &bundle,
            &Expected {
                version: "1.*".into(),
                commit: "a".repeat(40),
                pin: Pin::parse(
                    "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v*",
                    ISSUER,
                    None,
                )
                .unwrap(),
                signer: Signer::new("1.8.0", &"b".repeat(40)).unwrap(),
            },
        )
        .unwrap_err();
        assert!(err.contains("invalid release version"), "{err}");
    }
    #[test]
    fn pin_grammar_and_signer_validation() {
        let base = "https://github.com/acme/shop/.github/workflows/release.yml@";
        for reference in ["refs/tags/v*", "refs/tags/v1.8.0"] {
            let p = Pin::parse(&format!("{base}{reference}"), ISSUER, Some("123")).unwrap();
            assert_eq!(p.ref_for("1.8.0").unwrap(), "refs/tags/v1.8.0");
        }
        for reference in [
            "refs/heads/main",
            "abc123",
            "refs/tags/*",
            "refs/tags/v1.*",
            "refs/tags/v**",
            "refs/tags/v1.8.0$",
            "refs/tags/v1.8.0/",
            "refs/tags/v1.8.0?x",
            "refs/tags/v1.8.0#x",
            "refs/tags/v1.8.0 ",
            "refs/tags/v1.8.0@x",
        ] {
            assert!(
                Pin::parse(&format!("{base}{reference}"), ISSUER, None).is_err(),
                "{reference}"
            );
        }
        let identity = format!("{base}refs/tags/v*");
        for bad in [
            identity.replace("https://", "http://"),
            identity.replace("github.com", "example.com"),
            identity.replace("github.com", "GITHUB.COM"),
            identity.replace("shop/", "%2e/"),
            identity.replace("release.yml", "sub/release.yml"),
            identity.replace("release.yml", "release.txt"),
            identity.replace(".github/workflows", "other/workflows"),
            identity.replace("acme/", "/"),
            identity.replace("shop/", "/"),
        ] {
            assert!(Pin::parse(&bad, ISSUER, None).is_err(), "{bad}");
        }
        assert!(Pin::parse(&identity, "other", None)
            .unwrap_err()
            .contains("not supported"));
        for id in ["0123", "abc", ""] {
            assert!(Pin::parse(&identity, ISSUER, Some(id)).is_err());
        }
        assert!(Pin::parse(&format!("{base}refs/tags/v1.8.1"), ISSUER, None)
            .unwrap()
            .ref_for("1.8.0")
            .unwrap_err()
            .contains("does not match VIBE_RELEASE_VERSION"));
        assert!(Signer::new("bad", &"a".repeat(40)).is_err());
        for digest in ["", "abc", &"A".repeat(40), &"g".repeat(40)] {
            assert!(Signer::new("1.8.0", digest).is_err());
        }
        if option_env!("VIBE_SDK_COMMIT").is_none_or(str::is_empty) {
            assert!(Signer::compiled()
                .unwrap_err()
                .contains("no compiled-in SDK signer commit"));
        }
    }
    #[test]
    fn project_b_cannot_impersonate_a_even_with_same_commit_and_version() {
        let e = expected();
        for (version, commit) in [("1.8.1", "c".repeat(40)), ("1.8.0", "a".repeat(40))] {
            let config = format!(
                "https://github.com/acme/shop-b/.github/workflows/other.yml@refs/tags/v{version}"
            );
            let reference = format!("refs/tags/v{version}");
            let cert = fulcio_cert(
                &[
                    (OID_SOURCE_REPOSITORY_URI, "https://github.com/acme/shop-b"),
                    (OID_BUILD_CONFIG_URI, &config),
                    (OID_SOURCE_REPOSITORY_REF, &reference),
                    (OID_SOURCE_REPOSITORY_DIGEST, &commit),
                ],
                &e.signer.identity,
            );
            assert!(check_bundle_identity(&bundle_with(&cert, true), &e)
                .unwrap_err()
                .contains("source repository"));
        }
    }
    #[test]
    fn caller_workflow_and_tag_must_match() {
        let e = expected();
        for (config, reference) in [
            (
                "https://github.com/acme/shop-a/.github/workflows/other.yml@refs/tags/v1.8.0",
                "refs/tags/v1.8.0",
            ),
            (
                "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v1.8.1",
                "refs/tags/v1.8.1",
            ),
        ] {
            let cert = fulcio_cert(
                &[
                    (OID_BUILD_CONFIG_URI, config),
                    (OID_SOURCE_REPOSITORY_REF, reference),
                ],
                &e.signer.identity,
            );
            let err = check_bundle_identity(&bundle_with(&cert, true), &e).unwrap_err();
            assert!(err.contains("certificate build config"), "{err}");
        }
    }

    #[test]
    fn signer_error_names_injected_sdk_version() {
        let mut e = expected();
        e.signer = Signer::new("1.8.2", &"b".repeat(40)).unwrap();
        let cert = fulcio_cert(&[], "https://github.com/acme/shop-a");
        let err = check_bundle_identity(&bundle_with(&cert, true), &e).unwrap_err();
        assert!(err.ends_with("at v1.8.2"), "{err}");
    }

    #[test]
    fn extra_and_project_sans_are_refused() {
        let e = expected();
        for san in [
            format!("{},https://github.com/acme/shop-a", e.signer.identity),
            "https://github.com/acme/shop-a/.github/workflows/release.yml@refs/tags/v1.8.0".into(),
        ] {
            let cert = fulcio_cert(&[], &san);
            assert!(check_bundle_identity(&bundle_with(&cert, true), &e)
                .unwrap_err()
                .contains("signer workflow"));
        }
    }
    #[test]
    fn all_caller_and_signer_fields_are_checked() {
        let e = expected();
        for (oid, value, field) in [
            (OID_BUILD_CONFIG_URI, "wrong", "build config"),
            (OID_BUILD_TRIGGER, "pull_request", "build trigger"),
            (OID_BUILD_SIGNER_URI, "wrong", "signer workflow"),
            (OID_BUILD_SIGNER_DIGEST, "wrong", "build signer digest"),
            (
                OID_SOURCE_REPOSITORY_URI,
                "https://github.com/acme/shop-b",
                "source repository",
            ),
        ] {
            let cert = fulcio_cert(&[(oid, value)], &e.signer.identity);
            let err = check_bundle_identity(&bundle_with(&cert, true), &e).unwrap_err();
            assert!(err.contains(field), "{err}");
        }
        for reference in ["refs/tags/v*", "refs/tags/v1.8.0"] {
            for id in [None, Some("123")] {
                let mut e = expected();
                e.pin = Pin::parse(
                    &format!(
                        "https://github.com/acme/shop-a/.github/workflows/release.yml@{reference}"
                    ),
                    ISSUER,
                    id,
                )
                .unwrap();
                let cert = fulcio_cert(
                    &[(OID_SOURCE_REPOSITORY_IDENTIFIER, "123")],
                    &e.signer.identity,
                );
                check_bundle_identity(&bundle_with(&cert, true), &e).unwrap();
                if id.is_some() {
                    let cert = fulcio_cert(
                        &[(OID_SOURCE_REPOSITORY_IDENTIFIER, "124")],
                        &e.signer.identity,
                    );
                    assert!(check_bundle_identity(&bundle_with(&cert, true), &e)
                        .unwrap_err()
                        .contains("source repository id"));
                }
            }
        }
    }
}

//! Sigstore keyless verification of release manifests (spec v1.8, decisions
//! 113, 118 and 119).
//!
//! Release manifests carry no key signature. The release job in the
//! `release` environment attests the manifest with
//! `actions/attest-build-provenance`, which signs keyless through Sigstore:
//! a GitHub Actions OIDC token, a short-lived Fulcio certificate and a Rekor
//! transparency log entry. The output is a Sigstore bundle
//! (`manifest.json.sigstore.json`).
//!
//! The deploy job verifies that bundle with `gh attestation verify`. This
//! module is the runtime check done again by `vibe serve` / `vibe migrate`:
//! it fails closed unless
//!
//! - the Sigstore public-good trust root (embedded TUF `trusted_root.json`)
//!   verifies the Fulcio certificate chain, the Rekor inclusion proof or SET,
//!   and that the certificate was valid at the integrated time,
//! - the DSSE envelope signature and in-toto subject digest match the
//!   manifest bytes,
//! - the certificate is a GitHub Actions OIDC identity for the pinned repo,
//!   the pinned release workflow and a `refs/tags/v*` ref, on a GitHub-hosted
//!   runner, with the exact SAN identity required by decision 119.

use std::sync::OnceLock;

use sigstore_trust_root::{TrustedRoot, SIGSTORE_PRODUCTION_TRUSTED_ROOT};
use sigstore_types::Bundle;
use sigstore_verify::{VerificationPolicy, Verifier};

pub const GITHUB_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
pub const DEFAULT_REPO: &str = "vibe-backend/sdk";
/// Pinned release workflow path (decision 119).
pub const DEFAULT_WORKFLOW: &str = ".github/workflows/release.yml";
/// Value stored in `Manifest::signature` of a release manifest: the
/// signature lives in the Sigstore bundle next to it.
pub const SIGSTORE_SIGNATURE: &str = "sigstore-keyless";
const SLSA_V1: &str = "https://slsa.dev/provenance/v1";

/// Who may have signed a release manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// `owner/name`.
    pub repo: String,
    /// Workflow path inside the repo, e.g. `.github/workflows/release.yml`.
    pub workflow: String,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            repo: DEFAULT_REPO.into(),
            workflow: DEFAULT_WORKFLOW.into(),
        }
    }
}

impl Policy {
    /// Compiled-in defaults. `VIBE_RELEASE_REPO` / `VIBE_RELEASE_WORKFLOW`
    /// may override them only when `VIBE_ENV` is a development value;
    /// otherwise an override is refused.
    pub fn from_env() -> Result<Self, String> {
        let repo = env_override("VIBE_RELEASE_REPO")?;
        let workflow = env_override("VIBE_RELEASE_WORKFLOW")?;
        Ok(Self {
            repo: repo.unwrap_or_else(|| DEFAULT_REPO.to_string()),
            workflow: workflow.unwrap_or_else(|| DEFAULT_WORKFLOW.to_string()),
        })
    }

    /// Expected certificate SAN / build-signer URI for `source_ref`.
    pub fn expected_signer(&self, source_ref: &str) -> String {
        format!(
            "https://github.com/{}/{}@{}",
            self.repo,
            self.workflow.trim_start_matches('/'),
            source_ref
        )
    }
}

fn env_override(name: &str) -> Result<Option<String>, String> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let run_env =
        crate::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), false);
    if !run_env.is_dev() {
        return Err(format!(
            "{name} is only accepted when VIBE_ENV=development; unset it or use the compiled-in release identity"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// The verified signer identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub repo: String,
    pub source_ref: String,
    pub commit: String,
    pub signer_workflow: String,
    pub rekor_log_index: String,
}

fn verifier() -> Result<&'static Verifier, String> {
    static VERIFIER: OnceLock<Result<Verifier, String>> = OnceLock::new();
    match VERIFIER.get_or_init(|| {
        let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)
            .map_err(|err| format!("sigstore trust root: {err}"))?;
        Verifier::new(&root).map_err(|err| format!("sigstore verifier: {err}"))
    }) {
        Ok(v) => Ok(v),
        Err(err) => Err(err.clone()),
    }
}

/// Checks Fulcio CI claims against `policy` (decision 119). Used after the
/// trust-root verification has already accepted the certificate.
#[allow(clippy::too_many_arguments)]
pub fn check_identity_claims(
    identity_san: Option<&str>,
    issuer: Option<&str>,
    source_repository_uri: Option<&str>,
    source_repository_ref: Option<&str>,
    source_repository_digest: Option<&str>,
    build_signer_uri: Option<&str>,
    runner_environment: Option<&str>,
    policy: &Policy,
) -> Result<(String, String, String), String> {
    let issuer = issuer.ok_or("sigstore certificate: missing OIDC issuer")?;
    if issuer != GITHUB_OIDC_ISSUER {
        return Err(format!(
            "sigstore certificate: issuer {issuer:?} is not GitHub Actions"
        ));
    }
    let repo_uri =
        source_repository_uri.ok_or("sigstore certificate: missing source repository")?;
    let want_repo = format!("https://github.com/{}", policy.repo);
    if repo_uri != want_repo {
        return Err(format!(
            "sigstore certificate: repository {repo_uri} is not {want_repo}"
        ));
    }
    let source_ref = source_repository_ref.ok_or("sigstore certificate: missing source ref")?;
    if source_ref
        .strip_prefix("refs/tags/v")
        .is_none_or(str::is_empty)
    {
        return Err(format!(
            "sigstore certificate: ref {source_ref} is not a refs/tags/v* release tag"
        ));
    }
    let want_signer = policy.expected_signer(source_ref);
    let signer = build_signer_uri.ok_or("sigstore certificate: missing build signer")?;
    if signer != want_signer {
        return Err(format!(
            "sigstore certificate: signer workflow {signer} is not {want_signer}"
        ));
    }
    match identity_san {
        Some(san) if san == want_signer => {}
        Some(san) => {
            return Err(format!(
                "sigstore certificate: SAN {san} does not name the signer workflow {want_signer}"
            ))
        }
        None => return Err("sigstore certificate: SAN does not name the signer workflow".into()),
    }
    let runner = runner_environment.ok_or("sigstore certificate: missing runner environment")?;
    if runner != "github-hosted" {
        return Err(format!(
            "sigstore certificate: runner {runner:?} is not github-hosted"
        ));
    }
    let commit = source_repository_digest.ok_or("sigstore certificate: missing source commit")?;
    if commit.is_empty() {
        return Err("sigstore certificate: source commit is empty".into());
    }
    Ok((
        source_ref.to_string(),
        commit.to_string(),
        signer.to_string(),
    ))
}

/// Verifies `bundle_json` for `manifest_bytes` against `policy`.
pub fn verify_bundle(
    manifest_bytes: &[u8],
    bundle_json: &[u8],
    policy: &Policy,
) -> Result<Identity, String> {
    let text = std::str::from_utf8(bundle_json)
        .map_err(|_| "sigstore bundle: not valid UTF-8".to_string())?;
    let bundle = Bundle::from_json(text).map_err(|err| format!("sigstore bundle: {err}"))?;

    // Cryptographic verification against the embedded public-good trust root:
    // Fulcio chain, SCT, Rekor inclusion proof / SET, certificate validity at
    // integrated time, DSSE signature, and subject digest of the manifest.
    let crypto_policy =
        VerificationPolicy::any_identity().require_issuer(GITHUB_OIDC_ISSUER.to_string());
    let result = verifier()?
        .verify(manifest_bytes, &bundle, &crypto_policy)
        .map_err(|err| format!("sigstore bundle: {err}"))?;
    if !result.certificate_verified() || !result.tlog_verified() {
        return Err(
            "sigstore bundle: certificate chain or Rekor transparency log was not verified".into(),
        );
    }

    let cert = result
        .certificate()
        .ok_or("sigstore bundle: no signing certificate (keyless signing required)")?;
    let claims = &cert.ci_claims;
    let san = result.identity().map(|s| s.as_str());
    let (source_ref, commit, signer) = check_identity_claims(
        san,
        result.issuer().or(cert.issuer.as_deref()),
        claims.source_repository_uri.as_deref(),
        claims.source_repository_ref.as_deref(),
        claims.source_repository_digest.as_deref(),
        claims.build_signer_uri.as_deref(),
        claims.runner_environment.as_deref(),
        policy,
    )?;

    // Require SLSA provenance v1 (actions/attest-build-provenance).
    let statement = match &bundle.content {
        sigstore_types::SignatureContent::DsseEnvelope(env) => {
            serde_json::from_slice::<serde_json::Value>(env.payload.as_bytes())
                .map_err(|err| format!("sigstore bundle: statement json: {err}"))?
        }
        _ => return Err("sigstore bundle: no DSSE envelope (an attestation is required)".into()),
    };
    if statement["predicateType"].as_str() != Some(SLSA_V1) {
        return Err("sigstore bundle: predicate is not SLSA provenance v1".into());
    }

    let entry = bundle
        .verification_material
        .tlog_entries
        .first()
        .ok_or("sigstore bundle: no Rekor transparency log entry")?;

    Ok(Identity {
        repo: policy.repo.clone(),
        source_ref,
        commit,
        signer_workflow: signer,
        rekor_log_index: entry.log_index.to_string(),
    })
}

/// Default bundle path for a manifest: `<manifest>.sigstore.json`.
pub fn default_bundle_path(manifest: &std::path::Path) -> std::path::PathBuf {
    let mut name = manifest.as_os_str().to_owned();
    name.push(".sigstore.json");
    name.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_claims_require_the_pinned_release_workflow() {
        let policy = Policy::default();
        let source_ref = "refs/tags/v1.8.0";
        let signer = policy.expected_signer(source_ref);
        assert_eq!(
            signer,
            "https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.8.0"
        );
        let (got_ref, commit, got_signer) = check_identity_claims(
            Some(&signer),
            Some(GITHUB_OIDC_ISSUER),
            Some("https://github.com/vibe-backend/sdk"),
            Some(source_ref),
            Some("abc123"),
            Some(&signer),
            Some("github-hosted"),
            &policy,
        )
        .unwrap();
        assert_eq!(got_ref, source_ref);
        assert_eq!(commit, "abc123");
        assert_eq!(got_signer, signer);
    }

    #[test]
    fn identity_claims_refuse_the_wrong_repo_or_workflow() {
        let policy = Policy::default();
        let err = check_identity_claims(
            Some("https://github.com/attacker/sdk/.github/workflows/release.yml@refs/tags/v1.0.0"),
            Some(GITHUB_OIDC_ISSUER),
            Some("https://github.com/attacker/sdk"),
            Some("refs/tags/v1.0.0"),
            Some("abc"),
            Some("https://github.com/attacker/sdk/.github/workflows/release.yml@refs/tags/v1.0.0"),
            Some("github-hosted"),
            &policy,
        )
        .unwrap_err();
        assert!(err.contains("repository"), "{err}");

        let signer =
            "https://github.com/vibe-backend/sdk/.github/workflows/ci.yml@refs/tags/v1.0.0";
        let err = check_identity_claims(
            Some(signer),
            Some(GITHUB_OIDC_ISSUER),
            Some("https://github.com/vibe-backend/sdk"),
            Some("refs/tags/v1.0.0"),
            Some("abc"),
            Some(signer),
            Some("github-hosted"),
            &policy,
        )
        .unwrap_err();
        assert!(err.contains("signer workflow"), "{err}");
    }
}

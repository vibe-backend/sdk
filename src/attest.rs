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
//!   verifies the Fulcio certificate chain, the Rekor inclusion proof and SET
//!   (or checkpoint), and that the certificate was valid at the integrated
//!   time,
//! - the bundle media type is v0.2 or v0.3 (v0.1 is refused),
//! - the DSSE envelope signature and in-toto subject digest match the
//!   manifest bytes,
//! - the certificate identity equals the deployer-expected
//!   `https://github.com/<repo>/<workflow>@refs/tags/v<version>` (decision
//!   119), with matching ref and commit extensions, issuer, and a
//!   GitHub-hosted runner.
//!
//! Relaxed identity configuration (repo/workflow overrides, and serving a
//! development-key manifest without a bundle) exists only in debug builds and
//! requires an explicit `VIBE_ENV=development` (or the debug-only
//! `--insecure-dev` opt-in when `VIBE_ENV` is unset). Release builds always
//! require Sigstore verification.

use std::sync::OnceLock;

use sigstore_trust_root::{TrustedRoot, SIGSTORE_PRODUCTION_TRUSTED_ROOT};
use sigstore_types::{Bundle, MediaType, Sha256Hash};
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
    /// Compiled-in defaults. Overrides are refused unless
    /// [`relaxed_config_allowed`] is true.
    pub fn from_env() -> Result<Self, String> {
        let repo = env_override("VIBE_RELEASE_REPO")?;
        let workflow = env_override("VIBE_RELEASE_WORKFLOW")?;
        Ok(Self {
            repo: repo.unwrap_or_else(|| DEFAULT_REPO.to_string()),
            workflow: workflow.unwrap_or_else(|| DEFAULT_WORKFLOW.to_string()),
        })
    }

    /// Expected certificate SAN / build-signer URI for a release version.
    pub fn expected_signer(&self, version: &str) -> String {
        format!(
            "https://github.com/{}/{}@{}",
            self.repo,
            self.workflow.trim_start_matches('/'),
            expectation_source_ref(version)
        )
    }
}

/// Deployer-expected release version and commit. Both must match the
/// manifest provenance and the Fulcio certificate claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expectation {
    /// Version without a leading `v`, e.g. `1.8.0`.
    pub version: String,
    pub commit: String,
}

impl Expectation {
    /// Normalizes `version` (strips a leading `v`) and rejects empty fields.
    pub fn new(version: &str, commit: &str) -> Result<Self, String> {
        let version = normalize_version(version)?;
        let commit = commit.trim().to_string();
        if commit.is_empty() {
            return Err("release commit is empty".into());
        }
        Ok(Self { version, commit })
    }

    pub fn source_ref(&self) -> String {
        expectation_source_ref(&self.version)
    }
}

fn normalize_version(raw: &str) -> Result<String, String> {
    let v = raw.trim().trim_start_matches('v').trim().to_string();
    if v.is_empty() {
        return Err("release version is empty".into());
    }
    if v.contains('/') || v.contains('@') {
        return Err(format!("release version {raw:?} is not a bare version"));
    }
    Ok(v)
}

/// Release version named by the certificate itself: the tag of
/// `source_ref` (`refs/tags/v<version>`), which the SAN must end with. The
/// result comes only from the certificate; callers compare it with the
/// deployer's expected version, which comes from deployer config or the CLI.
pub fn attested_version(san: &str, source_ref: &str) -> Result<String, String> {
    let tag = source_ref
        .strip_prefix("refs/tags/v")
        .ok_or_else(|| format!("sigstore certificate: ref {source_ref} is not a v<version> tag"))?;
    let version = normalize_version(tag)
        .map_err(|err| format!("sigstore certificate: ref {source_ref}: {err}"))?;
    if version != tag || !san.ends_with(&format!("@{source_ref}")) {
        return Err(format!(
            "sigstore certificate: SAN {san} does not name the tag {source_ref}"
        ));
    }
    Ok(version)
}

fn expectation_source_ref(version: &str) -> String {
    format!("refs/tags/v{}", version.trim().trim_start_matches('v'))
}

/// Relaxed configuration (identity overrides, development-key serve without a
/// bundle). Release builds never allow it. Debug builds allow it when
/// `VIBE_ENV` is explicitly a development value, or when `VIBE_ENV` is unset
/// and `insecure_dev` was opted in.
#[cfg(debug_assertions)]
pub fn relaxed_config_allowed(insecure_dev: bool) -> bool {
    let raw = std::env::var("VIBE_ENV").ok();
    let unset = raw.as_deref().is_none_or(|v| v.trim().is_empty());
    let explicit_dev = crate::runtime::RunEnv::from_var(raw.as_deref(), false).is_dev();
    explicit_dev || (unset && insecure_dev)
}

/// Relaxed configuration is compiled out of release builds.
#[cfg(not(debug_assertions))]
pub fn relaxed_config_allowed(_insecure_dev: bool) -> bool {
    false
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
    if !relaxed_config_allowed(insecure_dev_from_env()) {
        return Err(format!(
            "{name} is only accepted by a debug build with VIBE_ENV=development; unset it or use the compiled-in release identity"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

#[cfg(debug_assertions)]
fn insecure_dev_from_env() -> bool {
    matches!(
        std::env::var("VIBE_INSECURE_DEV").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

#[cfg(not(debug_assertions))]
fn insecure_dev_from_env() -> bool {
    false
}

/// The verified signer identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub repo: String,
    pub source_ref: String,
    pub commit: String,
    pub version: String,
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

/// Checks Fulcio CI claims against `policy` and the deployer `expected`
/// version/commit (decision 119).
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
    expected: &Expectation,
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
    let want_ref = expected.source_ref();
    if source_ref != want_ref {
        return Err(format!(
            "sigstore certificate: ref {source_ref} is not the expected {want_ref}"
        ));
    }
    let want_signer = policy.expected_signer(&expected.version);
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
    let san = identity_san.unwrap_or_default();
    let attested = attested_version(san, source_ref)?;
    if attested != expected.version {
        return Err(format!(
            "sigstore certificate: release version {attested} is not the expected {}",
            expected.version
        ));
    }
    let runner = runner_environment.ok_or("sigstore certificate: missing runner environment")?;
    if runner != "github-hosted" {
        return Err(format!(
            "sigstore certificate: runner {runner:?} is not github-hosted"
        ));
    }
    let commit = source_repository_digest.ok_or("sigstore certificate: missing source commit")?;
    if commit != expected.commit {
        return Err(format!(
            "sigstore certificate: commit {commit} is not the expected {}",
            expected.commit
        ));
    }
    Ok((
        source_ref.to_string(),
        commit.to_string(),
        signer.to_string(),
    ))
}

fn require_bundle_media_type(bundle: &Bundle) -> Result<(), String> {
    match bundle.media_type() {
        MediaType::Bundle0_3 | MediaType::Bundle0_2 => Ok(()),
        MediaType::Bundle0_1 => Err(
            "sigstore bundle: media type version 0.1 is not accepted; use a v0.2 or v0.3 bundle"
                .into(),
        ),
        other => Err(format!(
            "sigstore bundle: unsupported media type {}",
            other.as_str()
        )),
    }
}

/// Exactly one Rekor entry is accepted: every entry a bundle carries must be
/// the one that is checked, so extra entries are rejected rather than ignored.
fn single_tlog_entry(
    bundle: &Bundle,
) -> Result<&sigstore_types::bundle::TransparencyLogEntry, String> {
    match bundle.verification_material.tlog_entries.as_slice() {
        [entry] => Ok(entry),
        [] => Err("sigstore bundle: no Rekor transparency log entry".into()),
        entries => Err(format!(
            "sigstore bundle: expected exactly one Rekor transparency log entry, found {}",
            entries.len()
        )),
    }
}

fn require_inclusion_and_set(bundle: &Bundle) -> Result<(), String> {
    let entry = single_tlog_entry(bundle)?;
    let proof = entry
        .inclusion_proof
        .as_ref()
        .ok_or("sigstore bundle: Rekor entry has no inclusion proof")?;
    let has_set = entry.inclusion_promise.is_some();
    let has_checkpoint = !proof.checkpoint.is_empty();
    if !has_set && !has_checkpoint {
        return Err(
            "sigstore bundle: Rekor entry has no signed entry timestamp or checkpoint".into(),
        );
    }
    Ok(())
}

fn finish_identity(
    result: &sigstore_verify::VerificationResult,
    policy: &Policy,
    expected: &Expectation,
    rekor_log_index: String,
) -> Result<Identity, String> {
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
        expected,
    )?;
    let version = attested_version(&signer, &source_ref)?;
    Ok(Identity {
        repo: policy.repo.clone(),
        source_ref,
        commit,
        version,
        signer_workflow: signer,
        rekor_log_index,
    })
}

fn verify_parsed_bundle<'a>(
    artifact: impl Into<sigstore_types::Artifact<'a>>,
    bundle: &Bundle,
    policy: &Policy,
    expected: &Expectation,
    require_slsa_v1: bool,
) -> Result<Identity, String> {
    require_bundle_media_type(bundle)?;
    require_inclusion_and_set(bundle)?;

    let crypto_policy =
        VerificationPolicy::any_identity().require_issuer(GITHUB_OIDC_ISSUER.to_string());
    let result = verifier()?
        .verify(artifact, bundle, &crypto_policy)
        .map_err(|err| format!("sigstore bundle: {err}"))?;
    if !result.certificate_verified() || !result.tlog_verified() {
        return Err(
            "sigstore bundle: certificate chain or Rekor transparency log was not verified".into(),
        );
    }

    if require_slsa_v1 {
        let statement = match &bundle.content {
            sigstore_types::SignatureContent::DsseEnvelope(env) => {
                serde_json::from_slice::<serde_json::Value>(env.payload.as_bytes())
                    .map_err(|err| format!("sigstore bundle: statement json: {err}"))?
            }
            _ => {
                return Err("sigstore bundle: no DSSE envelope (an attestation is required)".into())
            }
        };
        let pred = statement["predicateType"].as_str().unwrap_or("");
        if pred != SLSA_V1 && pred != "https://slsa.dev/spec/v1.0/provenance" {
            return Err("sigstore bundle: predicate is not SLSA provenance v1".into());
        }
    }

    let entry = single_tlog_entry(bundle)?;
    finish_identity(&result, policy, expected, entry.log_index.to_string())
}

/// Verifies `bundle_json` for `manifest_bytes` against `policy` and the
/// deployer `expected` version/commit.
pub fn verify_bundle(
    manifest_bytes: &[u8],
    bundle_json: &[u8],
    policy: &Policy,
    expected: &Expectation,
) -> Result<Identity, String> {
    let text = std::str::from_utf8(bundle_json)
        .map_err(|_| "sigstore bundle: not valid UTF-8".to_string())?;
    let bundle = Bundle::from_json(text).map_err(|err| format!("sigstore bundle: {err}"))?;
    verify_parsed_bundle(manifest_bytes, &bundle, policy, expected, true)
}

/// Like [`verify_bundle`], but the artifact is identified by a SHA-256 hex
/// digest (for fixtures where the original bytes are not shipped).
pub fn verify_bundle_digest(
    sha256_hex: &str,
    bundle_json: &[u8],
    policy: &Policy,
    expected: &Expectation,
    require_slsa_v1: bool,
) -> Result<Identity, String> {
    let digest = Sha256Hash::from_hex(sha256_hex.trim())
        .map_err(|err| format!("sigstore bundle: subject digest: {err}"))?;
    let text = std::str::from_utf8(bundle_json)
        .map_err(|_| "sigstore bundle: not valid UTF-8".to_string())?;
    let bundle = Bundle::from_json(text).map_err(|err| format!("sigstore bundle: {err}"))?;
    verify_parsed_bundle(digest, &bundle, policy, expected, require_slsa_v1)
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
    fn identity_claims_require_the_deployer_version_and_commit() {
        let policy = Policy::default();
        let expected = Expectation::new("1.8.0", "abc123").unwrap();
        let signer = policy.expected_signer(&expected.version);
        assert_eq!(
            signer,
            "https://github.com/vibe-backend/sdk/.github/workflows/release.yml@refs/tags/v1.8.0"
        );
        let (got_ref, commit, got_signer) = check_identity_claims(
            Some(&signer),
            Some(GITHUB_OIDC_ISSUER),
            Some("https://github.com/vibe-backend/sdk"),
            Some(&expected.source_ref()),
            Some("abc123"),
            Some(&signer),
            Some("github-hosted"),
            &policy,
            &expected,
        )
        .unwrap();
        assert_eq!(got_ref, "refs/tags/v1.8.0");
        assert_eq!(commit, "abc123");
        assert_eq!(got_signer, signer);
    }

    #[test]
    fn identity_claims_refuse_a_ref_or_commit_that_is_not_the_expected() {
        let policy = Policy::default();
        let expected = Expectation::new("1.8.0", "abc123").unwrap();
        let signer = policy.expected_signer("1.8.0");
        let err = check_identity_claims(
            Some(&signer),
            Some(GITHUB_OIDC_ISSUER),
            Some("https://github.com/vibe-backend/sdk"),
            Some("refs/tags/v9.9.9"),
            Some("abc123"),
            Some(&signer),
            Some("github-hosted"),
            &policy,
            &expected,
        )
        .unwrap_err();
        assert!(err.contains("is not the expected"), "{err}");

        let err = check_identity_claims(
            Some(&signer),
            Some(GITHUB_OIDC_ISSUER),
            Some("https://github.com/vibe-backend/sdk"),
            Some("refs/tags/v1.8.0"),
            Some("deadbeef"),
            Some(&signer),
            Some("github-hosted"),
            &policy,
            &expected,
        )
        .unwrap_err();
        assert!(err.contains("is not the expected"), "{err}");
    }
}

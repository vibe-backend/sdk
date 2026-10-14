//! Sigstore keyless verification of release manifests (spec v1.8, decisions
//! 113 and 118).
//!
//! Release manifests carry no key signature. The release job in the
//! `release` environment attests the manifest with
//! `actions/attest-build-provenance`, which signs keyless through Sigstore:
//! a GitHub Actions OIDC token, a short-lived Fulcio certificate and a Rekor
//! transparency log entry. The output is a Sigstore bundle
//! (`manifest.json.sigstore.json`).
//!
//! The deploy job verifies that bundle cryptographically with
//! `gh attestation verify` (Fulcio chain, Rekor inclusion, identity). This
//! module is the runtime check done again by `vibe serve` / `vibe migrate`:
//! it fails closed unless
//!
//! - the DSSE envelope signature verifies with the bundle's certificate key,
//! - the in-toto subject digest is the SHA-256 of the manifest bytes,
//! - the certificate is a GitHub Actions OIDC identity for the pinned repo,
//!   the pinned release workflow and a `refs/tags/v*` ref, on a GitHub-hosted
//!   runner,
//! - a Rekor entry is present, carries an inclusion proof or promise, was
//!   integrated while the certificate was valid, and records this envelope's
//!   payload and signature.
//!
//! Not done here (see `docs/ci/sigstore-release.md`): the Fulcio chain to the
//! Sigstore trust root and the Rekor checkpoint/SET signatures. Those need
//! the Sigstore TUF trust root and stay in the deploy job until the runtime
//! adopts sigstore-rs.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const GITHUB_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
pub const DEFAULT_REPO: &str = "vibe-backend/sdk";
pub const DEFAULT_WORKFLOW: &str = ".github/workflows/vibe-release.yml";
/// Value stored in `Manifest::signature` of a release manifest: the
/// signature lives in the Sigstore bundle next to it.
pub const SIGSTORE_SIGNATURE: &str = "sigstore-keyless";
const SLSA_V1: &str = "https://slsa.dev/provenance/v1";
const IN_TOTO: &str = "application/vnd.in-toto+json";

// Fulcio certificate extensions (v2, DER UTF8String values).
const OID_ISSUER: &str = "1.3.6.1.4.1.57264.1.8";
const OID_BUILD_SIGNER_URI: &str = "1.3.6.1.4.1.57264.1.9";
const OID_RUNNER_ENV: &str = "1.3.6.1.4.1.57264.1.11";
const OID_SOURCE_REPO_URI: &str = "1.3.6.1.4.1.57264.1.12";
const OID_SOURCE_DIGEST: &str = "1.3.6.1.4.1.57264.1.13";
const OID_SOURCE_REF: &str = "1.3.6.1.4.1.57264.1.14";

/// Who may have signed a release manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// `owner/name`.
    pub repo: String,
    /// Workflow path inside the repo, e.g. `.github/workflows/vibe-release.yml`.
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
    /// `VIBE_RELEASE_REPO` / `VIBE_RELEASE_WORKFLOW` from the runtime
    /// configuration, else the defaults.
    pub fn from_env() -> Self {
        let get = |name: &str, default: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        Self {
            repo: get("VIBE_RELEASE_REPO", DEFAULT_REPO),
            workflow: get("VIBE_RELEASE_WORKFLOW", DEFAULT_WORKFLOW),
        }
    }
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bundle {
    #[serde(default)]
    media_type: String,
    verification_material: Material,
    dsse_envelope: Option<Envelope>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Material {
    certificate: Option<RawBytes>,
    x509_certificate_chain: Option<Chain>,
    #[serde(default)]
    tlog_entries: Vec<TlogEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBytes {
    raw_bytes: String,
}

#[derive(Deserialize)]
struct Chain {
    #[serde(default)]
    certificates: Vec<RawBytes>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TlogEntry {
    #[serde(default)]
    log_index: String,
    #[serde(default)]
    integrated_time: String,
    inclusion_promise: Option<serde_json::Value>,
    inclusion_proof: Option<serde_json::Value>,
    #[serde(default)]
    canonicalized_body: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    payload: String,
    payload_type: String,
    #[serde(default)]
    signatures: Vec<EnvelopeSig>,
}

#[derive(Deserialize)]
struct EnvelopeSig {
    sig: String,
}

fn b64(what: &str, text: &str) -> Result<Vec<u8>, String> {
    B64.decode(text.trim())
        .map_err(|_| format!("sigstore bundle: {what} is not base64"))
}

/// DSSE pre-authentication encoding.
pub fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "DSSEv1 {} {} {} ",
        payload_type.len(),
        payload_type,
        payload.len()
    )
    .into_bytes();
    out.extend_from_slice(payload);
    out
}

/// Decodes a DER UTF8String (Fulcio v2 extension value).
fn der_utf8(value: &[u8]) -> Option<String> {
    let (&tag, rest) = value.split_first()?;
    if tag != 0x0c {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (len, body) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | *b as usize);
        (len, &rest[n..])
    };
    if body.len() != len {
        return None;
    }
    String::from_utf8(body.to_vec()).ok()
}

/// Verifies `bundle_json` for `manifest_bytes` against `policy`.
pub fn verify_bundle(
    manifest_bytes: &[u8],
    bundle_json: &[u8],
    policy: &Policy,
) -> Result<Identity, String> {
    let bundle: Bundle = serde_json::from_slice(bundle_json)
        .map_err(|err| format!("sigstore bundle json: {err}"))?;
    if !bundle
        .media_type
        .starts_with("application/vnd.dev.sigstore.bundle")
    {
        return Err("sigstore bundle: unexpected mediaType".into());
    }
    let envelope = bundle
        .dsse_envelope
        .ok_or("sigstore bundle: no DSSE envelope (an attestation is required)")?;
    let material = bundle.verification_material;

    // Certificate (bundle v0.3 `certificate`, v0.1/0.2 chain leaf).
    let cert_b64 = match (&material.certificate, &material.x509_certificate_chain) {
        (Some(cert), _) => cert.raw_bytes.clone(),
        (None, Some(chain)) => chain
            .certificates
            .first()
            .map(|c| c.raw_bytes.clone())
            .ok_or("sigstore bundle: empty certificate chain")?,
        _ => {
            return Err("sigstore bundle: no signing certificate (keyless signing required)".into())
        }
    };
    let cert_der = b64("certificate", &cert_b64)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&cert_der)
        .map_err(|err| format!("sigstore bundle: certificate: {err}"))?;

    // 1. Envelope signature with the certificate key.
    let payload = b64("DSSE payload", &envelope.payload)?;
    if envelope.payload_type != IN_TOTO {
        return Err(format!(
            "sigstore bundle: payload type {:?} is not in-toto",
            envelope.payload_type
        ));
    }
    let [sig] = envelope.signatures.as_slice() else {
        return Err("sigstore bundle: expected exactly one DSSE signature".into());
    };
    let sig_bytes = b64("DSSE signature", &sig.sig)?;
    let spki = cert.public_key().raw;
    let key =
        <p256::ecdsa::VerifyingKey as p256::pkcs8::DecodePublicKey>::from_public_key_der(spki)
            .map_err(|_| "sigstore bundle: certificate key is not ECDSA P-256".to_string())?;
    let signature = p256::ecdsa::Signature::from_der(&sig_bytes)
        .map_err(|_| "sigstore bundle: DSSE signature is malformed".to_string())?;
    p256::ecdsa::signature::Verifier::verify(
        &key,
        &pae(&envelope.payload_type, &payload),
        &signature,
    )
    .map_err(|_| "sigstore bundle: DSSE signature does not verify".to_string())?;

    // 2. Subject digest is this manifest.
    let statement: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|err| format!("sigstore bundle: statement json: {err}"))?;
    if statement["predicateType"] != SLSA_V1 {
        return Err("sigstore bundle: predicate is not SLSA provenance v1".into());
    }
    let digest = hex::encode(Sha256::digest(manifest_bytes));
    let subjects = statement["subject"].as_array().cloned().unwrap_or_default();
    if !subjects
        .iter()
        .any(|s| s["digest"]["sha256"].as_str() == Some(digest.as_str()))
    {
        return Err(format!(
            "sigstore bundle: no subject matches the manifest digest sha256:{digest}"
        ));
    }

    // 3. Certificate identity.
    let ext = |oid: &str| -> Option<String> {
        cert.extensions()
            .iter()
            .find(|e| e.oid.to_id_string() == oid)
            .and_then(|e| der_utf8(e.value))
    };
    let need = |oid: &str, what: &str| {
        ext(oid).ok_or_else(|| format!("sigstore certificate: missing {what}"))
    };
    let issuer = need(OID_ISSUER, "OIDC issuer")?;
    if issuer != GITHUB_OIDC_ISSUER {
        return Err(format!(
            "sigstore certificate: issuer {issuer:?} is not GitHub Actions"
        ));
    }
    let repo_uri = need(OID_SOURCE_REPO_URI, "source repository")?;
    let want_repo = format!("https://github.com/{}", policy.repo);
    if repo_uri != want_repo {
        return Err(format!(
            "sigstore certificate: repository {repo_uri} is not {want_repo}"
        ));
    }
    let source_ref = need(OID_SOURCE_REF, "source ref")?;
    if source_ref
        .strip_prefix("refs/tags/v")
        .is_none_or(str::is_empty)
    {
        return Err(format!(
            "sigstore certificate: ref {source_ref} is not a refs/tags/v* release tag"
        ));
    }
    let signer = need(OID_BUILD_SIGNER_URI, "build signer")?;
    let want_signer = format!(
        "https://github.com/{}/{}@{}",
        policy.repo,
        policy.workflow.trim_start_matches('/'),
        source_ref
    );
    if signer != want_signer {
        return Err(format!(
            "sigstore certificate: signer workflow {signer} is not {want_signer}"
        ));
    }
    let san_ok = cert
        .subject_alternative_name()
        .ok()
        .flatten()
        .is_some_and(|san| {
            san.value.general_names.iter().any(
                |n| matches!(n, x509_parser::extensions::GeneralName::URI(uri) if *uri == signer),
            )
        });
    if !san_ok {
        return Err("sigstore certificate: SAN does not name the signer workflow".into());
    }
    let runner = need(OID_RUNNER_ENV, "runner environment")?;
    if runner != "github-hosted" {
        return Err(format!(
            "sigstore certificate: runner {runner:?} is not github-hosted"
        ));
    }
    let commit = need(OID_SOURCE_DIGEST, "source commit")?;

    // 4. Rekor transparency log entry for this envelope.
    let entry = material
        .tlog_entries
        .first()
        .ok_or("sigstore bundle: no Rekor transparency log entry")?;
    if entry.log_index.trim().is_empty()
        || (entry.inclusion_proof.is_none() && entry.inclusion_promise.is_none())
    {
        return Err("sigstore bundle: Rekor entry has no inclusion proof or promise".into());
    }
    let integrated: i64 = entry
        .integrated_time
        .trim()
        .parse()
        .map_err(|_| "sigstore bundle: Rekor entry has no integrated time".to_string())?;
    let validity = cert.validity();
    if integrated < validity.not_before.timestamp() || integrated > validity.not_after.timestamp() {
        return Err(
            "sigstore bundle: Rekor entry was not integrated while the certificate was valid"
                .into(),
        );
    }
    let body: serde_json::Value =
        serde_json::from_slice(&b64("Rekor entry body", &entry.canonicalized_body)?)
            .map_err(|_| "sigstore bundle: Rekor entry body is not json".to_string())?;
    let payload_hash = hex::encode(Sha256::digest(&payload));
    let recorded_sig = body["spec"]["signatures"][0]["signature"]
        .as_str()
        .and_then(|s| B64.decode(s).ok());
    if body["kind"] != "dsse"
        || body["spec"]["payloadHash"]["value"].as_str() != Some(payload_hash.as_str())
        || recorded_sig.as_deref() != Some(sig_bytes.as_slice())
    {
        return Err("sigstore bundle: Rekor entry does not record this attestation".into());
    }

    Ok(Identity {
        repo: policy.repo.clone(),
        source_ref,
        commit,
        signer_workflow: signer,
        rekor_log_index: entry.log_index.clone(),
    })
}

/// Default bundle path for a manifest: `<manifest>.sigstore.json`.
pub fn default_bundle_path(manifest: &std::path::Path) -> std::path::PathBuf {
    let mut name = manifest.as_os_str().to_owned();
    name.push(".sigstore.json");
    name.into()
}

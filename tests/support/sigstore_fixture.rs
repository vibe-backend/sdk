//! Builds Sigstore bundles shaped like `actions/attest-build-provenance`
//! output (bundle v0.3, DSSE, Fulcio-style certificate, Rekor dsse entry),
//! signed with a throwaway P-256 key. Only for tests.

#![allow(dead_code)]

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::pkcs8::EncodePrivateKey;
use sha2::{Digest, Sha256};

pub struct Fixture {
    pub repo: String,
    pub workflow: String,
    pub source_ref: String,
    pub commit: String,
    pub issuer: String,
    pub runner: String,
    pub tlog: bool,
    pub inclusion: bool,
    /// Sign a different envelope than the one in the bundle.
    pub tamper_payload: bool,
    /// Rekor entry records another payload.
    pub foreign_rekor_entry: bool,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            repo: "vibe-backend/sdk".into(),
            workflow: ".github/workflows/release.yml".into(),
            source_ref: "refs/tags/v1.0.0".into(),
            commit: "abc123".into(),
            issuer: "https://token.actions.githubusercontent.com".into(),
            runner: "github-hosted".into(),
            tlog: true,
            inclusion: true,
            tamper_payload: false,
            foreign_rekor_entry: false,
        }
    }
}

fn utf8(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = vec![0x0c];
    if bytes.len() < 0x80 {
        out.push(bytes.len() as u8);
    } else {
        out.push(0x81);
        out.push(bytes.len() as u8);
    }
    out.extend_from_slice(bytes);
    out
}

fn ext(last: u64, value: &str) -> rcgen::CustomExtension {
    rcgen::CustomExtension::from_oid_content(&[1, 3, 6, 1, 4, 1, 57264, 1, last], utf8(value))
}

fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
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

impl Fixture {
    /// The bundle JSON for `manifest_bytes`.
    pub fn bundle(&self, manifest_bytes: &[u8]) -> Vec<u8> {
        let signing = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        let der = signing.to_pkcs8_der().unwrap();
        let key_pair = rcgen::KeyPair::try_from(der.as_bytes()).unwrap();
        let signer = format!(
            "https://github.com/{}/{}@{}",
            self.repo, self.workflow, self.source_ref
        );
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = vec![rcgen::SanType::URI(
            rcgen::Ia5String::try_from(signer.clone()).unwrap(),
        )];
        params.not_before = rcgen::date_time_ymd(2026, 1, 1);
        params.not_after = rcgen::date_time_ymd(2027, 1, 1);
        params.custom_extensions = vec![
            ext(8, &self.issuer),
            ext(9, &signer),
            ext(11, &self.runner),
            ext(12, &format!("https://github.com/{}", self.repo)),
            ext(13, &self.commit),
            ext(14, &self.source_ref),
        ];
        let cert = params.self_signed(&key_pair).unwrap();

        let digest = hex::encode(Sha256::digest(manifest_bytes));
        let statement = serde_json::json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{"name": "manifest.json", "digest": {"sha256": digest}}],
            "predicateType": "https://slsa.dev/provenance/v1",
            "predicate": {"buildDefinition": {}, "runDetails": {}},
        });
        let payload = serde_json::to_vec(&statement).unwrap();
        let payload_type = "application/vnd.in-toto+json";
        let signed_payload = if self.tamper_payload {
            b"something else".to_vec()
        } else {
            payload.clone()
        };
        let sig: p256::ecdsa::Signature = signing.sign(&pae(payload_type, &signed_payload));
        let sig = sig.to_der().as_bytes().to_vec();

        let recorded = if self.foreign_rekor_entry {
            hex::encode(Sha256::digest(b"another attestation"))
        } else {
            hex::encode(Sha256::digest(&payload))
        };
        let body = serde_json::json!({
            "apiVersion": "0.0.1",
            "kind": "dsse",
            "spec": {
                "envelopeHash": {"algorithm": "sha256", "value": "00"},
                "payloadHash": {"algorithm": "sha256", "value": recorded},
                "signatures": [{"signature": B64.encode(&sig), "verifier": ""}],
            },
        });
        let mut entry = serde_json::json!({
            "logIndex": "123456",
            "logId": {"keyId": "wNI9atQGlz+VWfO6LRygH4QUfY/8W4RFwiT5i5WRgB0="},
            "kindVersion": {"kind": "dsse", "version": "0.0.1"},
            "integratedTime": "1790000000",
            "canonicalizedBody": B64.encode(serde_json::to_vec(&body).unwrap()),
        });
        if self.inclusion {
            entry["inclusionPromise"] = serde_json::json!({"signedEntryTimestamp": "AA=="});
            entry["inclusionProof"] = serde_json::json!({"logIndex": "123", "treeSize": "200"});
        }
        let tlog = if self.tlog { vec![entry] } else { vec![] };
        serde_json::to_vec_pretty(&serde_json::json!({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "verificationMaterial": {
                "tlogEntries": tlog,
                "timestampVerificationData": {},
                "certificate": {"rawBytes": B64.encode(cert.der())},
            },
            "dsseEnvelope": {
                "payload": B64.encode(&payload),
                "payloadType": payload_type,
                "signatures": [{"sig": B64.encode(&sig), "keyid": ""}],
            },
        }))
        .unwrap()
    }
}

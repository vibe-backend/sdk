//! ES256 private keys are checked when they are loaded (#257), for both PEM
//! and DER, instead of failing at the first signature.

use base64::Engine;
use vibe::runtime::{ec_signing_key_from_der, ec_signing_key_from_pem, load_signing_keys};

const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

/// P-256 group order n.
const N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
];

fn der() -> Vec<u8> {
    let b64: String = PRIVATE_PEM
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap()
}

fn to_pem(der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let body: Vec<String> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| String::from_utf8(c.to_vec()).unwrap())
        .collect();
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        body.join("\n")
    )
}

/// Replaces the 32-byte private scalar inside the PKCS#8 structure.
fn with_d(d: [u8; 32]) -> Vec<u8> {
    let mut der = der();
    let marker = [0x02, 0x01, 0x01, 0x04, 0x20];
    let at = der.windows(marker.len()).position(|w| w == marker).unwrap() + marker.len();
    der[at..at + 32].copy_from_slice(&d);
    der
}

fn with_trailing() -> Vec<u8> {
    let mut der = der();
    der.extend_from_slice(&[0, 0]);
    der
}

fn above_n() -> [u8; 32] {
    let mut d = N;
    d[31] += 1;
    d
}

fn bad_keys() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("trailing bytes", with_trailing()),
        ("d = 0", with_d([0; 32])),
        ("d = n", with_d(N)),
        ("d > n", with_d(above_n())),
        ("d = 2^256 - 1", with_d([0xff; 32])),
    ]
}

#[test]
fn valid_key_loads_from_pem_and_der() {
    ec_signing_key_from_pem(PRIVATE_PEM.as_bytes()).unwrap();
    ec_signing_key_from_der(&der()).unwrap();
    ec_signing_key_from_pem(to_pem(&der()).as_bytes()).unwrap();
}

#[test]
fn invalid_der_keys_are_rejected_at_load() {
    for (name, der) in bad_keys() {
        assert!(
            ec_signing_key_from_der(&der).is_err(),
            "DER {name} accepted"
        );
    }
}

#[test]
fn invalid_pem_keys_are_rejected_at_load() {
    for (name, der) in bad_keys() {
        let pem = to_pem(&der);
        assert!(
            ec_signing_key_from_pem(pem.as_bytes()).is_err(),
            "PEM {name} accepted"
        );
    }
}

#[test]
fn startup_refuses_invalid_ec_key() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/auth-app/manifest.json"
    );
    let manifest = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for (name, der) in bad_keys() {
        let pem = to_pem(&der);
        let res = load_signing_keys(&manifest, &|_| Some(pem.clone()), "local");
        assert!(res.is_err(), "startup accepted {name}");
    }
    load_signing_keys(&manifest, &|_| Some(PRIVATE_PEM.to_string()), "local").unwrap();
}

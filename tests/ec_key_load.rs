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
    ec_signing_key_from_pem(PRIVATE_PEM.replace('\n', "\\n").as_bytes()).unwrap();
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

#[test]
fn pem_with_data_outside_the_block_is_rejected() {
    let trailing = format!("{PRIVATE_PEM}extra");
    assert!(ec_signing_key_from_pem(trailing.as_bytes()).is_err());
    let leading = format!("extra\n{PRIVATE_PEM}");
    assert!(ec_signing_key_from_pem(leading.as_bytes()).is_err());
}

#[test]
fn truncated_der_is_rejected() {
    let der = der();
    assert!(ec_signing_key_from_der(&der[..der.len() - 1]).is_err());
    assert!(ec_signing_key_from_der(&[]).is_err());
}

fn decode_pem(pem: &str) -> Vec<u8> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .unwrap()
}

#[test]
fn sec1_is_rejected_and_pkcs8_of_the_same_key_loads() {
    // Extract the ECPrivateKey from this PKCS#8 and add its named curve.
    let pkcs8 = der();
    let mut sec1 = pkcs8[29..].to_vec();
    assert_eq!(&sec1[..7], &[0x30, 0x6b, 2, 1, 1, 4, 32]);
    let curve = [0xa0, 10, 6, 8, 0x2a, 0x86, 0x48, 0xce, 0x3d, 3, 1, 7];
    sec1.splice(39..39, curve);
    sec1[1] += 12;
    let pem = to_pem(&sec1).replace("PRIVATE KEY", "EC PRIVATE KEY");
    ec_signing_key_from_pem(PRIVATE_PEM.as_bytes()).unwrap();
    assert_eq!(
        ec_signing_key_from_pem(pem.as_bytes()).err().unwrap(),
        "SEC1 EC PRIVATE KEY is not supported; use PKCS#8 PRIVATE KEY"
    );
    ec_signing_key_from_der(&pkcs8).unwrap();
    let expected = "SEC1 EC PRIVATE KEY is not supported; use PKCS#8 PRIVATE KEY";
    // Single-line environment values are normalized before checking the label.
    assert_eq!(
        ec_signing_key_from_pem(pem.replace('\n', "\\n").as_bytes())
            .err()
            .unwrap(),
        expected
    );
    // A PKCS#8 label cannot make a bare SEC1 structure acceptable.
    assert_eq!(
        ec_signing_key_from_pem(to_pem(&sec1).as_bytes())
            .err()
            .unwrap(),
        expected
    );
    assert_eq!(ec_signing_key_from_der(&sec1).err().unwrap(), expected);
}

#[test]
fn wrong_curve_and_off_curve_private_public_points_are_refused() {
    let p384 = include_str!("../src/runtime/testdata/ec-p384-k.pem");
    assert!(ec_signing_key_from_pem(p384.as_bytes()).is_err());
    assert!(ec_signing_key_from_der(&decode_pem(p384)).is_err());
    let sec1 = include_str!("../src/runtime/testdata/ec-p384-sec1.pem");
    assert!(ec_signing_key_from_pem(sec1.as_bytes()).is_err());
    assert!(ec_signing_key_from_der(&decode_pem(sec1)).is_err());
    let mut off_curve = der();
    *off_curve.last_mut().unwrap() ^= 1;
    assert!(ec_signing_key_from_pem(to_pem(&off_curve).as_bytes()).is_err());
    assert!(ec_signing_key_from_der(&off_curve).is_err());
}

#[test]
fn invalid_pem_errors_never_disclose_decoder_details() {
    for body in ["ABCD!EFG", "ABCDéEFG", "MIGHAgE"] {
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n");
        let err = ec_signing_key_from_pem(pem.as_bytes()).err().unwrap();
        assert_eq!(err, "invalid PEM encoding");
        for secret in [body, "ABCD", "EFG", "MIGH", "!", "é"] {
            assert!(!err.contains(secret));
        }
        assert!(!err.chars().any(|c| c.is_ascii_digit()));
    }
    for pem in ["-----BEGIN PRIVATE KEY-----\nAAAA", "not PEM"] {
        assert_eq!(
            ec_signing_key_from_pem(pem.as_bytes()).err().unwrap(),
            "invalid PEM encoding"
        );
    }
}

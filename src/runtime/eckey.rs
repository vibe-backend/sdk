//! EC key curve checks (decision 139, spec §4.1a): ES256 keys must be on
//! P-256. Static verification keys (`VIBE_JWT_PUBLIC_KEY_PEM`) and ES256
//! signing keys are checked at startup; other curves refuse to start.
//! These hand-written curve/point checks are defense-in-depth only, reporting
//! errors early at startup. Authoritative key parsing and verification are
//! performed by jsonwebtoken (aws_lc_rs backend). JWKS public keys are also checked
//! when loaded.
//!
//! Reads SubjectPublicKeyInfo (`PUBLIC KEY`), PKCS#8 (`PRIVATE KEY`) and
//! SEC1 (`EC PRIVATE KEY`) PEMs. Public points must be uncompressed and lie
//! on the curve.

use super::rsakey::{algorithm_identifier, pem_der, tlv, whole};

/// id-ecPublicKey (1.2.840.10045.2.1).
const EC_PUBLIC_KEY_OID: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
/// prime256v1 / P-256 (1.2.840.10045.3.1.7).
const P256_OID: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

/// Curve check failure; never carries key material.
#[derive(Debug, PartialEq, Eq)]
pub enum EcProblem {
    NotEc,
    InvalidPem,
    OtherCurve,
    BadPoint,
    /// Private scalar d is not in `1..n-1`.
    BadScalar,
    /// Bytes left over inside the key encoding.
    TrailingData,
}

/// `Ok` when `pem` is a P-256 public key with a valid point.
pub fn p256_public(pem: &str) -> Result<(), EcProblem> {
    let (label, der) = pem_der(pem).ok_or(EcProblem::InvalidPem)?;
    if label != "PUBLIC KEY" {
        return Err(EcProblem::NotEc);
    }
    // SEQUENCE { AlgorithmIdentifier, BIT STRING }
    let seq = whole(&der, 0x30).ok_or(EcProblem::NotEc)?;
    let (alg, rest) = tlv(seq, 0x30).ok_or(EcProblem::NotEc)?;
    curve_of_alg(alg)?;
    let bits = whole(rest, 0x03).ok_or(EcProblem::NotEc)?;
    match bits.split_first() {
        Some((0, point)) => check_point(point),
        _ => Err(EcProblem::NotEc),
    }
}

/// Validates a PKCS#8 or SEC1 PEM and returns PKCS#8 DER for the signing backend.
/// SEC1 is wrapped only after validation; no second private-key parser is needed.
pub fn p256_private(pem: &str) -> Result<Vec<u8>, EcProblem> {
    let (label, der) = pem_der(pem).ok_or(EcProblem::InvalidPem)?;
    match label.as_str() {
        "PRIVATE KEY" => {
            p256_private_der(&der)?;
            Ok(der)
        }
        "EC PRIVATE KEY" => {
            sec1_private(&der, false)?;
            let mut body = vec![2, 1, 0];
            let mut alg = der_element(0x06, EC_PUBLIC_KEY_OID);
            alg.extend(der_element(0x06, P256_OID));
            body.extend(der_element(0x30, &alg));
            body.extend(der_element(0x04, &der));
            Ok(der_element(0x30, &body))
        }
        _ => Err(EcProblem::NotEc),
    }
}

fn der_element(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if body.len() < 128 {
        out.push(body.len() as u8);
    } else {
        let len = (body.len() as u32).to_be_bytes();
        let first = len.iter().position(|b| *b != 0).unwrap();
        out.push(0x80 | (4 - first) as u8);
        out.extend_from_slice(&len[first..]);
    }
    out.extend_from_slice(body);
    out
}

/// Checks a PKCS#8 DER private key, sharing all checks with the PEM path.
pub fn p256_private_der(der: &[u8]) -> Result<(), EcProblem> {
    // SEQUENCE { INTEGER version, AlgorithmIdentifier, OCTET STRING, ... }
    let seq = whole(der, 0x30).ok_or(EcProblem::NotEc)?;
    let (_ver, rest) = tlv(seq, 0x02).ok_or(EcProblem::NotEc)?;
    let (alg, rest) = tlv(rest, 0x30).ok_or(EcProblem::NotEc)?;
    curve_of_alg(alg)?;
    let (key, mut rest) = tlv(rest, 0x04).ok_or(EcProblem::NotEc)?;
    // Optional attributes and a public point (implicit [1] in RFC 5958).
    if let Some((_, more)) = tlv(rest, 0xa0) {
        rest = more;
    }
    if let Some((bits, more)) = tlv(rest, 0x81) {
        check_public_bits(bits)?;
        rest = more;
    } else if let Some((public, more)) = tlv(rest, 0xa1) {
        check_public_bits(whole(public, 0x03).ok_or(EcProblem::NotEc)?)?;
        rest = more;
    }
    if !rest.is_empty() {
        return Err(EcProblem::TrailingData);
    }
    sec1_private(key, true)
}

/// AlgorithmIdentifier must be id-ecPublicKey with the P-256 named curve.
fn curve_of_alg(alg: &[u8]) -> Result<(), EcProblem> {
    let (oid, params) = algorithm_identifier(alg).ok_or(EcProblem::NotEc)?;
    if oid != EC_PUBLIC_KEY_OID {
        return Err(EcProblem::NotEc);
    }
    match whole(params, 0x06) {
        Some(curve) if curve == P256_OID => Ok(()),
        _ => Err(EcProblem::OtherCurve),
    }
}

/// ECPrivateKey: SEQUENCE { INTEGER 1, OCTET STRING d, [0] curve OPTIONAL,
/// [1] publicKey OPTIONAL }. The [0] curve is required unless the
/// surrounding PKCS#8 already named it.
fn sec1_private(der: &[u8], curve_known: bool) -> Result<(), EcProblem> {
    let seq = whole(der, 0x30).ok_or(EcProblem::NotEc)?;
    let (version, rest) = tlv(seq, 0x02).ok_or(EcProblem::NotEc)?;
    if version != [1] {
        return Err(EcProblem::NotEc);
    }
    let (d, mut rest) = tlv(rest, 0x04).ok_or(EcProblem::NotEc)?;
    if d.len() != 32 {
        return Err(EcProblem::OtherCurve);
    }
    // 0 < d < n.
    let scalar = from_be(d);
    if scalar == [0; 4] || !less(&scalar, &N) {
        return Err(EcProblem::BadScalar);
    }
    let mut curve_seen = curve_known;
    if let Some((params, more)) = tlv(rest, 0xa0) {
        match whole(params, 0x06) {
            Some(curve) if curve == P256_OID => curve_seen = true,
            _ => return Err(EcProblem::OtherCurve),
        }
        rest = more;
    }
    if !curve_seen {
        return Err(EcProblem::OtherCurve);
    }
    if let Some((public, more)) = tlv(rest, 0xa1) {
        rest = more;
        let bits = whole(public, 0x03).ok_or(EcProblem::NotEc)?;
        match bits.split_first() {
            Some((0, point)) => check_point(point)?,
            _ => return Err(EcProblem::NotEc),
        }
    }
    if !rest.is_empty() {
        return Err(EcProblem::TrailingData);
    }
    Ok(())
}

fn check_public_bits(bits: &[u8]) -> Result<(), EcProblem> {
    match bits.split_first() {
        Some((0, point)) => check_point(point),
        _ => Err(EcProblem::BadPoint),
    }
}

// --- P-256 point validation -------------------------------------------------

type U256 = [u64; 4]; // little-endian limbs

const P: U256 = [
    0xffff_ffff_ffff_ffff,
    0x0000_0000_ffff_ffff,
    0x0000_0000_0000_0000,
    0xffff_ffff_0000_0001,
];
/// P-256 group order n.
const N: U256 = [
    0xf3b9_cac2_fc63_2551,
    0xbce6_faad_a717_9e84,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_0000_0000,
];
const B: U256 = [
    0x3bce_3c3e_27d2_604b,
    0x651d_06b0_cc53_b0f6,
    0xb3eb_bd55_7698_86bc,
    0x5ac6_35d8_aa3a_93e7,
];

/// Uncompressed SEC1 point (0x04 || x || y) with x, y < p and
/// y² = x³ − 3x + b (mod p).
fn check_point(point: &[u8]) -> Result<(), EcProblem> {
    if point.len() != 65 || point[0] != 0x04 {
        return Err(EcProblem::BadPoint);
    }
    let x = from_be(&point[1..33]);
    let y = from_be(&point[33..65]);
    if !less(&x, &P) || !less(&y, &P) {
        return Err(EcProblem::BadPoint);
    }
    let lhs = mul_mod(&y, &y);
    let x3 = mul_mod(&mul_mod(&x, &x), &x);
    let three_x = add_mod(&add_mod(&x, &x), &x);
    let rhs = add_mod(&sub_mod(&x3, &three_x), &B);
    if lhs == rhs {
        Ok(())
    } else {
        Err(EcProblem::BadPoint)
    }
}

fn from_be(bytes: &[u8]) -> U256 {
    let mut out = [0u64; 4];
    for (i, chunk) in bytes.rchunks(8).enumerate() {
        out[i] = chunk.iter().fold(0u64, |a, b| (a << 8) | *b as u64);
    }
    out
}

fn less(a: &U256, b: &U256) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}

/// a − b, wrapping; returns the borrow.
fn sub_raw(a: &U256, b: &U256) -> (U256, bool) {
    let mut out = [0u64; 4];
    let mut borrow = false;
    for i in 0..4 {
        let (d, b1) = a[i].overflowing_sub(b[i]);
        let (d, b2) = d.overflowing_sub(borrow as u64);
        out[i] = d;
        borrow = b1 || b2;
    }
    (out, borrow)
}

/// a + b, wrapping; returns the carry.
fn add_raw(a: &U256, b: &U256) -> (U256, bool) {
    let mut out = [0u64; 4];
    let mut carry = false;
    for i in 0..4 {
        let (s, c1) = a[i].overflowing_add(b[i]);
        let (s, c2) = s.overflowing_add(carry as u64);
        out[i] = s;
        carry = c1 || c2;
    }
    (out, carry)
}

fn add_mod(a: &U256, b: &U256) -> U256 {
    let (s, carry) = add_raw(a, b);
    if carry || !less(&s, &P) {
        sub_raw(&s, &P).0
    } else {
        s
    }
}

fn sub_mod(a: &U256, b: &U256) -> U256 {
    let (d, borrow) = sub_raw(a, b);
    if borrow {
        add_raw(&d, &P).0
    } else {
        d
    }
}

/// a · b mod p: schoolbook product, then bitwise reduction (startup only).
fn mul_mod(a: &U256, b: &U256) -> U256 {
    let mut wide = [0u64; 8];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let t = wide[i + j] as u128 + a[i] as u128 * b[j] as u128 + carry;
            wide[i + j] = t as u64;
            carry = t >> 64;
        }
        wide[i + 4] = carry as u64;
    }
    let mut r = [0u64; 4];
    for bit in (0..512).rev() {
        // r = 2r + bit (mod p); r < p so 2r fits after one subtraction.
        let top = r[3] >> 63;
        for i in (1..4).rev() {
            r[i] = (r[i] << 1) | (r[i - 1] >> 63);
        }
        r[0] = (r[0] << 1) | ((wide[bit / 64] >> (bit % 64)) & 1);
        if top == 1 || !less(&r, &P) {
            r = sub_raw(&r, &P).0;
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const P256_PUB: &str = include_str!("testdata/ec-p256-p.pem");
    const P256_SEC1: &str = include_str!("testdata/ec-p256-sec1.pem");
    const P384_PUB: &str = include_str!("testdata/ec-p384-p.pem");
    const P384_PKCS8: &str = include_str!("testdata/ec-p384-k.pem");
    const P384_SEC1: &str = include_str!("testdata/ec-p384-sec1.pem");

    #[test]
    fn p256_keys_are_accepted() {
        assert_eq!(p256_public(P256_PUB), Ok(()));
        assert_eq!(p256_private(P256_SEC1).map(|_| ()), Ok(()));
    }

    #[test]
    fn other_curves_are_refused() {
        assert_eq!(p256_public(P384_PUB), Err(EcProblem::OtherCurve));
        assert_eq!(p256_private(P384_PKCS8), Err(EcProblem::OtherCurve));
        assert_eq!(p256_private(P384_SEC1), Err(EcProblem::OtherCurve));
        assert_eq!(
            p256_public(include_str!("testdata/rsa-p2048.pem")),
            Err(EcProblem::NotEc)
        );
        assert_eq!(p256_public("garbage"), Err(EcProblem::InvalidPem));
    }

    #[test]
    fn points_off_the_curve_are_refused() {
        use base64::Engine;
        let (label, mut der) = pem_der(P256_PUB).unwrap();
        let last = der.len() - 1;
        der[last] ^= 1;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let pem = format!("-----BEGIN {label}-----\n{b64}\n-----END {label}-----\n");
        assert_eq!(p256_public(&pem), Err(EcProblem::BadPoint));
    }

    const P256_PKCS8: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63,
        0x25, 0x51,
    ];

    fn pem(label: &str, der: &[u8]) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(der);
        format!("-----BEGIN {label}-----\n{b64}\n-----END {label}-----\n")
    }

    /// The PEM with its 32-byte private scalar replaced by `d`.
    fn with_d(src: &str, d: &[u8; 32]) -> String {
        let (label, mut der) = pem_der(src).unwrap();
        let marker = [0x02, 0x01, 0x01, 0x04, 0x20];
        let at = der.windows(5).position(|w| w == marker).unwrap() + 5;
        der[at..at + 32].copy_from_slice(d);
        pem(&label, &der)
    }

    fn scalars() -> Vec<(&'static str, [u8; 32], bool)> {
        let mut above = ORDER;
        above[31] += 1;
        let mut below = ORDER;
        below[31] -= 1;
        let mut one = [0u8; 32];
        one[31] = 1;
        vec![
            ("d = 0", [0; 32], false),
            ("d = n", ORDER, false),
            ("d = n + 1", above, false),
            ("d = 2^256 - 1", [0xff; 32], false),
            ("d = 1", one, true),
            ("d = n - 1", below, true),
        ]
    }

    #[test]
    fn private_scalar_must_be_in_range() {
        assert_eq!(p256_private(P256_PKCS8).map(|_| ()), Ok(()));
        for src in [P256_PKCS8, P256_SEC1] {
            for (name, d, ok) in scalars() {
                let res = p256_private(&with_d(src, &d)).map(|_| ());
                if ok {
                    assert_eq!(res, Ok(()), "{name}");
                } else {
                    assert_eq!(res, Err(EcProblem::BadScalar), "{name}");
                }
            }
        }
    }

    #[test]
    fn trailing_data_in_private_keys_is_refused() {
        for src in [P256_PKCS8, P256_SEC1] {
            let (label, der) = pem_der(src).unwrap();
            // After the outer structure.
            let mut outer = der.clone();
            outer.extend_from_slice(&[0x05, 0x00]);
            assert_eq!(p256_private(&pem(&label, &outer)), Err(EcProblem::NotEc));
            // Inside the outer SEQUENCE (lengths fixed up).
            let mut inner = der.clone();
            let len_at = if der[1] == 0x81 { 2 } else { 1 };
            assert!(inner[len_at] < 0xfe);
            inner[len_at] += 2;
            inner.extend_from_slice(&[0x05, 0x00]);
            assert_eq!(
                p256_private(&pem(&label, &inner)),
                Err(EcProblem::TrailingData),
                "{label}"
            );
        }
    }

    #[test]
    fn private_public_points_are_checked_in_all_containers() {
        for src in [P256_PKCS8, P256_SEC1] {
            let (label, mut der) = pem_der(src).unwrap();
            *der.last_mut().unwrap() ^= 1;
            assert_eq!(p256_private(&pem(&label, &der)), Err(EcProblem::BadPoint));
        }
        // RFC 5958 outer publicKey, implicit [1], must also be on P-256.
        let (_, der) = pem_der(P256_PKCS8).unwrap();
        let mut body = whole(&der, 0x30).unwrap().to_vec();
        let mut bits = vec![0, 4];
        bits.extend([0; 64]);
        body.extend(der_element(0x81, &bits));
        assert_eq!(
            p256_private_der(&der_element(0x30, &body)),
            Err(EcProblem::BadPoint)
        );
    }

    #[test]
    fn invalid_pem_encoding_has_a_fixed_problem() {
        for body in ["ABCD!EFG", "ABCDéEFG", "MIGHAgE"] {
            let pem = format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----");
            assert_eq!(p256_private(&pem), Err(EcProblem::InvalidPem));
            assert_eq!(p256_public(&pem), Err(EcProblem::InvalidPem));
        }
    }

    #[test]
    fn field_arithmetic_matches_known_values() {
        let one = [1, 0, 0, 0];
        let p_minus_1 = sub_raw(&P, &one).0;
        // (p-1)^2 = 1 mod p
        assert_eq!(mul_mod(&p_minus_1, &p_minus_1), one);
        assert_eq!(add_mod(&p_minus_1, &one), [0; 4]);
        assert_eq!(sub_mod(&[0; 4], &one), p_minus_1);
    }
}

//! RSA key size from a PEM (decision 139, spec §4.1a): RS256 signing keys and
//! static verification keys must have a modulus of at least 2048 bits.
//!
//! Reads the modulus from PKCS#1 (`RSA PUBLIC KEY` / `RSA PRIVATE KEY`),
//! SubjectPublicKeyInfo (`PUBLIC KEY`) and PKCS#8 (`PRIVATE KEY`) PEMs.

use base64::Engine;

/// Smallest accepted RSA modulus, in bits.
pub const MIN_RSA_BITS: usize = 2048;

/// rsaEncryption (1.2.840.113549.1.1.1) as a DER OBJECT IDENTIFIER body.
const RSA_ENCRYPTION_OID: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

/// The first PEM block: (label, DER bytes). Escaped `\\n` newlines (as in
/// env values) are accepted.
pub(crate) fn pem_der(pem: &str) -> Option<(String, Vec<u8>)> {
    let pem = pem.replace("\\n", "\n");
    let mut label = None;
    let mut body = String::new();
    for line in pem.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            label = Some(rest.trim_end_matches('-').to_string());
            body.clear();
        } else if line.starts_with("-----END ") {
            break;
        } else if label.is_some() {
            body.push_str(line);
        }
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .ok()?;
    Some((label?, der))
}

/// The single top-level DER element with `tag`; trailing bytes after it
/// make the input unreadable.
pub(crate) fn whole(der: &[u8], tag: u8) -> Option<&[u8]> {
    let (inner, rest) = tlv(der, tag)?;
    rest.is_empty().then_some(inner)
}

/// AlgorithmIdentifier: SEQUENCE { OID algorithm, parameters }. Returns
/// (algorithm OID body, raw parameters).
pub(crate) fn algorithm_identifier(alg: &[u8]) -> Option<(&[u8], &[u8])> {
    let (oid, params) = tlv(alg, 0x06)?;
    Some((oid, params))
}

/// Modulus size in bits, or `None` when the PEM is not a readable RSA key.
///
/// The DER is checked strictly: no trailing bytes after the outer
/// structure, and for SubjectPublicKeyInfo / PKCS#8 the AlgorithmIdentifier
/// must be rsaEncryption.
pub fn rsa_pem_bits(pem: &str) -> Option<usize> {
    let (label, der) = pem_der(pem)?;
    match label.as_str() {
        "RSA PUBLIC KEY" => pkcs1_public_bits(&der),
        "RSA PRIVATE KEY" => pkcs1_private_bits(&der),
        "PUBLIC KEY" => {
            // SEQUENCE { AlgorithmIdentifier, BIT STRING }
            let seq = whole(&der, 0x30)?;
            let (alg, rest) = tlv(seq, 0x30)?;
            if algorithm_identifier(alg)?.0 != RSA_ENCRYPTION_OID {
                return None;
            }
            let bits = whole(rest, 0x03)?;
            let (unused, key) = bits.split_first()?;
            if *unused != 0 {
                return None;
            }
            pkcs1_public_bits(key)
        }
        "PRIVATE KEY" => {
            // SEQUENCE { INTEGER version, AlgorithmIdentifier, OCTET STRING, ... }
            let seq = whole(&der, 0x30)?;
            let (_ver, rest) = tlv(seq, 0x02)?;
            let (alg, rest) = tlv(rest, 0x30)?;
            if algorithm_identifier(alg)?.0 != RSA_ENCRYPTION_OID {
                return None;
            }
            let (key, _attributes) = tlv(rest, 0x04)?;
            pkcs1_private_bits(key)
        }
        _ => None,
    }
}

/// `Err` with a message naming `what` when the RSA key is under 2048 bits or
/// unreadable. Never includes key material.
pub fn check_rsa_pem(what: &str, pem: &str) -> Result<(), String> {
    match rsa_pem_bits(pem) {
        Some(bits) if bits >= MIN_RSA_BITS => Ok(()),
        Some(bits) => Err(format!(
            "{what}: RS256 key must be at least {MIN_RSA_BITS} bits (got {bits})"
        )),
        None => Err(format!("{what}: not a readable RSA key")),
    }
}

/// RSAPublicKey: SEQUENCE { INTEGER n, INTEGER e }
fn pkcs1_public_bits(der: &[u8]) -> Option<usize> {
    let seq = whole(der, 0x30)?;
    let (n, _) = tlv(seq, 0x02)?;
    int_bits(n)
}

/// RSAPrivateKey: SEQUENCE { INTEGER version, INTEGER n, ... }
fn pkcs1_private_bits(der: &[u8]) -> Option<usize> {
    let seq = whole(der, 0x30)?;
    let (_ver, rest) = tlv(seq, 0x02)?;
    let (n, _) = tlv(rest, 0x02)?;
    int_bits(n)
}

fn int_bits(bytes: &[u8]) -> Option<usize> {
    let first = bytes.iter().position(|b| *b != 0)?;
    Some((bytes.len() - first) * 8 - bytes[first].leading_zeros() as usize)
}

/// One DER element with the expected tag: (contents, remainder).
pub(crate) fn tlv(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&t, rest) = input.split_first()?;
    if t != tag {
        return None;
    }
    let (&len0, mut rest) = rest.split_first()?;
    let len = if len0 < 0x80 {
        len0 as usize
    } else {
        let n = (len0 & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let mut len = 0usize;
        for b in &rest[..n] {
            len = (len << 8) | *b as usize;
        }
        rest = &rest[n..];
        len
    };
    if rest.len() < len {
        return None;
    }
    Some((&rest[..len], &rest[len..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const PRIV_1024: &str = include_str!("testdata/rsa-k1024.pem");
    pub const PRIV_2048: &str = include_str!("testdata/rsa-k2048.pem");
    pub const PUB_1024: &str = include_str!("testdata/rsa-p1024.pem");
    pub const PUB_2048: &str = include_str!("testdata/rsa-p2048.pem");
    const PKCS1_PRIV_1024: &str = include_str!("testdata/rsa-t1024.pem");
    const PKCS1_PUB_1024: &str = include_str!("testdata/rsa-r1024.pem");

    #[test]
    fn reads_modulus_size_from_every_pem_form() {
        assert_eq!(rsa_pem_bits(PRIV_1024), Some(1024));
        assert_eq!(rsa_pem_bits(PRIV_2048), Some(2048));
        assert_eq!(rsa_pem_bits(PUB_1024), Some(1024));
        assert_eq!(rsa_pem_bits(PUB_2048), Some(2048));
        assert_eq!(rsa_pem_bits(PKCS1_PRIV_1024), Some(1024));
        assert_eq!(rsa_pem_bits(PKCS1_PUB_1024), Some(1024));
        // Escaped newlines (as in env values) are accepted.
        assert_eq!(rsa_pem_bits(&PUB_2048.replace('\n', "\\n")), Some(2048));
        assert_eq!(rsa_pem_bits("not a pem"), None);
    }

    #[test]
    fn short_keys_fail_closed_without_printing_the_key() {
        assert!(check_rsa_pem("K", PRIV_2048).is_ok());
        assert!(check_rsa_pem("K", PUB_2048).is_ok());
        let err = check_rsa_pem("K", PRIV_1024).unwrap_err();
        assert!(err.contains("at least 2048 bits"), "{err}");
        assert!(!err.contains("MII"), "{err}");
        assert!(check_rsa_pem("K", PUB_1024).is_err());
        assert!(check_rsa_pem("K", "garbage").is_err());
    }

    fn reencode(label: &str, der: &[u8]) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(der);
        format!("-----BEGIN {label}-----\n{b64}\n-----END {label}-----\n")
    }

    #[test]
    fn trailing_bytes_and_other_algorithms_are_unreadable() {
        let (label, mut der) = pem_der(PUB_2048).unwrap();
        der.push(0);
        assert_eq!(rsa_pem_bits(&reencode(&label, &der)), None);
        let (label, mut der) = pem_der(PRIV_2048).unwrap();
        der.extend([0x05, 0x00]);
        assert_eq!(rsa_pem_bits(&reencode(&label, &der)), None);
        // An EC SubjectPublicKeyInfo is not read as RSA.
        assert_eq!(rsa_pem_bits(include_str!("testdata/ec-p256-p.pem")), None);
        assert_eq!(rsa_pem_bits(include_str!("testdata/ec-p384-k.pem")), None);
    }

    #[test]
    fn modulus_bits_ignore_leading_zeros() {
        // 2047-bit value with a DER sign byte: 0x00 0x40 ...
        let mut n = vec![0u8, 0x40];
        n.extend(vec![1u8; 255]);
        assert_eq!(int_bits(&n), Some(2047));
        let mut n = vec![0u8, 0x80];
        n.extend(vec![1u8; 255]);
        assert_eq!(int_bits(&n), Some(2048));
        assert_eq!(int_bits(&[0, 0]), None);
    }
}

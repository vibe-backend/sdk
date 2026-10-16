use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use zeroize::Zeroizing;

pub fn sign_bytes(bytes: &[u8], key: &SigningKey) -> String {
    hex::encode(key.sign(bytes).to_bytes())
}

pub fn verify_bytes(bytes: &[u8], signature_hex: &str, key: &VerifyingKey) -> bool {
    let Ok(raw) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&raw) else {
        return false;
    };
    // Reject weak keys and non-canonical / malleable signatures.
    key.verify_strict(bytes, &signature).is_ok()
}

pub fn generate_key() -> SigningKey {
    let mut seed = Zeroizing::new([0u8; 32]);
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, seed.as_mut())
        .expect("OS random number generator failed");
    SigningKey::from_bytes(&seed)
}

const DEV_KEY: &str = "signing.key";
const DEV_PUB: &str = "signing.pub";

fn decode_key(hex_key: &str, what: &str) -> Result<Zeroizing<[u8; 32]>, String> {
    let bytes =
        Zeroizing::new(hex::decode(hex_key.trim()).map_err(|_| format!("{what} is not hex"))?);
    if bytes.len() != 32 {
        return Err(format!("{what} must be 32 bytes"));
    }
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// `vibe keygen`: creates a development key pair in `<dir>/.vibe`. The private
/// key is created with mode 0600 and an existing key is never overwritten.
pub fn create_dev_key(dir: &std::path::Path) -> Result<SigningKey, String> {
    let vibe = dir.join(".vibe");
    std::fs::create_dir_all(&vibe).map_err(|err| err.to_string())?;
    let path = vibe.join(DEV_KEY);
    let key = generate_key();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{} already exists; not overwriting it", path.display())
        } else {
            err.to_string()
        }
    })?;
    let encoded = Zeroizing::new(hex::encode(key.to_bytes()));
    std::io::Write::write_all(&mut file, encoded.as_bytes()).map_err(|err| err.to_string())?;
    std::fs::write(
        vibe.join(DEV_PUB),
        hex::encode(key.verifying_key().to_bytes()),
    )
    .map_err(|err| err.to_string())?;
    Ok(key)
}

/// Loads the development signing key. Never creates one: a missing key is an
/// error, and a key file readable by group or others is refused.
pub fn load_dev_key(dir: &std::path::Path) -> Result<SigningKey, String> {
    let path = dir.join(".vibe").join(DEV_KEY);
    let meta = std::fs::metadata(&path).map_err(|_| {
        format!(
            "no signing key at {}; run `vibe keygen --project {}` for a development key",
            path.display(),
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!("{} must be mode 0600", path.display()));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    let text = Zeroizing::new(std::fs::read_to_string(&path).map_err(|err| err.to_string())?);
    Ok(SigningKey::from_bytes(&*decode_key(&text, "signing key")?))
}

/// Development public key from `<dir>/.vibe/signing.pub`. Only for
/// development; production must use [`pinned_public_key`].
pub fn load_dev_public(dir: &std::path::Path) -> Result<VerifyingKey, String> {
    let text =
        std::fs::read_to_string(dir.join(".vibe").join(DEV_PUB)).map_err(|err| err.to_string())?;
    VerifyingKey::from_bytes(&*decode_key(&text, "public key")?).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_uses_strict_verification() {
        let key = generate_key();
        let msg = b"manifest-bytes";
        let sig = sign_bytes(msg, &key);
        assert!(verify_bytes(msg, &sig, &key.verifying_key()));
        assert!(!verify_bytes(b"other", &sig, &key.verifying_key()));
    }

    #[test]
    fn weak_public_key_is_rejected() {
        // Identity point (order 1) — a classic weak ed25519 public key.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        let vk = VerifyingKey::from_bytes(&weak).expect("identity encodes");
        assert!(vk.is_weak());
        let msg = b"anyone can forge under a weak key";
        // A zero signature is enough: verify_strict must refuse the weak key
        // before accepting any message.
        let sig = Signature::from_bytes(&[0u8; 64]);
        assert!(vk.verify_strict(msg, &sig).is_err());
        assert!(!verify_bytes(msg, &hex::encode(sig.to_bytes()), &vk));
    }

    #[test]
    fn non_canonical_signature_scalar_is_rejected() {
        let key = generate_key();
        let msg = b"payload";
        let mut raw = key.sign(msg).to_bytes();
        // Force s outside [0, L). Signature::from_slice may still accept the
        // bytes; verify_strict must refuse the non-canonical scalar.
        raw[63] |= 0xe0;
        assert!(!verify_bytes(msg, &hex::encode(raw), &key.verifying_key()));
    }
}

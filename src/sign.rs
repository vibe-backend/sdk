use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

pub fn sign_lock(text_with_none: &str, key: &SigningKey) -> String {
    let signature = hex::encode(key.sign(text_with_none.as_bytes()).to_bytes());
    text_with_none.replacen("signature: (none)", &format!("signature: {signature}"), 1)
}

pub fn verify_lock(text: &str, key: &VerifyingKey) -> bool {
    let Some(signature) = text
        .lines()
        .find(|line| line.starts_with("signature: "))
        .map(|line| line.trim_start_matches("signature: ").trim())
    else {
        return false;
    };
    if signature == "(none)" || signature.is_empty() {
        return false;
    }
    let payload = text.replacen(&format!("signature: {signature}"), "signature: (none)", 1);
    verify_bytes(payload.as_bytes(), signature, key)
}

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
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    SigningKey::from_bytes(&seed)
}

pub fn load_or_create(dir: &std::path::Path) -> Result<SigningKey, String> {
    let path = dir.join(".vibe").join("signing.key");
    if path.exists() {
        let hex_key = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
        let bytes = hex::decode(hex_key.trim()).map_err(|err| err.to_string())?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "signing key must be 32 bytes".to_string())?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    let key = generate_key();
    std::fs::create_dir_all(dir.join(".vibe")).map_err(|err| err.to_string())?;
    std::fs::write(&path, hex::encode(key.to_bytes())).map_err(|err| err.to_string())?;
    std::fs::write(
        dir.join(".vibe").join("signing.pub"),
        hex::encode(key.verifying_key().to_bytes()),
    )
    .map_err(|err| err.to_string())?;
    Ok(key)
}

pub fn load_public(dir: &std::path::Path) -> Result<VerifyingKey, String> {
    let text = std::fs::read_to_string(dir.join(".vibe").join("signing.pub"))
        .map_err(|err| err.to_string())?;
    let bytes = hex::decode(text.trim()).map_err(|err| err.to_string())?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "public key must be 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&bytes).map_err(|err| err.to_string())
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

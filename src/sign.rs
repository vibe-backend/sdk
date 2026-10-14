use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

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
    key.verify(bytes, &signature).is_ok()
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

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

const DEV_KEY: &str = "signing.key";
const DEV_PUB: &str = "signing.pub";

fn decode_key(hex_key: &str, what: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(hex_key.trim()).map_err(|_| format!("{what} is not hex"))?;
    bytes
        .try_into()
        .map_err(|_| format!("{what} must be 32 bytes"))
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
    std::io::Write::write_all(&mut file, hex::encode(key.to_bytes()).as_bytes())
        .map_err(|err| err.to_string())?;
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
    let text = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
    Ok(SigningKey::from_bytes(&decode_key(&text, "signing key")?))
}

/// Release signing key: only from `VIBE_RELEASE_SIGNING_KEY` (hex), set as a
/// secret of the protected `release` environment. There is no fallback to a
/// file or a generated key.
pub fn load_release_key() -> Result<SigningKey, String> {
    let text = std::env::var("VIBE_RELEASE_SIGNING_KEY").map_err(|_| {
        "release builds need VIBE_RELEASE_SIGNING_KEY (the release environment secret); there is no fallback".to_string()
    })?;
    Ok(SigningKey::from_bytes(&decode_key(
        &text,
        "VIBE_RELEASE_SIGNING_KEY",
    )?))
}

/// The pinned release public key: `VIBE_MANIFEST_PUBKEY` (hex) from the
/// runtime configuration, else one compiled into the binary with
/// `VIBE_RELEASE_PUBKEY` at build time.
pub fn pinned_public_key() -> Result<Option<VerifyingKey>, String> {
    let raw = std::env::var("VIBE_MANIFEST_PUBKEY")
        .ok()
        .or_else(|| option_env!("VIBE_RELEASE_PUBKEY").map(str::to_string));
    match raw {
        Some(text) if !text.trim().is_empty() => {
            let bytes = decode_key(&text, "pinned manifest public key")?;
            VerifyingKey::from_bytes(&bytes)
                .map(Some)
                .map_err(|err| err.to_string())
        }
        _ => Ok(None),
    }
}

/// Development public key from `<dir>/.vibe/signing.pub`. Only for
/// development; production must use [`pinned_public_key`].
pub fn load_dev_public(dir: &std::path::Path) -> Result<VerifyingKey, String> {
    let text =
        std::fs::read_to_string(dir.join(".vibe").join(DEV_PUB)).map_err(|err| err.to_string())?;
    VerifyingKey::from_bytes(&decode_key(&text, "public key")?).map_err(|err| err.to_string())
}

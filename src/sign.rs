use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
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
    key.verify(bytes, &signature).is_ok()
}

pub fn generate_key() -> SigningKey {
    let mut seed = Zeroizing::new([0u8; 32]);
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, seed.as_mut());
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

/// Official release builds are never signed with a key from the
/// environment: they are signed keyless with Sigstore by the release job
/// (decisions 113, 118). `VIBE_RELEASE_SIGNING_KEY` is refused outright so a
/// stale secret cannot produce something that looks like a release.
pub fn refuse_env_key_for_release() -> Result<(), String> {
    if std::env::var_os("VIBE_RELEASE_SIGNING_KEY").is_some() {
        return Err("VIBE_RELEASE_SIGNING_KEY is not accepted for release builds: releases are signed keyless with Sigstore (GitHub OIDC, Fulcio, Rekor) by the release workflow; unset it".into());
    }
    Ok(())
}

/// A pinned ed25519 manifest key. A key compiled into the binary with
/// `VIBE_RELEASE_PUBKEY` at build time takes precedence: the runtime
/// `VIBE_MANIFEST_PUBKEY` cannot override it (a differing value is refused).
/// Without a compiled-in key, `VIBE_MANIFEST_PUBKEY` (hex) is used.
/// Development only: release manifests are verified with their Sigstore
/// bundle, never with a key.
pub fn pinned_public_key() -> Result<Option<VerifyingKey>, String> {
    resolve_pinned(
        option_env!("VIBE_RELEASE_PUBKEY"),
        std::env::var("VIBE_MANIFEST_PUBKEY").ok().as_deref(),
    )
}

fn parse_public(text: &str) -> Result<VerifyingKey, String> {
    let bytes = decode_key(text, "pinned manifest public key")?;
    VerifyingKey::from_bytes(&bytes).map_err(|err| err.to_string())
}

pub(crate) fn resolve_pinned(
    compiled: Option<&str>,
    runtime: Option<&str>,
) -> Result<Option<VerifyingKey>, String> {
    let compiled = compiled.filter(|t| !t.trim().is_empty());
    let runtime = runtime.filter(|t| !t.trim().is_empty());
    match (compiled, runtime) {
        (Some(c), Some(r)) => {
            let (c, r) = (parse_public(c)?, parse_public(r)?);
            if c != r {
                return Err("VIBE_MANIFEST_PUBKEY differs from the release key compiled into this binary; the compiled-in key cannot be overridden".into());
            }
            Ok(Some(c))
        }
        (Some(c), None) => parse_public(c).map(Some),
        (None, Some(r)) => parse_public(r).map(Some),
        (None, None) => Ok(None),
    }
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
    fn runtime_key_cannot_override_the_compiled_key() {
        let a = hex::encode(generate_key().verifying_key().to_bytes());
        let b = hex::encode(generate_key().verifying_key().to_bytes());
        let err = resolve_pinned(Some(&a), Some(&b)).unwrap_err();
        assert!(err.contains("cannot be overridden"), "{err}");
        let same = resolve_pinned(Some(&a), Some(&a)).unwrap().unwrap();
        assert_eq!(hex::encode(same.to_bytes()), a);
        let compiled = resolve_pinned(Some(&a), None).unwrap().unwrap();
        assert_eq!(hex::encode(compiled.to_bytes()), a);
        let runtime = resolve_pinned(None, Some(&b)).unwrap().unwrap();
        assert_eq!(hex::encode(runtime.to_bytes()), b);
        assert!(resolve_pinned(None, None).unwrap().is_none());
    }
}

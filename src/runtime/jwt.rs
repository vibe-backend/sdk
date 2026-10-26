//! Dev EC P-256 keypair for `VIBE_DB=memory` when no JWT env vars are set.
//!
//! The pair is fixed and published in this binary on purpose: it is only
//! installed for the in-memory executor, and only when both `JWT_SIGNING_KEY`
//! and `VIBE_JWT_PUBLIC_KEY_PEM` are unset. Production refuses memory mode.
//! `OnceLock` keeps the load once per process.

use std::sync::OnceLock;

use jsonwebtoken::{Algorithm, EncodingKey};

use super::http::{AuthConfig, SigningKey};

const DEV_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

const DEV_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE0mqNqll14aIJEhYqCiRLWJX63Ty6\n\
Q0UxBzbmJ2wI9FMVJLw4XEhuIJFqIdA/P6b6ozSNGyOUZK7OjIfGKtpNYw==\n\
-----END PUBLIC KEY-----\n";

struct DevKey {
    private_pem: &'static str,
    public_pem: &'static str,
    encoding: EncodingKey,
}

fn dev_key() -> &'static DevKey {
    static KEY: OnceLock<DevKey> = OnceLock::new();
    KEY.get_or_init(|| DevKey {
        private_pem: DEV_PRIVATE_PEM,
        public_pem: DEV_PUBLIC_PEM,
        encoding: EncodingKey::from_ec_pem(DEV_PRIVATE_PEM.as_bytes())
            .expect("built-in dev EC P-256 key"),
    })
}

pub fn dev_private_pem() -> &'static str {
    dev_key().private_pem
}

pub fn dev_public_pem() -> &'static str {
    dev_key().public_pem
}

pub fn dev_encoding_key() -> &'static EncodingKey {
    &dev_key().encoding
}

/// Both signing env vars are absent, so memory mode may use the dev keypair.
pub fn env_allows_dev_jwt(env: &dyn Fn(&str) -> Option<String>) -> bool {
    let empty = |name: &str| {
        env(name)
            .as_ref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
    };
    empty("JWT_SIGNING_KEY") && empty("VIBE_JWT_PUBLIC_KEY_PEM")
}

/// Fill a missing verifier and the `JWT_SIGNING_KEY` slot from the dev pair.
/// Existing `public_pem` and signing keys are left as they are.
pub fn ensure_dev_jwt(auth: &mut AuthConfig) -> Result<(), String> {
    if auth.alg != Algorithm::ES256 {
        return Err(
            "memory JWT fallback only supports ES256 (unset VIBE_JWT_ALG or set ES256)".into(),
        );
    }
    if auth.public_pem.trim().is_empty() {
        auth.public_pem = dev_public_pem().to_string();
        super::http::check_static_jwt_key(&auth.public_pem, Algorithm::ES256)?;
    }
    auth.signing_keys
        .entry("JWT_SIGNING_KEY".to_string())
        .or_insert_with(|| SigningKey {
            alg: Algorithm::ES256,
            kid: auth.kid.clone(),
            key: dev_encoding_key().clone(),
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, DecodingKey, Validation};

    #[test]
    fn dev_keypair_signs_and_verifies() {
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::ES256),
            &serde_json::json!({"sub": "user", "exp": 9_999_999_999_i64}),
            dev_encoding_key(),
        )
        .unwrap();
        let mut validation = Validation::new(Algorithm::ES256);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        let key = DecodingKey::from_ec_pem(dev_public_pem().as_bytes()).unwrap();
        let data = decode::<serde_json::Value>(&token, &key, &validation).unwrap();
        assert_eq!(data.claims["sub"], "user");
    }

    #[test]
    fn explicit_keys_block_the_dev_fallback() {
        let env = |name: &str| (name == "JWT_SIGNING_KEY").then(|| "pem".to_string());
        assert!(!env_allows_dev_jwt(&env));
        assert!(env_allows_dev_jwt(&|_| None));
    }
}

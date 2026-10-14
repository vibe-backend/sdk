//! Executable method ops (spec §12.8, decisions 52/55/62): `query`,
//! `verify-password`, `hash-password`, `random-token`, `hash-token` and
//! `sign-jwt`. Refresh tokens (`issue-refresh`) come in a later change.

use std::sync::OnceLock;

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{Map, Value};
use sha2::Sha256;

use crate::manifest::{MethodSpec, StepSpec};

use super::crud::{eval_expr, hash_password, CallCtx, MethodFail};
use super::exec::{ExecError, QueryExec};

/// At most four argon2id computations at once per instance (spec 4A).
fn argon_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(4))
}

/// Compared against when the user does not exist, so both paths cost the same.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("dummy-password-for-timing").expect("dummy hash"))
}

fn opt<'a>(step: &'a StepSpec, key: &str) -> &'a str {
    step.opts.get(key).map(String::as_str).unwrap_or("").trim()
}

fn internal(msg: impl Into<String>) -> MethodFail {
    MethodFail::code("INTERNAL", msg)
}

fn eval(expr: &str, vars: &Map<String, Value>) -> Result<Value, MethodFail> {
    eval_expr(expr, vars).map_err(internal)
}

fn hmac_hex(pepper: &[u8], value: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(pepper).expect("hmac key");
    mac.update(value.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn as_param(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s),
        Value::Array(items) => {
            // PostgreSQL array literal for text[] parameters.
            let inner: Vec<String> = items
                .iter()
                .map(|v| {
                    let s = v
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string());
                    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
                })
                .collect();
            Some(format!("{{{}}}", inner.join(",")))
        }
        other => Some(other.to_string()),
    }
}

/// `query`: the first row as an object, or null when there is none.
pub async fn query(
    exec: &dyn QueryExec,
    step: &StepSpec,
    vars: &Map<String, Value>,
    ctx: CallCtx<'_>,
) -> Result<Value, MethodFail> {
    if step.table.starts_with("_platform") || step.sql.to_ascii_lowercase().contains("_platform_") {
        return Err(internal("platform tables cannot be queried"));
    }
    let mut params = Vec::with_capacity(step.params.len());
    for expr in &step.params {
        params.push(as_param(eval(expr, vars)?));
    }
    let rows = exec
        .raw(ctx.tenant_id, &step.sql, params)
        .await
        .map_err(|err| match err {
            ExecError::Message(msg) if msg.contains("--memory") => internal(msg),
            ExecError::Message(msg) if msg.contains("duplicate key") => {
                MethodFail::code("CONFLICT", "already exists")
            }
            other => {
                tracing::warn!(error = ?other, "query step failed");
                internal("query failed")
            }
        })?;
    Ok(rows
        .into_iter()
        .next()
        .map(Value::Object)
        .unwrap_or(Value::Null))
}

/// `verify-password`: constant-time argon2id check. A missing hash (no such
/// user) is compared against a dummy hash and always fails.
pub async fn verify_password(
    step: &StepSpec,
    vars: &Map<String, Value>,
) -> Result<Value, MethodFail> {
    let hash = eval(opt(step, "hash"), vars)?;
    let plain = eval(opt(step, "plain"), vars)?;
    let plain = plain.as_str().unwrap_or("").to_string();
    let (hash, real) = match hash.as_str() {
        Some(h) if !h.is_empty() => (h.to_string(), true),
        _ => {
            if opt(step, "missing-user") != "dummy-hash" {
                return Err(internal("verify-password needs missing-user: dummy-hash"));
            }
            (dummy_hash().to_string(), false)
        }
    };
    let _slot = argon_slots()
        .acquire()
        .await
        .map_err(|_| internal("argon2 pool closed"))?;
    let ok = tokio::task::spawn_blocking(move || {
        use argon2::password_hash::{PasswordHash, PasswordVerifier};
        let Ok(parsed) = PasswordHash::new(&hash) else {
            return false;
        };
        argon2::Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .map_err(|_| internal("argon2 task failed"))?;
    Ok(Value::Bool(ok && real))
}

/// `hash-password` with the platform floor (WEAK_PASSWORD below it).
pub async fn hash_password_op(
    step: &StepSpec,
    vars: &Map<String, Value>,
) -> Result<Value, MethodFail> {
    let plain = eval(opt(step, "plain"), vars)?;
    let plain = plain.as_str().unwrap_or("").to_string();
    let _slot = argon_slots()
        .acquire()
        .await
        .map_err(|_| internal("argon2 pool closed"))?;
    let hash = tokio::task::spawn_blocking(move || hash_password(&plain))
        .await
        .map_err(|_| internal("argon2 task failed"))??;
    Ok(Value::String(hash))
}

/// `random-token`: `{value, hash}`; `value` is base64url (sensitive), `hash`
/// is HMAC-SHA-256(pepper, value) hex, the same as `hash-token` of `value`.
pub fn random_token(step: &StepSpec, ctx: CallCtx<'_>) -> Result<Value, MethodFail> {
    let bytes: usize = match opt(step, "bytes") {
        "" => 32,
        raw => raw
            .parse()
            .map_err(|_| internal("random-token bytes must be a number"))?,
    };
    if !(16..=64).contains(&bytes) {
        return Err(internal("random-token bytes must be 16..=64"));
    }
    let mut buf = vec![0u8; bytes];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut buf);
    let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf);
    let hash = hmac_hex(&ctx.auth.pepper, &value);
    Ok(serde_json::json!({"value": value, "hash": hash}))
}

/// `hash-token`: HMAC-SHA-256(pepper, value) hex; `normalize: email` trims and
/// lowercases first.
pub fn hash_token(
    step: &StepSpec,
    vars: &Map<String, Value>,
    ctx: CallCtx<'_>,
) -> Result<Value, MethodFail> {
    let value = eval(&step.value, vars)?;
    let Some(text) = value.as_str() else {
        return Ok(Value::Null);
    };
    let text = match opt(step, "normalize") {
        "" => text.to_string(),
        "email" => text.trim().to_lowercase(),
        other => return Err(internal(format!("unknown normalize {other}"))),
    };
    Ok(Value::String(hmac_hex(&ctx.auth.pepper, &text)))
}

/// Claims the platform sets; a method's `claims` may not name them.
const PLATFORM_CLAIMS: &[&str] = &[
    "iss",
    "aud",
    "exp",
    "nbf",
    "iat",
    "jti",
    "tenant_id",
    "roles",
    "scope",
    "scp",
];

/// `sign-jwt`: only in methods with `capabilities: [sign-token]`. `tenant_id`
/// is always the request's tenant (decision 62); iss/aud come from the
/// runtime; ttl is capped at 1 h.
pub fn sign_jwt(
    method: &MethodSpec,
    step: &StepSpec,
    vars: &Map<String, Value>,
    ctx: CallCtx<'_>,
) -> Result<Value, MethodFail> {
    if !method.capabilities.iter().any(|c| c == "sign-token") {
        return Err(internal("sign-jwt requires capabilities: [sign-token]"));
    }
    match opt(step, "issue-refresh") {
        "" | "none" | "platform-decided" => {}
        other => {
            return Err(internal(format!(
                "issue-refresh {other} is not supported yet"
            )))
        }
    }
    let key_ref = opt(step, "key-ref");
    let key = ctx
        .auth
        .signing_keys
        .get(key_ref)
        .ok_or_else(|| internal(format!("signing key {key_ref} is not loaded")))?;
    let alg = match opt(step, "alg") {
        "ES256" => jsonwebtoken::Algorithm::ES256,
        "RS256" => jsonwebtoken::Algorithm::RS256,
        other => return Err(internal(format!("unsupported alg {other}"))),
    };
    if alg != key.alg {
        return Err(internal("sign-jwt alg does not match the key"));
    }
    let ttl_ms = match opt(step, "ttl") {
        "" => 3_600_000,
        raw => crate::llm::parse_duration_ms(raw).ok_or_else(|| internal("bad ttl"))?,
    };
    if ttl_ms == 0 || ttl_ms > 3_600_000 {
        return Err(internal("access token ttl must be within 1h"));
    }
    let mut claims = Map::new();
    if !opt(step, "claims").is_empty() {
        let pairs = crate::lock::parse_inline_map(opt(step, "claims")).map_err(internal)?;
        for (name, expr) in pairs {
            if PLATFORM_CLAIMS.contains(&name.as_str()) {
                return Err(internal(format!("claim {name} is set by the platform")));
            }
            claims.insert(name, eval(&expr, vars)?);
        }
    }
    let sub = claims.get("sub").and_then(Value::as_str).unwrap_or("");
    if sub.is_empty() {
        return Err(internal("sign-jwt needs a sub claim"));
    }
    let roles = match opt(step, "roles") {
        "" => Value::Array(Vec::new()),
        expr => match eval(expr, vars)? {
            Value::Array(items) if items.iter().all(Value::is_string) => Value::Array(items),
            Value::Null => Value::Array(Vec::new()),
            _ => return Err(internal("roles must be a list of strings")),
        },
    };
    let now = chrono::Utc::now().timestamp();
    let ttl = (ttl_ms / 1000) as i64;
    claims.insert("iss".into(), ctx.auth.iss.clone().into());
    claims.insert("aud".into(), ctx.auth.aud.clone().into());
    claims.insert("iat".into(), now.into());
    claims.insert("nbf".into(), now.into());
    claims.insert("exp".into(), (now + ttl).into());
    claims.insert("jti".into(), uuid::Uuid::new_v4().to_string().into());
    claims.insert("tenant_id".into(), ctx.tenant_id.into());
    claims.insert(ctx.auth.roles_claim.clone(), roles);
    let mut header = jsonwebtoken::Header::new(alg);
    header.kid = Some(key.kid.clone());
    let token = jsonwebtoken::encode(&header, &Value::Object(claims), &key.key)
        .map_err(|err| internal(format!("sign-jwt: {err}")))?;
    Ok(serde_json::json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": ttl,
    }))
}

/// Startup: every `sign-jwt` key-ref must be loadable from the environment
/// variable of the same name (a private PEM matching the step's alg).
pub fn load_signing_keys(
    manifest: &crate::manifest::Manifest,
    env: &dyn Fn(&str) -> Option<String>,
    default_kid: &str,
) -> Result<std::collections::HashMap<String, super::http::SigningKey>, String> {
    let mut keys = std::collections::HashMap::new();
    for method in &manifest.methods {
        for step in method.steps.iter().filter(|s| s.op == "sign-jwt") {
            let key_ref = opt(step, "key-ref").to_string();
            if keys.contains_key(&key_ref) {
                continue;
            }
            let pem = env(&key_ref)
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| {
                    format!(
                        "{}: set {key_ref} to the private signing key PEM",
                        method.target
                    )
                })?;
            let (alg, key) = match opt(step, "alg") {
                "ES256" => (
                    jsonwebtoken::Algorithm::ES256,
                    jsonwebtoken::EncodingKey::from_ec_pem(pem.as_bytes()),
                ),
                "RS256" => (
                    jsonwebtoken::Algorithm::RS256,
                    jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()),
                ),
                other => return Err(format!("{}: unsupported alg {other}", method.target)),
            };
            let key = key.map_err(|err| format!("{key_ref}: {err}"))?;
            let kid = env(&format!("{key_ref}_KID"))
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default_kid.to_string());
            keys.insert(key_ref, super::http::SigningKey { alg, kid, key });
        }
    }
    Ok(keys)
}

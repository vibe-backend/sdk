//! Executable method ops (spec §12.8, decisions 52/55/62): `query`,
//! `verify-password`, `hash-password`, `random-token`, `hash-token` and
//! `sign-jwt` (with platform refresh tokens via `issue-refresh`).

use std::sync::OnceLock;

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
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

/// Builds the dummy hash at startup so the first login does not pay for it.
pub fn warm_up() {
    let _ = dummy_hash();
}

/// Compared against when the user does not exist, so both paths cost the same.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("dummy-password-for-timing").expect("dummy hash"))
}

/// Waits at most 5 s for an argon2 slot; a saturated instance answers 503.
async fn argon_slot() -> Result<tokio::sync::SemaphorePermit<'static>, MethodFail> {
    match tokio::time::timeout(std::time::Duration::from_secs(5), argon_slots().acquire()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(internal("argon2 pool closed")),
        Err(_) => Err(MethodFail::code("SERVICE_UNAVAILABLE", "Service is busy")),
    }
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

fn hmac(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
    mac.update(value);
    mac.finalize().into_bytes().to_vec()
}

/// Token hashes use a key derived from the pepper, never the pepper itself,
/// so they cannot collide with other pepper uses (API-key hashes).
fn token_key(pepper: &[u8]) -> Vec<u8> {
    hmac(pepper, b"vibe/token-hash/v1")
}

/// HMAC-SHA-256 of a token as a PostgreSQL `bytea` literal (`\x...`), which
/// a text-format parameter binds to a `bytea` column (decision 66).
fn token_hash(pepper: &[u8], value: &str) -> String {
    format!(
        "\\x{}",
        hex::encode(hmac(&token_key(pepper), value.as_bytes()))
    )
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
    method: &MethodSpec,
    step: &StepSpec,
    vars: &Map<String, Value>,
    ctx: CallCtx<'_>,
) -> Result<Value, MethodFail> {
    if step.table.starts_with("_platform") {
        return Err(internal("platform tables cannot be queried"));
    }
    // Parser allowlist and statement kind (no substring heuristics).
    let info = crate::sqlguard::analyze(&step.sql).map_err(internal)?;
    let writes = info.writes;
    if writes && !step.writes {
        return Err(internal("query writes but the step is not marked writes"));
    }
    if (writes || step.writes) && !method.writes {
        return Err(internal(
            "query writes but the method is not marked writes: true",
        ));
    }
    // Spec §12.8: queries run as public_reader, or public_writer for writes.
    let role = if writes {
        "public_writer"
    } else {
        "public_reader"
    };
    let mut params = Vec::with_capacity(step.params.len());
    for expr in &step.params {
        params.push(as_param(eval(expr, vars)?));
    }
    let rows = exec
        .raw(ctx.tenant_id, role, writes, &step.sql, params)
        .await
        .map_err(query_error)?;
    Ok(rows
        .into_iter()
        .next()
        .map(Value::Object)
        .unwrap_or(Value::Null))
}

fn query_error(err: ExecError) -> MethodFail {
    match err {
        ExecError::Message(msg) if msg.starts_with("MEMORY_UNSUPPORTED_SQL:") => {
            MethodFail::code("MEMORY_UNSUPPORTED_SQL", msg)
        }
        // SQLSTATE 23505 (unique violation), mapped in exec.
        ExecError::UniqueViolation => {
            MethodFail::code("UNIQUE_VIOLATION", "unique constraint violated")
        }
        ExecError::Conflict => MethodFail::code("TX_CONFLICT", "transaction conflict"),
        ExecError::Timeout => MethodFail::code("TX_TIMEOUT", "transaction timed out"),
        other => {
            tracing::warn!(error = ?other, "query step failed");
            internal("query failed")
        }
    }
}

/// Longest password accepted anywhere (spec §4A.2 floor: 12..=256).
pub const MAX_PASSWORD_CHARS: usize = 256;

/// Only hashes made with the platform's own argon2id parameters verify.
fn pinned_hash(hash: &str) -> bool {
    use argon2::password_hash::phc::PasswordHash;
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    use super::crud::{ARGON2_M_COST, ARGON2_P_COST, ARGON2_T_COST};
    let param = |name: &str| parsed.params.get_decimal(name);
    parsed.algorithm.as_str() == "argon2id"
        && parsed.version == Some(0x13)
        && param("m") == Some(ARGON2_M_COST)
        && param("t") == Some(ARGON2_T_COST)
        && param("p") == Some(ARGON2_P_COST)
        && parsed.hash.is_some_and(|h| h.len() == 32)
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
    // Same upper bound as the password floor; longer input never matches.
    let in_bounds = plain.chars().count() <= MAX_PASSWORD_CHARS;
    let plain = if in_bounds { plain } else { String::new() };
    let (hash, real) = match hash.as_str() {
        Some(h) if !h.is_empty() && pinned_hash(h) => (h.to_string(), in_bounds),
        Some(h) if !h.is_empty() => {
            tracing::warn!("stored password hash does not use the platform argon2id parameters");
            (dummy_hash().to_string(), false)
        }
        _ => {
            if opt(step, "missing-user") != "dummy-hash" {
                return Err(internal("verify-password needs missing-user: dummy-hash"));
            }
            (dummy_hash().to_string(), false)
        }
    };
    let _slot = argon_slot().await?;
    let ok = tokio::task::spawn_blocking(move || {
        use argon2::password_hash::{phc::PasswordHash, PasswordVerifier};
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
    let _slot = argon_slot().await?;
    let hash = tokio::task::spawn_blocking(move || hash_password(&plain))
        .await
        .map_err(|_| internal("argon2 task failed"))??;
    Ok(Value::String(hash))
}

/// `random-token`: `{value, hash}`. `value` is 32 random bytes, base64url
/// (sensitive); `hash` is HMAC-SHA-256 under the token key as a `bytea`
/// literal, the same as `hash-token` of `value`.
pub fn random_token(step: &StepSpec, ctx: CallCtx<'_>) -> Result<Value, MethodFail> {
    if !matches!(opt(step, "bytes"), "" | "32") {
        return Err(internal("random-token is always 32 bytes"));
    }
    let mut buf = [0u8; 32];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut buf)
        .map_err(|_| internal("random-token: system random source unavailable"))?;
    let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    let hash = token_hash(&ctx.auth.pepper, &value);
    Ok(serde_json::json!({"value": value, "hash": hash}))
}

/// `hash-token`: HMAC-SHA-256 under the token key, as a `bytea` literal; `normalize: email` trims and
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
    Ok(Value::String(token_hash(&ctx.auth.pepper, &text)))
}

/// Custom claim names a `sign-jwt` step may set: declared in `custom-claims`,
/// a plain identifier, not a reserved JWT claim and not a roles claim.
pub fn custom_claim_problem(
    name: &str,
    declared: &[String],
    roles_claims: &[&str],
) -> Option<String> {
    if !declared.iter().any(|d| d == name) {
        return Some(format!("claim {name} is not declared in custom-claims"));
    }
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Some(format!("custom claim {name} must be a plain identifier"));
    }
    let lower = name.to_ascii_lowercase();
    if crate::manifest::RESERVED_JWT_CLAIMS.contains(&lower.as_str()) || lower == "roles" {
        return Some(format!("claim {name} is set by the platform"));
    }
    // A roles claim may be a dotted path (`realm_access.roles`): its first
    // segment is off limits too.
    if roles_claims.iter().any(|rc| {
        let rc = rc.to_ascii_lowercase();
        rc == lower || rc.split('.').next() == Some(lower.as_str())
    }) {
        return Some(format!("claim {name} is a configured roles claim"));
    }
    None
}

/// Outputs of `query` steps that run before `step`.
pub fn row_outputs(steps: &[StepSpec], step: &StepSpec) -> Vec<String> {
    steps
        .iter()
        .take_while(|s| !std::ptr::eq(*s, step))
        .filter(|s| s.op == "query" && !s.out.is_empty())
        .map(|s| s.out.clone())
        .collect()
}

/// `sub` must be a field of a row a `query` step read (`u.id`).
pub fn sub_source_problem(expr: &str, rows: &[String]) -> Option<String> {
    let expr = expr.trim().trim_matches('`');
    let Some((root, field)) = expr.split_once('.') else {
        return Some("sign-jwt sub must be a field of a query step result".into());
    };
    let plain = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !rows.iter().any(|r| r == root) || !field.split('.').all(plain) {
        return Some("sign-jwt sub must be a field of a query step result".into());
    }
    None
}

/// Every roles claim the runtime reads: the global one and per-route ones.
fn roles_claims<'a>(
    manifest: &'a crate::manifest::Manifest,
    auth: &'a super::http::AuthConfig,
) -> Vec<&'a str> {
    let mut out = vec![auth.roles_claim.as_str(), "roles"];
    out.extend(
        manifest
            .routes
            .iter()
            .map(|r| r.roles_claim.as_str())
            .filter(|c| !c.is_empty()),
    );
    out
}

/// `sign-jwt`: only in methods with `capabilities: [sign-token]`. `sub` comes
/// from `claims`; roles are read by the platform from `users.roles` for that
/// `sub` (spec §12.8), never from the lock. Other claims must be declared in
/// `custom-claims`. `tenant_id` is the request's tenant (decision 62); iss/aud
/// come from the runtime; ttl is capped at 1 h.
pub async fn sign_jwt(
    manifest: &crate::manifest::Manifest,
    exec: &dyn QueryExec,
    method: &MethodSpec,
    step: &StepSpec,
    vars: &Map<String, Value>,
    ctx: CallCtx<'_>,
) -> Result<Value, MethodFail> {
    if !method.capabilities.iter().any(|c| c == "sign-token") {
        return Err(internal("sign-jwt requires capabilities: [sign-token]"));
    }
    if !opt(step, "roles").is_empty() {
        return Err(internal(
            "sign-jwt roles are read from users.roles by the platform; remove `roles`",
        ));
    }
    match opt(step, "issue-refresh") {
        "" | "none" | "platform-decided" | "new-family" => {}
        other => {
            return Err(internal(format!(
                "issue-refresh {other} is not supported by this runtime yet"
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
        // Decision 139: ES256 when `alg` is omitted.
        "ES256" | "" => jsonwebtoken::Algorithm::ES256,
        "RS256" => jsonwebtoken::Algorithm::RS256,
        other => return Err(internal(format!("unsupported alg {other}"))),
    };
    if alg != key.alg {
        return Err(internal("sign-jwt alg does not match the key"));
    }
    let ttl_ms = match opt(step, "ttl") {
        "" => 3_600_000,
        raw => crate::duration::parse_duration_ms(raw).ok_or_else(|| internal("bad ttl"))?,
    };
    if ttl_ms == 0 || ttl_ms > 3_600_000 {
        return Err(internal("access token ttl must be within 1h"));
    }
    let declared = match opt(step, "custom-claims") {
        "" => Vec::new(),
        raw => crate::lock::parse_string_list(raw).map_err(internal)?,
    };
    let reserved_roles = roles_claims(manifest, ctx.auth);
    let mut claims = Map::new();
    let mut sub = String::new();
    if !opt(step, "claims").is_empty() {
        let pairs = crate::lock::parse_inline_map(opt(step, "claims")).map_err(internal)?;
        for (name, expr) in pairs {
            if name == "sub" {
                if let Some(problem) = sub_source_problem(&expr, &row_outputs(&method.steps, step))
                {
                    return Err(internal(problem));
                }
            }
            let value = eval(&expr, vars)?;
            if name == "sub" {
                sub = value.as_str().unwrap_or("").to_string();
                continue;
            }
            if let Some(problem) = custom_claim_problem(&name, &declared, &reserved_roles) {
                return Err(internal(problem));
            }
            claims.insert(name, value);
        }
    }
    if sub.is_empty() {
        return Err(internal("sign-jwt needs a sub claim"));
    }
    let roles = exec
        .user_roles(ctx.tenant_id, &sub)
        .await
        .map_err(|err| match err {
            ExecError::Timeout => MethodFail::code("TX_TIMEOUT", "transaction timed out"),
            ExecError::Conflict => MethodFail::code("TX_CONFLICT", "transaction conflict"),
            ExecError::UniqueViolation => {
                MethodFail::code("UNIQUE_VIOLATION", "unique constraint violated")
            }
            other => {
                tracing::warn!(error = ?other, "sign-jwt roles lookup failed");
                internal("sign-jwt roles lookup failed")
            }
        })?
        .ok_or_else(|| internal("sign-jwt sub is not a user"))?;
    let now = chrono::Utc::now().timestamp();
    let ttl = (ttl_ms / 1000) as i64;
    claims.insert("sub".into(), sub.clone().into());
    claims.insert("iss".into(), ctx.auth.iss.clone().into());
    claims.insert("aud".into(), ctx.auth.aud.clone().into());
    claims.insert("iat".into(), now.into());
    claims.insert("nbf".into(), now.into());
    claims.insert("exp".into(), (now + ttl).into());
    claims.insert("jti".into(), uuid::Uuid::new_v4().to_string().into());
    claims.insert("tenant_id".into(), ctx.tenant_id.into());
    claims.insert(
        ctx.auth.roles_claim.clone(),
        Value::Array(roles.into_iter().map(Value::String).collect()),
    );
    let mut header = jsonwebtoken::Header::new(alg);
    header.kid = Some(key.kid.clone());
    let token = jsonwebtoken::encode(&header, &Value::Object(claims), &key.key)
        .map_err(|err| internal(format!("sign-jwt: {err}")))?;
    let mut out = serde_json::json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": ttl,
        "expires_at": chrono::DateTime::from_timestamp(now + ttl, 0)
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default(),
    });
    attach_refresh(manifest, exec, method, step, &mut out, &sub, ctx).await?;
    Ok(out)
}

/// A fresh opaque refresh token and its stored HMAC.
pub fn new_refresh_token(pepper: &[u8]) -> (String, String) {
    let mut buf = [0u8; 32];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut buf)
        .expect("OS random number generator failed");
    let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    let hash = refresh_hash(pepper, &value);
    (value, hash)
}

/// HMAC-SHA-256 of a refresh token (hex) under its own key derived from
/// the pepper; only this is stored.
pub fn refresh_hash(pepper: &[u8], value: &str) -> String {
    hex::encode(hmac(
        &hmac(pepper, b"vibe/refresh-token/v1"),
        value.as_bytes(),
    ))
}

/// The rotation route whose `sign-method` is `target`, if any.
pub fn refresh_route_for<'a>(
    manifest: &'a crate::manifest::Manifest,
    target: &str,
) -> Option<&'a crate::manifest::RefreshSpec> {
    manifest
        .routes
        .iter()
        .filter_map(|r| r.refresh.as_ref())
        .find(|spec| spec.sign_method == target)
}

pub fn policy_of(spec: &crate::manifest::RefreshSpec) -> super::refresh::RefreshPolicy {
    super::refresh::RefreshPolicy {
        family_ttl: spec.refresh_ttl_s,
        idle_ttl: spec.idle_ttl_s,
        grace: spec.reuse_grace_s,
    }
}

/// `issue-refresh` (spec §4A.1): on a rotation call (from the refresh route)
/// the rotated token was already stored and is handed back; it must be for
/// the same user. Otherwise (login) a new family starts. Either way a
/// `token-refresh` route must sign with this method, or the step errors.
async fn attach_refresh(
    manifest: &crate::manifest::Manifest,
    exec: &dyn QueryExec,
    method: &MethodSpec,
    step: &StepSpec,
    out: &mut Value,
    sub: &str,
    ctx: CallCtx<'_>,
) -> Result<(), MethodFail> {
    let mode = opt(step, "issue-refresh");
    if mode.is_empty() || mode == "none" {
        return Ok(());
    }
    let Some(spec) = refresh_route_for(manifest, &method.target) else {
        return Err(internal(format!(
            "issue-refresh {mode} needs a token-refresh route whose sign-method is {}",
            method.target
        )));
    };
    let token = match ctx.refresh_token {
        Some(token) => {
            if ctx.caller.subject != sub {
                return Err(internal("refreshed token must be for the same user"));
            }
            token.to_string()
        }
        None => {
            let (value, hash) = new_refresh_token(&ctx.auth.pepper);
            exec.refresh_issue(ctx.tenant_id, sub, &hash, policy_of(spec))
                .await
                .map_err(|err| internal(format!("refresh token: {err:?}")))?;
            value
        }
    };
    out["refresh_token"] = Value::String(token);
    // Seconds until this token expires if unused: the smaller of idle-ttl
    // and the family lifetime (a new family here; the refresh route replaces
    // it with the remaining value on rotation).
    out["refresh_expires_in"] = Value::from(spec.idle_ttl_s.min(spec.refresh_ttl_s));
    Ok(())
}

/// Loads a P-256 private key from PKCS#8 PEM, checking it at load time.
// The hand-written P-256 checks in eckey are defense-in-depth only to report
// errors early at startup; authoritative parsing and verification is done by
// jsonwebtoken (aws_lc_rs).
pub fn ec_signing_key_from_pem(pem: &[u8]) -> Result<jsonwebtoken::EncodingKey, String> {
    let text = std::str::from_utf8(pem).map_err(|_| "invalid PEM encoding".to_string())?;
    let der = super::eckey::p256_private(text)
        .map_err(|p| super::http::ec_problem_message("private key", p))?;
    let key = jsonwebtoken::EncodingKey::from_ec_der(&der);
    check_signing_key(&key, jsonwebtoken::Algorithm::ES256)?;
    Ok(key)
}

/// Loads PKCS#8 DER with the same validation and test signature as PEM.
/// Raw SEC1 DER is not supported by the signing backend.
pub fn ec_signing_key_from_der(der: &[u8]) -> Result<jsonwebtoken::EncodingKey, String> {
    super::eckey::p256_private_der(der)
        .map_err(|p| super::http::ec_problem_message("private key", p))?;
    let key = jsonwebtoken::EncodingKey::from_ec_der(der);
    check_signing_key(&key, jsonwebtoken::Algorithm::ES256)?;
    Ok(key)
}

/// Signs a fixed test payload so an unusable key fails at load time.
fn check_signing_key(
    key: &jsonwebtoken::EncodingKey,
    alg: jsonwebtoken::Algorithm,
) -> Result<(), String> {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(alg),
        &serde_json::json!({ "check": true }),
        key,
    )
    .map(|_| ())
    .map_err(|err| format!("private key is not usable for {alg:?}: {err}"))
}

/// Startup: every `sign-jwt` key-ref must be loadable from the environment
/// variable of the same name (a private PEM matching the step's alg).
pub fn load_signing_keys(
    manifest: &crate::manifest::Manifest,
    env: &dyn Fn(&str) -> Option<String>,
    default_kid: &str,
) -> Result<std::collections::HashMap<String, super::http::SigningKey>, String> {
    load_signing_keys_with(manifest, env, default_kid, false)
}

/// Like [`load_signing_keys`]. When `memory_fallback` is set, a missing ES256
/// key-ref uses the process dev key instead of refusing to start.
pub fn load_signing_keys_with(
    manifest: &crate::manifest::Manifest,
    env: &dyn Fn(&str) -> Option<String>,
    default_kid: &str,
    memory_fallback: bool,
) -> Result<std::collections::HashMap<String, super::http::SigningKey>, String> {
    let mut keys = std::collections::HashMap::new();
    for method in &manifest.methods {
        for step in method.steps.iter().filter(|s| s.op == "sign-jwt") {
            let key_ref = opt(step, "key-ref").to_string();
            if keys.contains_key(&key_ref) {
                continue;
            }
            let alg_name = opt(step, "alg");
            let pem = match env(&key_ref).filter(|v| !v.trim().is_empty()) {
                Some(pem) => pem,
                None if memory_fallback && matches!(alg_name, "ES256" | "") => {
                    super::jwt::dev_private_pem().to_string()
                }
                None => {
                    return Err(format!(
                        "{}: set {key_ref} to the private signing key PEM",
                        method.target
                    ))
                }
            };
            let (alg, key) = match opt(step, "alg") {
                // Decision 139: ES256 when `alg` is omitted.
                "ES256" | "" => (
                    jsonwebtoken::Algorithm::ES256,
                    ec_signing_key_from_pem(pem.as_bytes()),
                ),
                "RS256" => {
                    // Decision 139: RS256 signing keys need >= 2048 bits.
                    super::rsakey::check_rsa_pem(&key_ref, &pem)?;
                    (
                        jsonwebtoken::Algorithm::RS256,
                        jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
                            .map_err(|_| "invalid RSA private key".to_string()),
                    )
                }
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

#[cfg(test)]
mod query_error_tests {
    use super::*;
    #[test]
    fn unique_is_not_a_retryable_transaction_conflict() {
        let unsupported = query_error(ExecError::Message("MEMORY_UNSUPPORTED_SQL: JOIN".into()));
        assert_eq!(unsupported.code, "MEMORY_UNSUPPORTED_SQL");
        let unique = query_error(ExecError::UniqueViolation);
        assert_eq!(unique.code, "UNIQUE_VIOLATION");
        assert_eq!(unique.message, "unique constraint violated");
        assert_eq!(query_error(ExecError::Conflict).code, "TX_CONFLICT");
    }
}

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, Request, Response, StatusCode};

type HttpResponse = Response<Body>;
use axum::Router;
use hmac::{Hmac, KeyInit, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::manifest::{Manifest, RouteSpec};

use super::crud::{self, MethodFail};
use super::exec::QueryExec;
use super::limiter::{
    ip_bucket, owner_of, parse_hop, ProxyNet, QuotaBuckets, RateBuckets, RateDecision,
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredKey {
    pub key_id: String,
    pub secret_hash: String,
    pub env: String,
    pub tenant_id: String,
    pub scopes: Vec<String>,
    pub expires_unix: i64,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Clone)]
pub struct AuthConfig {
    pub public_pem: String,
    pub alg: Algorithm,
    pub kid: String,
    pub iss: String,
    pub aud: String,
    pub pepper: Vec<u8>,
    pub api_keys: Vec<StoredKey>,
    /// When set, keys come from this file (reloaded on change) instead of
    /// `api_keys`.
    pub api_keys_file: Option<Arc<ApiKeyFile>>,
    pub env_name: String,
    /// When set, JWT keys come from this JWKS (spec §4.1) instead of
    /// `public_pem`/`kid`.
    pub jwks: Option<Arc<super::jwks::JwksCache>>,
    /// Default claim holding the caller's roles (`VIBE_JWT_ROLES_CLAIM`,
    /// default `roles`); a route's `roles-claim` overrides it.
    pub roles_claim: String,
    /// Private keys for `sign-jwt` steps, by `key-ref` (loaded at startup).
    pub signing_keys: HashMap<String, SigningKey>,
    /// JWKS caches for routes with their own `jwks-uri` (or an `iss` whose
    /// default `{iss}/.well-known/jwks.json` differs), keyed by URI. Built at
    /// startup; a route whose URI is missing here fails closed.
    pub route_jwks: HashMap<String, Arc<super::jwks::JwksCache>>,
}

/// A JWT signing key named by a lock `key-ref`.
#[derive(Clone)]
pub struct SigningKey {
    pub alg: Algorithm,
    pub kid: String,
    pub key: jsonwebtoken::EncodingKey,
}

/// The JWKS URI a route authenticates against when it overrides the issuer:
/// its `jwks-uri`, else spec §4.1's `{iss}/.well-known/jwks.json`. `None`
/// means the route uses the process-wide key source.
pub fn route_jwks_uri(route: &RouteSpec) -> Option<String> {
    if !route.jwks_uri.is_empty() {
        Some(route.jwks_uri.clone())
    } else if !route.jwt_iss.is_empty() {
        Some(format!(
            "{}/.well-known/jwks.json",
            route.jwt_iss.trim_end_matches('/')
        ))
    } else {
        None
    }
}

impl AuthConfig {
    /// Creates one HTTPS JWKS cache per distinct route URI (cache rules apply
    /// per URI).
    pub fn add_route_jwks(&mut self, routes: &[RouteSpec]) -> Result<(), String> {
        for route in routes {
            if let Some(uri) = route_jwks_uri(route) {
                if self.route_jwks.contains_key(&uri) {
                    continue;
                }
                // Route URIs come from locks: public hosts only, no dev exception.
                let source = super::jwks::HttpsJwks::new(&uri)
                    .map_err(|err| format!("{} {}: {err}", route.method, route.path))?;
                self.route_jwks
                    .insert(uri, Arc::new(super::jwks::JwksCache::new(Arc::new(source))));
            }
        }
        Ok(())
    }

    pub fn anonymous() -> Self {
        Self {
            public_pem: String::new(),
            alg: Algorithm::ES256,
            kid: "local".into(),
            iss: "https://auth.localhost/shop".into(),
            aud: "shop".into(),
            pepper: b"dev-pepper".to_vec(),
            api_keys: Vec::new(),
            api_keys_file: None,
            env_name: "test".into(),
            jwks: None,
            roles_claim: "roles".into(),
            signing_keys: HashMap::new(),
            route_jwks: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Slug {
    pub tenant_id: String,
    pub public_access: bool,
    pub public_write: bool,
}

pub struct AppState {
    pub manifest: Manifest,
    pub exec: Arc<dyn QueryExec>,
    pub auth: AuthConfig,
    pub slugs: HashMap<String, Slug>,
    pub rates: Mutex<RateBuckets>,
    pub quotas: Mutex<QuotaBuckets>,
    pub disabled: HashSet<String>,
    pub min_override_ms: Option<u64>,
    pub honor_test_ip: bool,
    /// Proxies whose `X-Forwarded-For` is trusted. Empty = use the peer address.
    pub trusted_proxies: Vec<ProxyNet>,
    /// When set, an unknown public slug explains how to register it.
    pub dev_hints: bool,
    /// Verifies `x-challenge-proof` (#118). `None`: challenge routes fail closed.
    pub challenge: Option<Arc<dyn super::challenge::ChallengeVerifier>>,
}

pub fn router(state: Arc<AppState>) -> Router {
    super::ops::warm_up();
    Router::new().fallback(handle).with_state(state)
}

/// Minimum API-key pepper length in production.
pub const MIN_PEPPER_BYTES: usize = 32;

/// An API key file that is re-read when its mtime changes (checked at most
/// once a second), so revocations apply without a restart. A reload that
/// fails (bad JSON, bad mode) keeps the previous key set and logs a warning.
pub struct ApiKeyFile {
    path: std::path::PathBuf,
    state: std::sync::Mutex<ApiKeyFileState>,
}

struct ApiKeyFileState {
    mtime: Option<std::time::SystemTime>,
    checked: std::time::Instant,
    keys: Arc<Vec<StoredKey>>,
}

impl ApiKeyFile {
    /// Loads the file; a bad file at startup is an error.
    pub fn open(path: std::path::PathBuf) -> Result<Self, String> {
        let keys = load_api_keys(&path)?;
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Ok(Self {
            path,
            state: std::sync::Mutex::new(ApiKeyFileState {
                mtime,
                checked: std::time::Instant::now(),
                keys: Arc::new(keys),
            }),
        })
    }

    /// The current key set, reloading first if the file changed.
    pub fn keys(&self) -> Arc<Vec<StoredKey>> {
        self.keys_at(std::time::Instant::now())
    }

    fn keys_at(&self, now: std::time::Instant) -> Arc<Vec<StoredKey>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if now.duration_since(state.checked) >= std::time::Duration::from_secs(1) {
            state.checked = now;
            self.reload_if_changed(&mut state);
        }
        state.keys.clone()
    }

    /// Forces a re-read now (tests, signals), ignoring the mtime throttle.
    pub fn reload(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.checked = std::time::Instant::now();
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        match load_api_keys(&self.path) {
            Ok(keys) => {
                state.keys = Arc::new(keys);
                state.mtime = mtime;
            }
            Err(err) => {
                tracing::warn!(%err, "API key reload failed; keeping previous keys");
                // Remember the bad mtime so the periodic path does not spin.
                if mtime.is_some() {
                    state.mtime = mtime;
                }
            }
        }
    }

    fn reload_if_changed(&self, state: &mut ApiKeyFileState) {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if mtime.is_some() && mtime == state.mtime {
            return;
        }
        match load_api_keys(&self.path) {
            Ok(keys) => {
                state.keys = Arc::new(keys);
                state.mtime = mtime;
            }
            Err(err) => tracing::warn!(%err, "API key reload failed; keeping previous keys"),
        }
    }
}

/// Loads stored API keys from a JSON array of [`StoredKey`] records (secrets
/// are HMAC hashes, never plaintext). The file must not be writable by group
/// or others, nor readable by others, and every record must be well formed.
pub fn load_api_keys(path: &std::path::Path) -> Result<Vec<StoredKey>, String> {
    let shown = path.display();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|err| format!("VIBE_API_KEYS_FILE {shown}: {err}"))?
            .permissions()
            .mode();
        if mode & 0o026 != 0 {
            return Err(format!(
                "VIBE_API_KEYS_FILE {shown} has mode {:o}; it must not be group/other writable or other readable (chmod 600)",
                mode & 0o777
            ));
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("VIBE_API_KEYS_FILE {shown}: {err}"))?;
    let keys: Vec<StoredKey> =
        serde_json::from_str(&text).map_err(|err| format!("VIBE_API_KEYS_FILE {shown}: {err}"))?;
    let mut seen = HashSet::new();
    for key in &keys {
        let ok_id = key.key_id.len() == 12
            && key
                .key_id
                .chars()
                .all(|c| matches!(c, 'a'..='z' | '2'..='7'));
        let ok_hash =
            key.secret_hash.len() == 64 && key.secret_hash.chars().all(|c| c.is_ascii_hexdigit());
        if !ok_id
            || !ok_hash
            || !matches!(key.env.as_str(), "live" | "test")
            || !super::slugs::valid_tenant(&key.tenant_id)
        {
            return Err(format!(
                "VIBE_API_KEYS_FILE {shown}: invalid record for key_id {:?}",
                key.key_id
            ));
        }
        if !seen.insert(key.key_id.clone()) {
            return Err(format!(
                "VIBE_API_KEYS_FILE {shown}: duplicate key_id {}",
                key.key_id
            ));
        }
    }
    Ok(keys)
}

/// Deployment environment, from `VIBE_ENV` (case-insensitive). This is the
/// single place that decides "is this production?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnv {
    Dev,
    Ci,
    Production,
}

impl RunEnv {
    /// `dev`, `development`, `local` and `test` are development; `ci` is CI;
    /// anything else (`live`, `production`, `prod`, typos, ...) is production.
    /// Unset means development only for the in-memory executor and
    /// production otherwise (fail closed for a real database).
    pub fn from_var(raw: Option<&str>, memory: bool) -> Self {
        match raw.map(|v| v.trim().to_ascii_lowercase()) {
            None => {
                if memory {
                    RunEnv::Dev
                } else {
                    RunEnv::Production
                }
            }
            Some(v) if v.is_empty() => {
                if memory {
                    RunEnv::Dev
                } else {
                    RunEnv::Production
                }
            }
            Some(v) if matches!(v.as_str(), "dev" | "development" | "local" | "test") => {
                RunEnv::Dev
            }
            Some(v) if v == "ci" => RunEnv::Ci,
            Some(_) => RunEnv::Production,
        }
    }

    pub fn is_dev(self) -> bool {
        self == RunEnv::Dev
    }
}

/// Resolves the API-key pepper. Outside development a missing pepper or one
/// shorter than [`MIN_PEPPER_BYTES`] bytes is an error; in development the
/// public dev pepper is used with a warning.
pub fn resolve_pepper(
    env: RunEnv,
    pepper: Option<&str>,
) -> Result<(Vec<u8>, Option<String>), String> {
    let strict = !env.is_dev();
    match pepper {
        Some(p) if p.len() >= MIN_PEPPER_BYTES => Ok((p.as_bytes().to_vec(), None)),
        Some(_) if strict => Err(format!(
            "VIBE_API_KEY_PEPPER must be at least {MIN_PEPPER_BYTES} bytes outside development (generate one with `openssl rand -base64 48`)"
        )),
        Some(p) => Ok((
            p.as_bytes().to_vec(),
            Some(format!("VIBE_API_KEY_PEPPER is shorter than {MIN_PEPPER_BYTES} bytes")),
        )),
        None if strict => Err(
            "VIBE_API_KEY_PEPPER is required outside development (set VIBE_ENV=dev for local use, or generate one with `openssl rand -base64 48`)".to_string(),
        ),
        None => Ok((
            b"dev-pepper".to_vec(),
            Some("VIBE_API_KEY_PEPPER is not set; using the public dev pepper".to_string()),
        )),
    }
}

struct Principal {
    tenant_id: String,
    subject: String,
    roles: Vec<String>,
    scopes: Vec<String>,
}

async fn handle(State(state): State<Arc<AppState>>, req: Request<Body>) -> HttpResponse {
    let started = Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    if path == "/health" {
        return json_response(StatusCode::OK, json!({"status": "ok"}), &request_id, &[]);
    }
    let matched = match_route(&state.manifest.routes, &method, &path);
    let Some(found) = matched else {
        let allowed = allow_header(&state.manifest.routes, &path);
        if allowed.is_empty() {
            return error_response(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Not found",
                &request_id,
                &[],
            );
        }
        return error_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "Method not allowed",
            &request_id,
            &[("allow", allowed.as_str())],
        );
    };
    if state.disabled.contains(&found.route.feature_id) {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "ENDPOINT_DISABLED",
                "Endpoint disabled",
                &request_id,
                &[("retry-after", "30")],
            ),
        )
        .await;
    }
    let query = parse_query(req.uri().query().unwrap_or(""));
    if query
        .keys()
        .any(|key| matches!(key.as_str(), "api_key" | "api-key" | "key"))
    {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                "API keys are not accepted in the query",
                &request_id,
                &[],
            ),
        )
        .await;
    }
    let headers = header_map(req.headers());
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let ip = client_ip(&headers, peer, &state.trusted_proxies, state.honor_test_ip);
    let body_limit = found.route.body_limit as usize;
    let bytes = match axum::body::to_bytes(req.into_body(), body_limit.saturating_add(1)).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return finish(
                found.route,
                &state,
                started,
                error_response(
                    StatusCode::BAD_REQUEST,
                    "BAD_REQUEST",
                    "Could not read the body",
                    &request_id,
                    &[],
                ),
            )
            .await;
        }
    };
    if bytes.len() > body_limit {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "Body is too large",
                &request_id,
                &[],
            ),
        )
        .await;
    }
    let content_type = headers.get("content-type").cloned().unwrap_or_default();
    let body = if bytes.is_empty() {
        Value::Null
    } else if matches!(method.as_str(), "POST" | "PUT")
        && !content_type
            .to_ascii_lowercase()
            .starts_with("application/json")
    {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "UNSUPPORTED_MEDIA_TYPE",
                "Content-Type must be application/json",
                &request_id,
                &[],
            ),
        )
        .await;
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return finish(
                    found.route,
                    &state,
                    started,
                    error_response(
                        StatusCode::BAD_REQUEST,
                        "BAD_REQUEST",
                        "Body is not JSON",
                        &request_id,
                        &[],
                    ),
                )
                .await;
            }
        }
    };
    let principal = if found.route.public && found.route.auth_removed {
        let slug = found.params.get("slug").cloned().unwrap_or_default();
        match state.slugs.get(&slug) {
            Some(entry)
                if entry.public_access && (!found.route.public_writes || entry.public_write) =>
            {
                Principal {
                    tenant_id: entry.tenant_id.clone(),
                    subject: String::new(),
                    roles: Vec::new(),
                    scopes: Vec::new(),
                }
            }
            _ => {
                let unknown = !state.slugs.contains_key(&slug);
                let message = if state.dev_hints && unknown {
                    format!("Not found. Unknown slug `{slug}`; run `vibe slug add {slug} --tenant <id>` or copy slugs.example.json to .vibe/slugs.json")
                } else {
                    "Not found".into()
                };
                return finish(
                    found.route,
                    &state,
                    started,
                    error_response(
                        StatusCode::NOT_FOUND,
                        "NOT_FOUND",
                        &message,
                        &request_id,
                        &[],
                    ),
                )
                .await;
            }
        }
    } else {
        match authenticate(&state.auth, found.route, &headers).await {
            Ok(principal) => principal,
            Err(response) => {
                return finish(
                    found.route,
                    &state,
                    started,
                    response.with_request(&request_id),
                )
                .await
            }
        }
    };
    if !found.route.roles.is_empty()
        && !found
            .route
            .roles
            .iter()
            .any(|role| principal.roles.iter().any(|have| have == role))
    {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "Forbidden",
                &request_id,
                &[],
            ),
        )
        .await;
    }
    let scope_ok = if found.route.auth_mode == "api-key" {
        let definition = found.route.feature_id.split('/').next().unwrap_or("");
        let write = found.route.method != "GET";
        scope_allows(&principal.scopes, definition, write)
    } else {
        true
    };
    // Declared route scopes: the caller must hold every one of them.
    if !scope_ok || !route_scopes_allow(&principal.scopes, &found.route.scopes) {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "Forbidden",
                &request_id,
                &[],
            ),
        )
        .await;
    }
    if let Some(retry) = rate_limited(&state, found.route, &ip, &principal, &body) {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Too many requests",
                &request_id,
                &[("retry-after", retry.as_str())],
            ),
        )
        .await;
    }
    // Validate only after authentication and authorization, so unauthenticated
    // callers cannot probe the request schema of protected endpoints.
    if let Err(fail) = validate_request(found.route, &body, &query, &found.params, &headers) {
        return finish(
            found.route,
            &state,
            started,
            fail_response(found.route, fail, &request_id),
        )
        .await;
    }
    if found.route.public_writes && quota_exhausted(&state, &principal.tenant_id, &ip) {
        let retry = seconds_until_utc_midnight().to_string();
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Public write quota is exhausted",
                &request_id,
                &[("retry-after", retry.as_str())],
            ),
        )
        .await;
    }
    // The challenge runs after the rate limit and the public write quota, so
    // limited requests never reach the replay cache or siteverify.
    if found.route.public
        && found.route.challenge_type != "none"
        && found.route.challenge_when == "always"
    {
        if let Some(fail) = check_challenge(
            state.challenge.as_ref(),
            found.route,
            headers.get("x-challenge-proof"),
            &ip,
            &principal.tenant_id,
        )
        .await
        {
            return finish(found.route, &state, started, fail(&request_id)).await;
        }
    }
    let mut query_values = BTreeMap::new();
    for field in &found.route.request {
        if field.location == "query" {
            if let Some(raw) = query.get(&field.name) {
                query_values.insert(field.name.clone(), coerce(raw, &field.ty));
            }
        }
    }
    let mut header_values = BTreeMap::new();
    for field in &found.route.request {
        if field.location == "header" {
            if let Some(value) = headers.get(&field.name.to_ascii_lowercase()) {
                header_values.insert(field.name.to_ascii_lowercase(), value.clone());
            }
        }
    }
    let req_value = crud::req_var(body, query_values, found.params.clone(), header_values);
    let mut vars = Map::new();
    vars.insert("req".into(), req_value);
    vars.insert(
        "ctx".into(),
        crud::ctx_var(&crud::CallCtx {
            auth: &state.auth,
            tenant_id: &principal.tenant_id,
            role: "",
            caller: crud::Caller {
                subject: &principal.subject,
                roles: &principal.roles,
            },
        }),
    );
    let input = match crud::eval_expr(&found.route.transform_in, &vars) {
        Ok(value) => value,
        Err(err) => {
            return finish(found.route, &state, started, {
                tracing::warn!(%request_id, error = %err, "transform.in failed");
                error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "VALIDATION_FAILED",
                    "Invalid request",
                    &request_id,
                    &[],
                )
            })
            .await;
        }
    };
    let role = if found.route.public {
        if found.route.public_writes {
            "public_writer"
        } else {
            "public_reader"
        }
    } else {
        "app_rw"
    };
    let mut result_value = Value::Null;
    let mut result_etag = None;
    let mut status = StatusCode::OK;
    for step in &found.route.steps {
        match step.op.as_str() {
            "method" => {
                let args = if step.args == "transform.in" || step.args.is_empty() {
                    input.clone()
                } else {
                    vars.insert("input".into(), input.clone());
                    match crud::eval_expr(&step.args, &vars) {
                        Ok(value) => value,
                        Err(err) => {
                            return finish(found.route, &state, started, {
                                tracing::warn!(%request_id, error = %err, "step args failed");
                                error_response(
                                    StatusCode::UNPROCESSABLE_ENTITY,
                                    "VALIDATION_FAILED",
                                    "Invalid request",
                                    &request_id,
                                    &[],
                                )
                            })
                            .await;
                        }
                    }
                };
                match crud::run_method(
                    &state.manifest,
                    state.exec.as_ref(),
                    &step.target,
                    &args,
                    crud::CallCtx {
                        auth: &state.auth,
                        tenant_id: &principal.tenant_id,
                        role,
                        caller: crud::Caller {
                            subject: &principal.subject,
                            roles: &principal.roles,
                        },
                    },
                    1,
                )
                .await
                {
                    Ok(result) => {
                        result_etag = result.etag.or(result_etag);
                        if !step.out.is_empty() {
                            vars.insert(step.out.clone(), result.value.clone());
                        }
                        result_value = result.value;
                        if result.status == 204 {
                            status = StatusCode::NO_CONTENT;
                        }
                    }
                    Err(fail) => {
                        return finish(
                            found.route,
                            &state,
                            started,
                            fail_response(found.route, fail, &request_id),
                        )
                        .await;
                    }
                }
            }
            "respond" => {
                let value = if step.value == "transform.out" || step.value.is_empty() {
                    match crud::eval_expr(&found.route.transform_out, &vars) {
                        Ok(value) => value,
                        Err(_) => result_value.clone(),
                    }
                } else {
                    result_value.clone()
                };
                result_value = value;
                if step.status != 0 {
                    status = StatusCode::from_u16(step.status).unwrap_or(StatusCode::OK);
                }
            }
            "validate" => {
                vars.insert("input".into(), input.clone());
                if crud::when_matches(&step.when_expr, &vars) {
                    let code = if step.fail.is_empty() {
                        "VALIDATION_FAILED".to_string()
                    } else {
                        step.fail.clone()
                    };
                    let mapped = found.route.errors.iter().find(|e| e.from == code);
                    let status = mapped
                        .and_then(|e| StatusCode::from_u16(e.status).ok())
                        .unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
                    let public_code = mapped.map(|e| e.code.clone()).unwrap_or(code);
                    return finish(
                        found.route,
                        &state,
                        started,
                        error_response(status, &public_code, "Invalid request", &request_id, &[]),
                    )
                    .await;
                }
            }
            "transform" => {}
            _ => {
                return finish(
                    found.route,
                    &state,
                    started,
                    error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL",
                        "Unsupported api step",
                        &request_id,
                        &[],
                    ),
                )
                .await;
            }
        }
    }
    tracing::info!(
        request_id = %request_id,
        route = %found.route.feature_id,
        status = status.as_u16(),
        tenant = %principal.tenant_id,
        subject = %principal.subject,
        "request"
    );
    let mut extra = Vec::new();
    if let Some(tag) = result_etag {
        extra.push(("etag".into(), tag));
    }
    if status == StatusCode::CREATED {
        if let Some(id) = result_value.get("id").and_then(Value::as_str) {
            extra.push(("location".into(), format!("{path}/{id}")));
        }
    }
    if found.route.deprecated {
        extra.push(("deprecation".into(), "true".into()));
        if found.route.sunset != "(none)" && !found.route.sunset.is_empty() {
            extra.push(("sunset".into(), found.route.sunset.clone()));
        }
    }
    let response = if status == StatusCode::NO_CONTENT {
        empty_response(status, &request_id, &extra)
    } else {
        json_response(status, result_value, &request_id, &extra)
    };
    finish(found.route, &state, started, response).await
}

async fn finish(
    route: &RouteSpec,
    state: &AppState,
    started: Instant,
    response: HttpResponse,
) -> HttpResponse {
    let min_ms = state.min_override_ms.unwrap_or(if route.public {
        // Login-class endpoints (a verify-password step) pad to 500 ms so
        // timing does not reveal whether the account exists.
        if crud::route_verifies_password(&state.manifest, route) {
            route.min_response_ms.max(500)
        } else {
            route.min_response_ms
        }
    } else {
        0
    });
    let elapsed = started.elapsed();
    if min_ms > 0 {
        let min = Duration::from_millis(min_ms);
        if elapsed < min {
            tokio::time::sleep(min - elapsed).await;
        }
    }
    response
}

struct Found<'a> {
    route: &'a RouteSpec,
    params: BTreeMap<String, String>,
}

fn match_route<'a>(routes: &'a [RouteSpec], method: &str, path: &str) -> Option<Found<'a>> {
    let path = normalize_request_path(path);
    for route in routes {
        if route.method != method {
            continue;
        }
        if let Some(params) = match_path(&route.path, &path) {
            return Some(Found { route, params });
        }
    }
    None
}

fn allow_header(routes: &[RouteSpec], path: &str) -> String {
    let path = normalize_request_path(path);
    let mut methods: Vec<&str> = routes
        .iter()
        .filter(|route| match_path(&route.path, &path).is_some())
        .map(|route| route.method.as_str())
        .collect();
    methods.sort_unstable();
    methods.dedup();
    methods.join(", ")
}

fn match_path(template: &str, path: &str) -> Option<BTreeMap<String, String>> {
    let template: Vec<&str> = template
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let path: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if template.len() != path.len() {
        return None;
    }
    let mut params = BTreeMap::new();
    for (pattern, value) in template.into_iter().zip(path) {
        if let Some(name) = pattern.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            params.insert(name.to_string(), value.to_string());
        } else if !pattern.eq_ignore_ascii_case(value) {
            return None;
        }
    }
    Some(params)
}

fn normalize_request_path(path: &str) -> String {
    let mut path = path.to_string();
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

/// Validates the request against the route's declared fields.
///
/// Key policy: only top-level body keys are declared and checked; a declared
/// field's value may be any JSON of its type, and nested objects/arrays are
/// not walked (types are scalar, so an object value fails the type check).
/// Duplicate JSON keys are resolved by the parser (serde_json keeps the last
/// occurrence) before validation; duplicate query keys likewise keep the last
/// value.
fn validate_request(
    route: &RouteSpec,
    body: &Value,
    query: &BTreeMap<String, String>,
    path: &BTreeMap<String, String>,
    headers: &BTreeMap<String, String>,
) -> Result<(), MethodFail> {
    let has_body_fields = route.request.iter().any(|field| field.location == "body");
    if !body.is_null() && !body.is_object() {
        return Err(MethodFail::code(
            "VALIDATION_FAILED",
            if has_body_fields {
                "body must be a JSON object"
            } else {
                "body is not allowed"
            },
        ));
    }
    if let Some(obj) = body.as_object() {
        for key in obj.keys() {
            if !route
                .request
                .iter()
                .any(|field| field.location == "body" && field.name == *key)
            {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("unknown field {key}"),
                ));
            }
        }
    }
    for key in query.keys() {
        if !route
            .request
            .iter()
            .any(|field| field.location == "query" && field.name == *key)
        {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("unknown query {key}"),
            ));
        }
    }
    for field in &route.request {
        let present = match field.location.as_str() {
            "body" => {
                body.get(&field.name).is_some()
                    && !body.get(&field.name).is_some_and(Value::is_null)
            }
            "query" => query.contains_key(&field.name),
            "path" => path.contains_key(&field.name),
            // An empty (or whitespace-only) header counts as missing.
            "header" => headers
                .get(&field.name.to_ascii_lowercase())
                .is_some_and(|v| !v.trim().is_empty()),
            _ => false,
        };
        if field.required && !present {
            return Err(MethodFail::code(
                "VALIDATION_FAILED",
                format!("{} is required", field.name),
            ));
        }
        if field.location == "body" {
            if let Some(value) = body.get(&field.name) {
                check_type(field, value)?;
            }
        }
        if field.location == "query" {
            if let Some(raw) = query.get(&field.name) {
                check_type(field, &coerce(raw, &field.ty))?;
            }
        }
        // Path params are typed too, so e.g. a malformed `path.id` uuid is a
        // 422 here and never reaches SQL.
        if field.location == "path" {
            if let Some(raw) = path.get(&field.name) {
                check_type(field, &coerce(raw, &field.ty))?;
            }
        }
    }
    Ok(())
}

fn check_type(field: &crate::manifest::ReqField, value: &Value) -> Result<(), MethodFail> {
    match field.ty.as_str() {
        "integer" => {
            let number = value.as_i64().ok_or_else(|| {
                MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be an integer", field.name),
                )
            })?;
            if let Some(min) = field.min {
                if number < min {
                    return Err(MethodFail::code(
                        "VALIDATION_FAILED",
                        format!("{} is below the minimum", field.name),
                    ));
                }
            }
            if let Some(max) = field.max {
                if number > max {
                    return Err(MethodFail::code(
                        "VALIDATION_FAILED",
                        format!("{} is above the maximum", field.name),
                    ));
                }
            }
        }
        "string" => {
            let text = value.as_str().ok_or_else(|| {
                MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be a string", field.name),
                )
            })?;
            if let Some(max) = field.max {
                if text.chars().count() as i64 > max {
                    return Err(MethodFail::code(
                        "VALIDATION_FAILED",
                        format!("{} is too long", field.name),
                    ));
                }
            }
            if field.format == "email" && !text.contains('@') {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be an email", field.name),
                ));
            }
        }
        "number" => {
            if !value.is_number() {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be a number", field.name),
                ));
            }
        }
        "boolean" => {
            if !value.is_boolean() {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be a boolean", field.name),
                ));
            }
        }
        "uuid" => {
            if value.as_str().is_none_or(|s| !is_canonical_uuid(s)) {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be a uuid", field.name),
                ));
            }
        }
        other => {
            // `vibe validate` rejects unknown types; an old manifest fails closed.
            return Err(MethodFail::code(
                "INTERNAL",
                format!("unsupported request type {other} for {}", field.name),
            ));
        }
    }
    Ok(())
}

/// Request field types the runtime understands.
pub const REQUEST_TYPES: [&str; 5] = ["string", "integer", "number", "boolean", "uuid"];

/// Only the canonical hyphenated form `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`
/// (either case); braces, `urn:uuid:` and the 32-hex simple form are rejected.
fn is_canonical_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn coerce(raw: &str, ty: &str) -> Value {
    match ty {
        "integer" => raw
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or(Value::String(raw.into())),
        "number" => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(raw.into())),
        // Only the exact strings `true` and `false`; anything else stays a
        // string and fails the boolean check.
        "boolean" => match raw {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(raw.into()),
        },
        _ => Value::String(raw.into()),
    }
}

async fn authenticate(
    auth: &AuthConfig,
    route: &RouteSpec,
    headers: &BTreeMap<String, String>,
) -> Result<Principal, AuthError> {
    if route.auth_mode == "api-key" {
        let key = headers
            .get("x-api-key")
            .cloned()
            .ok_or_else(|| AuthError::unauthorized("ApiKey"))?;
        return verify_api_key(auth, &key);
    }
    let header = headers.get("authorization").cloned().unwrap_or_default();
    let token = header
        .strip_prefix("Bearer ")
        .ok_or_else(|| AuthError::unauthorized("Bearer"))?;
    let roles_claim = if route.roles_claim.is_empty() {
        auth.roles_claim.as_str()
    } else {
        route.roles_claim.as_str()
    };
    let jwt_header =
        jsonwebtoken::decode_header(token).map_err(|_| AuthError::unauthorized("Bearer"))?;
    let kid = jwt_header
        .kid
        .clone()
        .ok_or_else(|| AuthError::unauthorized("Bearer"))?;
    let route_uri = route_jwks_uri(route);
    let (alg, key) = if let Some(uri) = &route_uri {
        // Route-level issuer: only its own keys, never the env key source.
        let jwks = auth
            .route_jwks
            .get(uri)
            .ok_or_else(|| AuthError::unauthorized("Bearer"))?;
        let found = jwks
            .key(&kid)
            .await
            .ok_or_else(|| AuthError::unauthorized("Bearer"))?;
        (found.alg, found.key)
    } else if let Some(jwks) = &auth.jwks {
        let found = jwks
            .key(&kid)
            .await
            .ok_or_else(|| AuthError::unauthorized("Bearer"))?;
        (found.alg, found.key)
    } else {
        if auth.public_pem.is_empty() || kid != auth.kid {
            return Err(AuthError::unauthorized("Bearer"));
        }
        let key = if auth.alg == Algorithm::RS256 {
            DecodingKey::from_rsa_pem(auth.public_pem.as_bytes())
        } else {
            DecodingKey::from_ec_pem(auth.public_pem.as_bytes())
        }
        .map_err(|_| AuthError::unauthorized("Bearer"))?;
        (auth.alg, key)
    };
    if jwt_header.alg != alg {
        return Err(AuthError::unauthorized("Bearer"));
    }
    let iss = if route.jwt_iss.is_empty() {
        auth.iss.as_str()
    } else {
        route.jwt_iss.as_str()
    };
    let aud = if route.jwt_aud.is_empty() {
        auth.aud.as_str()
    } else {
        route.jwt_aud.as_str()
    };
    verify_jwt(token, alg, &key, iss, aud, roles_claim)
}

fn verify_jwt(
    token: &str,
    alg: Algorithm,
    key: &DecodingKey,
    iss: &str,
    aud: &str,
    roles_claim: &str,
) -> Result<Principal, AuthError> {
    let mut validation = Validation::new(alg);
    validation.set_issuer(&[iss]);
    validation.set_audience(&[aud]);
    validation.leeway = 60;
    validation.validate_nbf = true;
    let data = jsonwebtoken::decode::<Claims>(token, key, &validation)
        .map_err(|_| AuthError::unauthorized("Bearer"))?;
    if data.claims.exp.saturating_sub(data.claims.iat) > 3600 {
        return Err(AuthError::unauthorized("Bearer"));
    }
    if !super::slugs::valid_tenant(&data.claims.tenant_id) {
        return Err(AuthError::unauthorized("Bearer"));
    }
    // A present but malformed roles claim is rejected, not treated as "no roles".
    let roles = roles_from_claims(&data.claims.extra, roles_claim)?;
    let scopes = scopes_from_claims(&data.claims.extra);
    Ok(Principal {
        tenant_id: data.claims.tenant_id,
        subject: data.claims.sub,
        roles,
        scopes,
    })
}

/// Walks a dotted path (`realm_access.roles`) or a URL-style claim key.
fn claim_value<'a>(extra: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    if path.contains("://") {
        return extra.get(path);
    }
    let mut parts = path.split('.');
    let first = parts.next()?;
    let mut cur = extra.get(first)?;
    for part in parts {
        cur = cur.as_object()?.get(part)?;
    }
    Some(cur)
}

fn roles_from_claims(
    extra: &HashMap<String, Value>,
    roles_claim: &str,
) -> Result<Vec<String>, AuthError> {
    match claim_value(extra, roles_claim) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| AuthError::unauthorized("Bearer")),
        Some(_) => Err(AuthError::unauthorized("Bearer")),
    }
}

/// OAuth-style `scope` (space-separated string) or `scp` (array or string).
fn scopes_from_claims(extra: &HashMap<String, Value>) -> Vec<String> {
    let from = |v: &Value| -> Option<Vec<String>> {
        match v {
            Value::String(s) => Some(s.split_whitespace().map(str::to_string).collect()),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>(),
            _ => None,
        }
    };
    if let Some(v) = extra.get("scope").and_then(from) {
        return v;
    }
    if let Some(v) = extra.get("scp").and_then(from) {
        return v;
    }
    Vec::new()
}

fn verify_api_key(auth: &AuthConfig, token: &str) -> Result<Principal, AuthError> {
    let mut parts = token.split('_');
    let prefix = parts.next();
    let env = parts.next();
    let key_id = parts.next();
    let secret = parts.next();
    if prefix != Some("mbk") || parts.next().is_some() {
        return Err(AuthError::unauthorized("ApiKey"));
    }
    let (Some(env), Some(key_id), Some(secret)) = (env, key_id, secret) else {
        return Err(AuthError::unauthorized("ApiKey"));
    };
    if !matches!(env, "live" | "test")
        || key_id.len() != 12
        || !key_id.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7'))
    {
        return Err(AuthError::unauthorized("ApiKey"));
    }
    if env != auth.env_name {
        return Err(AuthError::unauthorized("ApiKey"));
    }
    let hash = hmac_hex(&auth.pepper, secret);
    let current = auth.api_keys_file.as_ref().map(|file| file.keys());
    let keys: &[StoredKey] = current.as_deref().map_or(&auth.api_keys, |k| k.as_slice());
    let Some(record) = keys.iter().find(|key| key.key_id == key_id) else {
        return Err(AuthError::unauthorized("ApiKey"));
    };
    if !ct_eq(&record.secret_hash, &hash) || record.revoked || record.env != env {
        return Err(AuthError::unauthorized("ApiKey"));
    }
    if record.expires_unix < chrono::Utc::now().timestamp() {
        return Err(AuthError::unauthorized("ApiKey"));
    }
    if !super::slugs::valid_tenant(&record.tenant_id) {
        return Err(AuthError::unauthorized("ApiKey"));
    }
    Ok(Principal {
        tenant_id: record.tenant_id.clone(),
        subject: record.key_id.clone(),
        roles: Vec::new(),
        scopes: record.scopes.clone(),
    })
}

fn hmac_hex(pepper: &[u8], secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(pepper).expect("hmac key");
    mac.update(secret.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn ct_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

/// Picks ES256 or RS256 from a public PEM. `VIBE_JWT_ALG` can override this.
pub fn detect_jwt_alg(pem: &str) -> Result<Algorithm, String> {
    if DecodingKey::from_rsa_pem(pem.as_bytes()).is_ok() {
        return Ok(Algorithm::RS256);
    }
    if DecodingKey::from_ec_pem(pem.as_bytes()).is_ok() {
        return Ok(Algorithm::ES256);
    }
    Err("VIBE_JWT_PUBLIC_KEY_PEM is not a usable RSA or EC public key".into())
}

pub fn hash_api_secret(pepper: &[u8], secret: &str) -> String {
    hmac_hex(pepper, secret)
}

/// Every scope declared on the route must be held, exactly or through a
/// `definition:*` / `definition:admin` grant (and `:write` covers `:read`).
fn route_scopes_allow(have: &[String], required: &[String]) -> bool {
    required.iter().all(|need| {
        have.iter().any(|h| {
            if h == need {
                return true;
            }
            match (h.split_once(':'), need.split_once(':')) {
                (Some((hd, ha)), Some((nd, na))) if hd == nd => {
                    matches!(ha, "*" | "admin") || (ha == "write" && na == "read")
                }
                _ => false,
            }
        })
    })
}

fn scope_allows(scopes: &[String], definition: &str, write: bool) -> bool {
    scopes.iter().any(|scope| {
        let Some((def, action)) = scope.split_once(':') else {
            return false;
        };
        if def != definition {
            return false;
        }
        match action {
            "*" | "admin" | "write" => true,
            "read" => !write,
            _ => false,
        }
    })
}

/// Shard and bucket for one rate-limit step. Shards are per route and key
/// type, so flooding one key type cannot evict another's buckets.
fn rate_bucket(
    key: &str,
    route: &RouteSpec,
    ip: &str,
    principal: &Principal,
    body: &Value,
    pepper: &[u8],
) -> (String, String) {
    let tenant = &principal.tenant_id;
    let feature = &route.feature_id;
    let field = match key {
        "tenant" => return (format!("{feature}:tenant"), tenant.clone()),
        "user" | "api-key" => {
            return (
                format!("{feature}:{key}"),
                format!("{tenant}:{}", principal.subject),
            )
        }
        // Deprecated alias (login-guard `account-per-minute`) for input.email.
        "account" => Some("email"),
        _ => key.strip_prefix("input."),
    };
    match field {
        Some(field) => {
            // Normalize, then HMAC so the plaintext is never stored (§4.3).
            // A missing field normalizes to "" so all requests without it share
            // one bucket per tenant: omitting the field cannot dodge the limit.
            let raw = body
                .get(field)
                .map(|v| match v {
                    Value::String(s) => s.trim().to_lowercase(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            (
                format!("{feature}:input.{field}"),
                format!("{tenant}:{}", hmac_hex(pepper, &raw)),
            )
        }
        None => (format!("{feature}:ip"), ip.to_string()),
    }
}

fn rate_limited(
    state: &AppState,
    route: &RouteSpec,
    ip: &str,
    principal: &Principal,
    body: &Value,
) -> Option<String> {
    if route.rates.is_empty() {
        return None;
    }
    let checks: Vec<(String, String, u32)> = route
        .rates
        .iter()
        .enumerate()
        .map(|(index, rate)| {
            let (shard, key) =
                rate_bucket(&rate.key, route, ip, principal, body, &state.auth.pepper);
            // Each block is its own limit, even when two share a key.
            (format!("{shard}#{index}"), key, rate.per_minute)
        })
        .collect();
    let Ok(mut rates) = state.rates.lock() else {
        return Some("1".into());
    };
    match rates.check(&checks, &owner_of(ip), Instant::now()) {
        RateDecision::Allow => None,
        RateDecision::Limited(wait) => Some(wait.to_string()),
    }
}

fn seconds_until_utc_midnight() -> i64 {
    let now = chrono::Utc::now();
    let tomorrow = (now.date_naive() + chrono::Days::new(1))
        .and_hms_opt(0, 0, 0)
        .expect("midnight")
        .and_utc();
    (tomorrow - now).num_seconds().max(1)
}

fn quota_exhausted(state: &AppState, tenant: &str, ip: &str) -> bool {
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let Ok(mut quotas) = state.quotas.lock() else {
        return true;
    };
    !quotas.check(
        &day,
        &[
            ("tenant", tenant.to_string(), 1000),
            ("ip", ip.to_string(), 50),
        ],
    )
}

/// Client IP bucket: the peer address, unless the peer is a trusted proxy, in
/// which case the rightmost `X-Forwarded-For` hop that is not a trusted proxy.
/// IPv4-mapped IPv6 becomes IPv4; IPv6 is grouped by /64.
fn client_ip(
    headers: &BTreeMap<String, String>,
    peer: Option<IpAddr>,
    trusted: &[ProxyNet],
    honor_test_ip: bool,
) -> String {
    if honor_test_ip {
        if let Some(ip) = headers.get("x-vibe-test-ip") {
            return ip.clone();
        }
    }
    let Some(peer) = peer.map(|p| p.to_canonical()) else {
        return "0.0.0.0".into();
    };
    let is_trusted = |ip: IpAddr| trusted.iter().any(|net| net.contains(ip));
    if !is_trusted(peer) {
        return ip_bucket(peer);
    }
    let Some(xff) = headers.get("x-forwarded-for") else {
        return ip_bucket(peer);
    };
    let mut last = peer;
    for hop in xff.rsplit(',') {
        match parse_hop(hop) {
            Some(ip) if is_trusted(ip) => last = ip,
            Some(ip) => return ip_bucket(ip),
            None => break,
        }
    }
    ip_bucket(last)
}

fn header_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_ascii_lowercase(), v.to_string()))
        })
        .collect()
}

fn parse_query(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if raw.is_empty() {
        return out;
    }
    for pair in raw.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(decode(key), decode(value));
    }
    out
}

fn decode(raw: &str) -> String {
    let replaced = raw.replace('+', " ");
    percent_decode(&replaced)
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    exp: i64,
    iat: i64,
    #[allow(dead_code)]
    nbf: Option<i64>,
    tenant_id: String,
    /// All other claims, including the configured roles claim and `scope`.
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

struct AuthError {
    scheme: String,
    /// Operator-facing reason; never sent to the client.
    reason: &'static str,
}

impl AuthError {
    fn unauthorized(scheme: &str) -> Self {
        Self::fail(scheme, "unauthorized")
    }

    fn fail(scheme: &str, reason: &'static str) -> Self {
        Self {
            scheme: scheme.into(),
            reason,
        }
    }

    fn with_request(self, request_id: &str) -> HttpResponse {
        tracing::debug!(
            request_id,
            scheme = %self.scheme,
            reason = self.reason,
            "auth rejected"
        );
        let header = format!("{} realm=\"vibe\"", self.scheme);
        error_response(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "Unauthorized",
            request_id,
            &[("www-authenticate", header.as_str())],
        )
    }
}

/// `Retry-After` (seconds) sent with 503 `DB_POOL_EXHAUSTED`.
pub const DB_POOL_RETRY_AFTER_SECS: &str = "1";

fn fail_response(route: &RouteSpec, fail: MethodFail, request_id: &str) -> HttpResponse {
    let mut status = platform_status(&fail.code);
    if fail.code == "DB_POOL_EXHAUSTED" {
        // Platform condition: always 503 with Retry-After, never remapped.
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &fail.code,
            &fail.message,
            request_id,
            &[("retry-after", DB_POOL_RETRY_AFTER_SECS)],
        );
    }
    if let Some(mapped) = route.errors.iter().find(|err| err.code == fail.code) {
        status = StatusCode::from_u16(mapped.status).unwrap_or(status);
    }
    if status == StatusCode::UNAUTHORIZED {
        error_response(
            status,
            &fail.code,
            &public_message(&fail),
            request_id,
            &[("www-authenticate", "Bearer realm=\"vibe\"")],
        )
    } else {
        error_response(status, &fail.code, &public_message(&fail), request_id, &[])
    }
}

fn public_message(fail: &MethodFail) -> String {
    if fail.code == "INTERNAL" {
        "Internal error".into()
    } else {
        fail.message.clone()
    }
}

fn platform_status(code: &str) -> StatusCode {
    match code {
        "VALIDATION_FAILED" | "WEAK_PASSWORD" => StatusCode::UNPROCESSABLE_ENTITY,
        "PRECONDITION_REQUIRED" => StatusCode::PRECONDITION_REQUIRED,
        "PRECONDITION_FAILED" => StatusCode::PRECONDITION_FAILED,
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "CONFLICT" => StatusCode::CONFLICT,
        "FORBIDDEN" => StatusCode::FORBIDDEN,
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "BAD_REQUEST" => StatusCode::BAD_REQUEST,
        "DB_POOL_EXHAUSTED" => StatusCode::SERVICE_UNAVAILABLE,
        "SERVICE_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

type FailFn = Box<dyn FnOnce(&str) -> HttpResponse + Send>;

/// Verifies the challenge proof of a `when: always` route. Fails closed: no
/// proof is 400, no verifier, a `pow` route or an unreachable provider is 503,
/// a rejected proof is 403.
async fn check_challenge(
    verifier: Option<&Arc<dyn super::challenge::ChallengeVerifier>>,
    route: &crate::manifest::RouteSpec,
    proof: Option<&String>,
    ip: impl std::fmt::Display,
    tenant: &str,
) -> Option<FailFn> {
    fn fail(status: StatusCode, code: &'static str, message: &'static str) -> Option<FailFn> {
        Some(Box::new(move |request_id: &str| {
            error_response(status, code, message, request_id, &[])
        }))
    }
    let token = match proof.map(|v| v.trim()) {
        Some(token) if !token.is_empty() => token,
        _ => {
            return fail(
                StatusCode::BAD_REQUEST,
                "CHALLENGE_REQUIRED",
                "A challenge proof is required",
            )
        }
    };
    let verifier = match (verifier, route.challenge_type.as_str()) {
        (Some(verifier), "captcha") => verifier,
        _ => {
            tracing::warn!(route = %route.feature_id, kind = %route.challenge_type, "no challenge verifier configured; refusing");
            return fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHALLENGE_UNAVAILABLE",
                "Challenge verification is unavailable",
            );
        }
    };
    let ctx = super::challenge::ChallengeContext {
        action: super::challenge::challenge_action(&route.feature_id),
        remote_ip: Some(ip.to_string()),
        tenant: Some(tenant.to_string()).filter(|t| !t.is_empty()),
    };
    match verifier.verify(token, &ctx).await {
        Ok(()) => None,
        Err(super::challenge::ChallengeError::Rejected(why)) => {
            tracing::info!(route = %route.feature_id, %why, "challenge rejected");
            fail(
                StatusCode::FORBIDDEN,
                "CHALLENGE_FAILED",
                "The challenge proof is invalid",
            )
        }
        Err(super::challenge::ChallengeError::Unavailable(why)) => {
            tracing::warn!(route = %route.feature_id, %why, "challenge provider unavailable");
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHALLENGE_UNAVAILABLE",
                "Challenge verification is unavailable",
            )
        }
    }
}

fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
    request_id: &str,
    extra: &[(&str, &str)],
) -> HttpResponse {
    let owned: Vec<(String, String)> = extra
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    json_response(
        status,
        json!({"error": {"code": code, "message": message, "request_id": request_id}}),
        request_id,
        &owned,
    )
}

fn json_response(
    status: StatusCode,
    value: Value,
    request_id: &str,
    extra: &[(String, String)],
) -> HttpResponse {
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-request-id", request_id);
    for (key, value) in extra {
        response = response.header(key.as_str(), value.as_str());
    }
    response
        .body(Body::from(serde_json::to_vec(&value).unwrap_or_default()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn empty_response(
    status: StatusCode,
    request_id: &str,
    extra: &[(String, String)],
) -> HttpResponse {
    let mut response = Response::builder()
        .status(status)
        .header("x-request-id", request_id);
    for (key, value) in extra {
        response = response.header(key.as_str(), value);
    }
    response
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod rate_tests {
    use super::*;
    use crate::runtime::challenge::{ChallengeContext, ChallengeError, ChallengeVerifier};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn xff(v: &str) -> BTreeMap<String, String> {
        let mut h = BTreeMap::new();
        h.insert("x-forwarded-for".to_string(), v.to_string());
        h
    }

    #[test]
    fn xff_ignored_without_trusted_proxy() {
        let got = client_ip(&xff("1.2.3.4"), Some(ip("9.9.9.9")), &[], false);
        assert_eq!(got, "9.9.9.9");
    }

    #[test]
    fn xff_rightmost_untrusted_hop_with_cidr_ports_and_mapped() {
        let trusted = [ProxyNet::parse("10.0.0.0/8").unwrap()];
        let got = client_ip(
            &xff("6.6.6.6, 5.5.5.5:4321, [::ffff:10.0.0.2]:80"),
            Some(ip("::ffff:10.0.0.1")),
            &trusted,
            false,
        );
        assert_eq!(got, "5.5.5.5", "spoofed leftmost entry must be ignored");
    }

    #[test]
    fn ipv6_clients_share_a_64() {
        let a = client_ip(&BTreeMap::new(), Some(ip("2001:db8:0:1::a")), &[], false);
        let b = client_ip(&BTreeMap::new(), Some(ip("2001:db8:0:1::b")), &[], false);
        assert_eq!(a, b);
    }

    #[test]
    fn buckets_follow_the_key() {
        let route = test_route();
        let p = Principal {
            tenant_id: "tenant0001".into(),
            subject: "u1".into(),
            roles: vec![],
            scopes: vec![],
        };
        let body = json!({"email": " A@X.COM "});
        let pep = b"p";
        let (shard, key) = rate_bucket("user", &route, "1.1.1.1", &p, &body, pep);
        assert_eq!(shard, "f:user");
        assert!(key.ends_with(":u1"));
        assert_eq!(
            rate_bucket("tenant", &route, "1.1.1.1", &p, &body, pep).0,
            "f:tenant"
        );
        let a = rate_bucket("input.email", &route, "1.1.1.1", &p, &body, pep);
        let b = rate_bucket(
            "input.email",
            &route,
            "2.2.2.2",
            &p,
            &json!({"email": "a@x.com"}),
            pep,
        );
        assert_eq!(a, b, "normalized input key is IP independent");
        assert!(!a.1.contains("a@x.com"), "plaintext must not be stored");
        assert_eq!(a, rate_bucket("account", &route, "3.3.3.3", &p, &body, pep));
        assert_eq!(
            rate_bucket("ip", &route, "1.1.1.1", &p, &body, pep),
            ("f:ip".to_string(), "1.1.1.1".to_string())
        );
    }

    #[test]
    fn missing_input_fields_share_one_empty_bucket() {
        let route = test_route();
        let p = Principal {
            tenant_id: "tenant0001".into(),
            subject: String::new(),
            roles: vec![],
            scopes: vec![],
        };
        let a = rate_bucket("input.email", &route, "1.1.1.1", &p, &json!({}), b"p");
        let b = rate_bucket("input.email", &route, "9.9.9.9", &p, &Value::Null, b"p");
        let c = rate_bucket(
            "input.email",
            &route,
            "8.8.8.8",
            &p,
            &json!({"email": ""}),
            b"p",
        );
        assert_eq!(a, b);
        assert_eq!(a, c, "missing and empty share the same bucket");
    }

    struct FakeVerifier(Result<(), ChallengeError>);

    #[async_trait::async_trait]
    impl ChallengeVerifier for FakeVerifier {
        fn provider(&self) -> &'static str {
            "fake"
        }
        async fn verify(&self, token: &str, ctx: &ChallengeContext) -> Result<(), ChallengeError> {
            assert_eq!(token, "tok");
            assert_eq!(ctx.action, "register");
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn challenge_gate_fails_closed() {
        let mut route = test_route();
        route.feature_id = "shop/v1/register".into();
        route.challenge_type = "captcha".into();
        route.challenge_when = "always".into();
        let status = |f: Option<FailFn>| f.map(|f| f("rid").status());
        let ok: Arc<dyn ChallengeVerifier> = Arc::new(FakeVerifier(Ok(())));
        let tok = "tok".to_string();
        let blank = "  ".to_string();
        // Missing or blank proof.
        assert_eq!(
            status(check_challenge(Some(&ok), &route, None, "1.2.3.4", "tenant0001").await),
            Some(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            status(check_challenge(Some(&ok), &route, Some(&blank), "1.2.3.4", "tenant0001").await),
            Some(StatusCode::BAD_REQUEST)
        );
        // No verifier configured: an arbitrary proof no longer passes.
        assert_eq!(
            status(check_challenge(None, &route, Some(&tok), "1.2.3.4", "tenant0001").await),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
        // Verified.
        assert_eq!(
            status(check_challenge(Some(&ok), &route, Some(&tok), "1.2.3.4", "tenant0001").await),
            None
        );
        // Rejected and unavailable.
        let bad: Arc<dyn ChallengeVerifier> =
            Arc::new(FakeVerifier(Err(ChallengeError::Rejected("x".into()))));
        assert_eq!(
            status(check_challenge(Some(&bad), &route, Some(&tok), "1.2.3.4", "tenant0001").await),
            Some(StatusCode::FORBIDDEN)
        );
        let down: Arc<dyn ChallengeVerifier> =
            Arc::new(FakeVerifier(Err(ChallengeError::Unavailable("x".into()))));
        assert_eq!(
            status(check_challenge(Some(&down), &route, Some(&tok), "1.2.3.4", "tenant0001").await),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
        // `pow` has no verifier in v1: fail closed.
        route.challenge_type = "pow".into();
        assert_eq!(
            status(check_challenge(Some(&ok), &route, Some(&tok), "1.2.3.4", "tenant0001").await),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
    }

    fn test_route() -> RouteSpec {
        serde_json::from_value(json!({
            "method": "GET", "path": "/", "feature_id": "f", "public": false,
            "auth_removed": false, "auth_mode": "jwt", "roles": [], "scopes": [],
            "rates": [], "body_limit": 0, "request": [], "transform_in": "",
            "transform_out": "", "steps": [], "errors": [], "min_response_ms": 0,
            "public_writes": false, "challenge_type": "", "challenge_when": "",
            "deprecated": false, "sunset": ""
        }))
        .unwrap()
    }
}

#[cfg(test)]
mod pepper_tests {
    use super::{resolve_pepper, RunEnv};

    #[test]
    fn env_is_case_insensitive_and_fails_closed() {
        for raw in [
            "production",
            "Production",
            "PROD",
            "Live",
            "live",
            "staging",
            "typo",
        ] {
            assert_eq!(
                RunEnv::from_var(Some(raw), true),
                RunEnv::Production,
                "{raw}"
            );
        }
        for raw in ["dev", "Development", "LOCAL", "test"] {
            assert_eq!(RunEnv::from_var(Some(raw), false), RunEnv::Dev, "{raw}");
        }
        assert_eq!(RunEnv::from_var(Some("CI"), false), RunEnv::Ci);
        assert_eq!(RunEnv::from_var(None, true), RunEnv::Dev);
        assert_eq!(RunEnv::from_var(None, false), RunEnv::Production);
        assert_eq!(RunEnv::from_var(Some(""), false), RunEnv::Production);
    }

    #[test]
    fn non_dev_requires_a_long_pepper() {
        for env in [RunEnv::Production, RunEnv::Ci] {
            assert!(resolve_pepper(env, None).is_err());
            assert!(resolve_pepper(env, Some("short")).is_err());
        }
        let long = "x".repeat(32);
        let (p, warn) = resolve_pepper(RunEnv::Production, Some(&long)).unwrap();
        assert_eq!(p, long.as_bytes());
        assert!(warn.is_none());
    }

    #[test]
    fn dev_falls_back_with_warning() {
        let (p, warn) = resolve_pepper(RunEnv::Dev, None).unwrap();
        assert_eq!(p, b"dev-pepper");
        assert!(warn.is_some());
        let (_, warn) = resolve_pepper(RunEnv::Dev, Some("short")).unwrap();
        assert!(warn.is_some());
    }
}

#[cfg(test)]
mod auth_wiring_tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header};

    const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";
    const X: &str = "0mqNqll14aIJEhYqCiRLWJX63Ty6Q0UxBzbmJ2wI9FM";
    const Y: &str = "FSS8OFxIbiCRaiHQPz-m-qM0jRsjlGSuzoyHxiraTWM";

    struct Doc(Value);
    #[async_trait::async_trait]
    impl super::super::jwks::JwksSource for Doc {
        async fn fetch(&self) -> Result<Value, String> {
            Ok(self.0.clone())
        }
    }

    fn route(json_patch: Value) -> RouteSpec {
        let mut v = json!({
            "method": "GET", "path": "/x", "feature_id": "catalog/x", "public": false,
            "auth_removed": false, "auth_mode": "jwt", "roles": [], "scopes": [],
            "rates": [], "body_limit": 0, "request": [], "transform_in": "",
            "transform_out": "", "steps": [], "errors": [], "min_response_ms": 0,
            "public_writes": false, "challenge_type": "", "challenge_when": "",
            "deprecated": false, "sunset": ""
        });
        for (k, val) in json_patch.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    fn token(kid: &str, extra: Value) -> String {
        let now = chrono::Utc::now().timestamp();
        let mut claims = json!({
            "sub": "u1", "exp": now + 600, "iat": now, "nbf": now,
            "iss": "https://auth.localhost/shop", "aud": "shop", "tenant_id": "tenant0001"
        });
        for (k, val) in extra.as_object().unwrap() {
            claims[k] = val.clone();
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(kid.into());
        jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn bearer(t: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("authorization".to_string(), format!("Bearer {t}"))])
    }

    fn jwks_auth() -> AuthConfig {
        let mut auth = AuthConfig::anonymous();
        auth.jwks = Some(Arc::new(super::super::jwks::JwksCache::new(Arc::new(Doc(
            json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1", "x": X, "y": Y}]}),
        )))));
        auth
    }

    #[tokio::test]
    async fn jwks_keys_verify_tokens_and_unknown_kids_fail() {
        let auth = jwks_auth();
        let r = route(json!({}));
        let p = authenticate(
            &auth,
            &r,
            &bearer(&token("k1", json!({"roles": ["staff"]}))),
        )
        .await
        .ok()
        .unwrap();
        assert_eq!(p.roles, vec!["staff"]);
        assert!(authenticate(&auth, &r, &bearer(&token("nope", json!({}))))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn roles_claim_is_configurable_and_malformed_claims_fail() {
        let mut auth = jwks_auth();
        auth.roles_claim = "https://example.com/roles".into();
        let r = route(json!({}));
        let t = token(
            "k1",
            json!({"https://example.com/roles": ["admin"], "roles": ["x"]}),
        );
        let p = authenticate(&auth, &r, &bearer(&t)).await.ok().unwrap();
        assert_eq!(p.roles, vec!["admin"]);
        // Route-level override.
        let r2 = route(json!({"roles_claim": "groups"}));
        let t = token("k1", json!({"groups": ["ops"]}));
        assert_eq!(
            authenticate(&auth, &r2, &bearer(&t))
                .await
                .ok()
                .unwrap()
                .roles,
            vec!["ops"]
        );
        let bad = token("k1", json!({"groups": "ops"}));
        assert!(authenticate(&auth, &r2, &bearer(&bad)).await.is_err());
    }

    #[tokio::test]
    async fn missing_api_key_challenges_with_apikey_scheme() {
        let auth = AuthConfig::anonymous();
        let r = route(json!({"auth_mode": "api-key"}));
        let err = authenticate(&auth, &r, &BTreeMap::new())
            .await
            .err()
            .unwrap();
        assert_eq!(err.scheme, "ApiKey");
    }

    #[test]
    fn route_scopes_must_all_be_held() {
        let have = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let need = have(&["catalog:read", "orders:read"]);
        assert!(!route_scopes_allow(&have(&["catalog:read"]), &need));
        assert!(route_scopes_allow(
            &have(&["catalog:write", "orders:*"]),
            &need
        ));
        assert!(!route_scopes_allow(
            &have(&["catalog:read", "orders:readx"]),
            &need
        ));
        assert!(route_scopes_allow(&[], &[]));
        assert!(!route_scopes_allow(
            &have(&["catalog:read"]),
            &have(&["catalog:write"])
        ));
    }

    #[test]
    fn api_keys_file_is_checked() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vibe-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.json");
        let good = json!([{
            "key_id": "abcdefghijk2", "secret_hash": "a".repeat(64), "env": "live",
            "tenant_id": "tenant0001", "scopes": ["catalog:read"], "expires_unix": 4102444800i64
        }]);
        std::fs::write(&path, good.to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_api_keys(&path).err().unwrap().contains("chmod 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load_api_keys(&path).unwrap().len(), 1);
        for bad in [
            json!([{"key_id": "short", "secret_hash": "a".repeat(64), "env": "live",
                    "tenant_id": "tenant0001", "scopes": [], "expires_unix": 0}]),
            json!([{"key_id": "abcdefghijk2", "secret_hash": "plaintext", "env": "live",
                    "tenant_id": "tenant0001", "scopes": [], "expires_unix": 0}]),
            json!([{"key_id": "abcdefghijk2", "secret_hash": "a".repeat(64), "env": "live",
                    "tenant_id": "x, t_victim", "scopes": [], "expires_unix": 0}]),
            json!([{"key_id": "abcdefghijk2", "secret": "s", "secret_hash": "a".repeat(64),
                    "env": "live", "tenant_id": "tenant0001", "scopes": [], "expires_unix": 0}]),
            json!([good[0].clone(), good[0].clone()]),
        ] {
            std::fs::write(&path, bad.to_string()).unwrap();
            assert!(load_api_keys(&path).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn jwt_rejects_alg_mismatch_hs256_wrong_aud_iss_and_expired() {
        let auth = jwks_auth();
        let r = route(json!({}));
        // Wrong kid / missing key → 401.
        assert!(authenticate(&auth, &r, &bearer(&token("nope", json!({}))))
            .await
            .is_err());
        // Expired.
        let now = chrono::Utc::now().timestamp();
        let mut claims = json!({
            "sub": "u1", "exp": now - 120, "iat": now - 600, "nbf": now - 600,
            "iss": "https://auth.localhost/shop", "aud": "shop", "tenant_id": "tenant0001",
            "roles": ["staff"]
        });
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some("k1".into());
        let expired = jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(authenticate(&auth, &r, &bearer(&expired)).await.is_err());
        // Wrong aud.
        claims["exp"] = json!(now + 600);
        claims["iat"] = json!(now);
        claims["nbf"] = json!(now);
        claims["aud"] = json!("other");
        let bad_aud = jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(authenticate(&auth, &r, &bearer(&bad_aud)).await.is_err());
        // Wrong iss.
        claims["aud"] = json!("shop");
        claims["iss"] = json!("https://evil.example/");
        let bad_iss = jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(authenticate(&auth, &r, &bearer(&bad_iss)).await.is_err());
        // HS256 header with EC key in JWKS must fail (alg pin).
        let mut hs = Header::new(Algorithm::HS256);
        hs.kid = Some("k1".into());
        let hs_token =
            jsonwebtoken::encode(&hs, &claims, &EncodingKey::from_secret(b"not-the-jwks-key"))
                .unwrap();
        assert!(authenticate(&auth, &r, &bearer(&hs_token)).await.is_err());
    }

    #[tokio::test]
    async fn nested_roles_claim_and_scp_scopes_are_read() {
        let mut auth = jwks_auth();
        auth.roles_claim = "realm_access.roles".into();
        let r = route(json!({"scopes": ["catalog:read"]}));
        let t = token(
            "k1",
            json!({
                "realm_access": {"roles": ["staff"]},
                "scp": ["catalog:read", "orders:read"]
            }),
        );
        let p = authenticate(&auth, &r, &bearer(&t)).await.ok().unwrap();
        assert_eq!(p.roles, vec!["staff"]);
        assert_eq!(p.scopes, vec!["catalog:read", "orders:read"]);
        // scope string still works when scp is absent.
        let t2 = token("k1", json!({"scope": "catalog:read other:x"}));
        let p2 = authenticate(&auth, &r, &bearer(&t2)).await.ok().unwrap();
        assert_eq!(p2.scopes, vec!["catalog:read", "other:x"]);
    }

    #[tokio::test]
    async fn empty_route_scopes_are_opt_in() {
        let auth = jwks_auth();
        let r = route(json!({"scopes": []}));
        let t = token("k1", json!({"scope": "catalog:read"}));
        assert!(authenticate(&auth, &r, &bearer(&t)).await.is_ok());
        // Case sensitivity: Catalog:read does not satisfy catalog:read.
        let need = vec!["catalog:read".into()];
        assert!(!route_scopes_allow(&["Catalog:read".into()], &need));
        assert!(!route_scopes_allow(&["other:*".into()], &need));
        assert!(route_scopes_allow(&["catalog:*".into()], &need));
    }

    struct Down;
    #[async_trait::async_trait]
    impl super::super::jwks::JwksSource for Down {
        async fn fetch(&self) -> Result<Value, String> {
            Err("connection refused".into())
        }
    }

    #[tokio::test]
    async fn route_iss_aud_and_jwks_uri_override_env_defaults() {
        const IDP: &str = "https://idp.example.com/t1";
        const URI: &str = "https://idp.example.com/t1/keys";
        let keys = json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "r1", "x": X, "y": Y}]});
        // Env JWKS has the same key under kid k1; the route must not use it.
        let mut auth = jwks_auth();
        auth.route_jwks.insert(
            URI.into(),
            Arc::new(super::super::jwks::JwksCache::new(Arc::new(Doc(keys)))),
        );
        let r = route(json!({"jwt_iss": IDP, "jwt_aud": "tenant-api", "jwks_uri": URI}));
        let good = token("r1", json!({"iss": IDP, "aud": "tenant-api"}));
        assert!(authenticate(&auth, &r, &bearer(&good)).await.is_ok());
        // Default issuer/audience tokens are refused on the overridden route.
        let env_tok = token("k1", json!({}));
        assert!(authenticate(&auth, &r, &bearer(&env_tok)).await.is_err());
        let wrong_aud = token("r1", json!({"iss": IDP, "aud": "shop"}));
        assert!(authenticate(&auth, &r, &bearer(&wrong_aud)).await.is_err());
        // And the route issuer's tokens are refused on default routes.
        assert!(authenticate(&auth, &route(json!({})), &bearer(&good))
            .await
            .is_err());
        // aud-only override keeps the env key source and issuer.
        let r_aud = route(json!({"jwt_aud": "tenant-api"}));
        let t = token("k1", json!({"aud": "tenant-api"}));
        assert!(authenticate(&auth, &r_aud, &bearer(&t)).await.is_ok());
    }

    #[tokio::test]
    async fn route_jwks_unreachable_or_missing_fails_closed() {
        const IDP: &str = "https://idp.example.com/t2";
        let mut auth = jwks_auth();
        let r = route(json!({"jwt_iss": IDP}));
        let t = token("k1", json!({"iss": IDP}));
        // Not registered at startup: 401, no fallback to the env JWKS.
        assert!(authenticate(&auth, &r, &bearer(&t)).await.is_err());
        // Registered (derived {iss}/.well-known/jwks.json) but unreachable: 401.
        assert_eq!(
            route_jwks_uri(&r).unwrap(),
            "https://idp.example.com/t2/.well-known/jwks.json"
        );
        auth.route_jwks.insert(
            route_jwks_uri(&r).unwrap(),
            Arc::new(super::super::jwks::JwksCache::new(Arc::new(Down))),
        );
        assert!(authenticate(&auth, &r, &bearer(&t)).await.is_err());
    }

    #[test]
    fn add_route_jwks_requires_https() {
        let mut auth = AuthConfig::anonymous();
        let ok = route(json!({"jwt_iss": "https://idp.example.com"}));
        auth.add_route_jwks(&[ok.clone(), ok]).unwrap();
        assert_eq!(auth.route_jwks.len(), 1, "one cache per URI");
        let bad = route(json!({"jwt_iss": "https://idp.example.com", "jwks_uri": "http://x/k"}));
        assert!(auth.add_route_jwks(&[bad]).is_err());
    }

    #[test]
    fn api_keys_file_with_bad_tenant_id_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vibe-keys-tenant-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.json");
        let rec = |tenant: &str| {
            json!([{
                "key_id": "abcdefghijk2", "secret_hash": "a".repeat(64), "env": "live",
                "tenant_id": tenant, "scopes": [], "expires_unix": 4102444800i64
            }])
        };
        let write = |v: Value| {
            std::fs::write(&path, v.to_string()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        for bad in [
            "short",
            "Tenant0001",
            "tenant-0001",
            "tenant0001; drop",
            "x, t_victim",
            "",
            &"a".repeat(62),
        ] {
            write(rec(bad));
            assert!(load_api_keys(&path).is_err(), "{bad:?}");
        }
        write(rec("tenant0001"));
        let file = ApiKeyFile::open(path.clone()).unwrap();
        // A reload that introduces a bad tenant id keeps the previous keys.
        write(rec("x, t_victim"));
        file.reload();
        assert_eq!(file.keys().len(), 1);
        assert_eq!(file.keys()[0].tenant_id, "tenant0001");
    }

    #[test]
    fn api_key_file_reloads_on_mtime_and_keeps_old_on_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vibe-keys-reload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.json");
        let rec = |id: &str| {
            json!({
                "key_id": id, "secret_hash": "a".repeat(64), "env": "live",
                "tenant_id": "tenant0001", "scopes": ["catalog:read"], "expires_unix": 4102444800i64
            })
        };
        std::fs::write(&path, json!([rec("abcdefghijk2")]).to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let file = ApiKeyFile::open(path.clone()).unwrap();
        assert_eq!(file.keys().len(), 1);
        // Corrupt the file: reload keeps the previous set.
        std::fs::write(&path, "{not-json").unwrap();
        file.reload();
        assert_eq!(file.keys().len(), 1, "bad reload must keep previous keys");
        // Valid replacement (write updates mtime).
        std::fs::write(
            &path,
            json!([rec("abcdefghijk2"), rec("bcdefghijkl3")]).to_string(),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        file.reload();
        assert_eq!(file.keys().len(), 2);
    }

    #[test]
    fn detect_jwt_alg_picks_ec_and_rsa() {
        assert_eq!(
            detect_jwt_alg(
                "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE0mqNqll14aIJEhYqCiRLWJX63Ty6\n\
Q0UxBzbmJ2wI9FMVJLw4XEhuIJFqIdA/P6b6ozSNGyOUZK7OjIfGKtpNYw==\n\
-----END PUBLIC KEY-----\n"
            )
            .unwrap(),
            Algorithm::ES256
        );
    }
}

#[cfg(test)]
mod request_validation_tests {
    use super::*;
    use crate::manifest::ReqField;
    use serde_json::json;

    fn field(location: &str, name: &str, ty: &str, required: bool) -> ReqField {
        ReqField {
            location: location.into(),
            name: name.into(),
            ty: ty.into(),
            required,
            min: None,
            max: None,
            format: String::new(),
        }
    }

    fn route(request: Vec<ReqField>) -> RouteSpec {
        let mut value = json!({
            "method": "POST", "path": "/x", "feature_id": "f", "public": false,
            "auth_removed": false, "auth_mode": "jwt", "roles": [], "scopes": [],
            "rates": [], "body_limit": 0, "request": [], "transform_in": "",
            "transform_out": "", "steps": [], "errors": [], "min_response_ms": 0,
            "public_writes": false, "challenge_type": "", "challenge_when": "",
            "deprecated": false, "sunset": ""
        });
        value["request"] = serde_json::to_value(request).unwrap();
        serde_json::from_value(value).unwrap()
    }

    fn check(route: &RouteSpec, body: Value, headers: &[(&str, &str)]) -> Result<(), MethodFail> {
        let headers = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        validate_request(route, &body, &BTreeMap::new(), &BTreeMap::new(), &headers)
    }

    fn check_path(route: &RouteSpec, path: &[(&str, &str)]) -> Result<(), MethodFail> {
        let path = path
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        validate_request(
            route,
            &Value::Null,
            &BTreeMap::new(),
            &path,
            &BTreeMap::new(),
        )
    }

    fn check_query(route: &RouteSpec, query: &[(&str, &str)]) -> Result<(), MethodFail> {
        let query = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        validate_request(
            route,
            &Value::Null,
            &query,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
    }

    #[test]
    fn path_params_are_typed() {
        let r = route(vec![
            field("path", "id", "uuid", true),
            field("path", "n", "integer", false),
        ]);
        let ok = "6f1c1e7e-8a4b-4c55-9b0e-2b8f6f0f2a11";
        assert!(check_path(&r, &[("id", ok)]).is_ok());
        assert!(check_path(&r, &[("id", "1; DROP TABLE x")]).is_err());
        assert!(check_path(&r, &[("id", ok), ("n", "12")]).is_ok());
        assert!(check_path(&r, &[("id", ok), ("n", "1x")]).is_err());
    }

    #[test]
    fn query_booleans_are_exact() {
        let r = route(vec![field("query", "flag", "boolean", false)]);
        assert!(check_query(&r, &[("flag", "true")]).is_ok());
        assert!(check_query(&r, &[("flag", "false")]).is_ok());
        for bad in ["1", "yes", "TRUE", ""] {
            assert!(check_query(&r, &[("flag", bad)]).is_err(), "{bad}");
        }
    }

    #[test]
    fn uuid_must_be_canonical_hyphenated() {
        let f = field("body", "v", "uuid", true);
        for bad in [
            "6f1c1e7e8a4b4c559b0e2b8f6f0f2a11",
            "{6f1c1e7e-8a4b-4c55-9b0e-2b8f6f0f2a11}",
            "urn:uuid:6f1c1e7e-8a4b-4c55-9b0e-2b8f6f0f2a11",
        ] {
            assert!(check_type(&f, &json!(bad)).is_err(), "{bad}");
        }
        assert!(check_type(&f, &json!("6F1C1E7E-8A4B-4C55-9B0E-2B8F6F0F2A11")).is_ok());
    }

    #[test]
    fn unknown_types_fail_closed() {
        let f = field("body", "v", "date", true);
        assert_eq!(check_type(&f, &json!("x")).unwrap_err().code, "INTERNAL");
    }

    #[test]
    fn required_headers_are_enforced_case_insensitively() {
        let r = route(vec![field("header", "Idempotency-Key", "string", true)]);
        assert!(check(&r, Value::Null, &[]).is_err());
        assert!(
            check(&r, Value::Null, &[("idempotency-key", "")]).is_err(),
            "empty = missing"
        );
        assert!(check(&r, Value::Null, &[("idempotency-key", "  ")]).is_err());
        assert!(check(&r, Value::Null, &[("idempotency-key", "k")]).is_ok());
    }

    #[test]
    fn non_object_bodies_are_rejected() {
        let r = route(vec![field("body", "sku", "string", false)]);
        for body in [
            json!([{"sku": "x"}]),
            json!("x"),
            json!(1),
            json!(true),
            json!([]),
        ] {
            assert!(check(&r, body.clone(), &[]).is_err(), "{body}");
        }
        assert!(check(&r, json!({"sku": "x"}), &[]).is_ok());
        // A null body (no body) is fine when no body field is required.
        assert!(check(&r, Value::Null, &[]).is_ok());
        let required = route(vec![field("body", "sku", "string", true)]);
        assert!(check(&required, Value::Null, &[]).is_err());
    }

    #[test]
    fn every_type_is_checked() {
        let cases = [
            ("number", json!(1.5), json!("1.5")),
            ("boolean", json!(true), json!("true")),
            (
                "uuid",
                json!("6f1c1e7e-8a4b-4c55-9b0e-2b8f6f0f2a11"),
                json!("not-a-uuid"),
            ),
            ("integer", json!(3), json!(3.5)),
            ("string", json!("s"), json!(1)),
        ];
        for (ty, good, bad) in cases {
            let f = field("body", "v", ty, true);
            assert!(check_type(&f, &good).is_ok(), "{ty} good");
            assert!(check_type(&f, &bad).is_err(), "{ty} bad");
        }
    }
}

#[cfg(test)]
mod crypto_kats {
    use super::hmac_hex;

    #[test]
    fn hmac_sha256_known_answer() {
        // RFC 4231 test case 1
        let mac = hmac_hex(&[0x0b; 20], "Hi There");
        assert_eq!(
            mac,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
}

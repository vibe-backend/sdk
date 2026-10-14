use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, Request, Response, StatusCode};

type HttpResponse = Response<Body>;
use axum::Router;
use hmac::{Hmac, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::manifest::{Manifest, RouteSpec};

use super::crud::{self, MethodFail};
use super::exec::QueryExec;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct StoredKey {
    pub key_id: String,
    pub secret_hash: String,
    pub env: String,
    pub tenant_id: String,
    pub scopes: Vec<String>,
    pub expires_unix: i64,
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
    pub env_name: String,
}

impl AuthConfig {
    pub fn anonymous() -> Self {
        Self {
            public_pem: String::new(),
            alg: Algorithm::ES256,
            kid: "local".into(),
            iss: "https://auth.localhost/shop".into(),
            aud: "shop".into(),
            pepper: b"dev-pepper".to_vec(),
            api_keys: Vec::new(),
            env_name: "test".into(),
        }
    }
}

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
    pub rates: Mutex<HashMap<String, Vec<Instant>>>,
    pub quotas: Mutex<HashMap<String, u32>>,
    pub disabled: HashSet<String>,
    pub min_override_ms: Option<u64>,
    pub honor_test_ip: bool,
    /// Proxies whose `X-Forwarded-For` is trusted. Empty = use the peer address.
    pub trusted_proxies: Vec<IpAddr>,
    /// When set, an unknown public slug explains how to register it.
    pub dev_hints: bool,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new().fallback(handle).with_state(state)
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
        return error_response(
            StatusCode::BAD_REQUEST,
            "BAD_REQUEST",
            "API keys are not accepted in the query",
            &request_id,
            &[],
        );
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
            return error_response(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                "Could not read the body",
                &request_id,
                &[],
            );
        }
    };
    if bytes.len() > body_limit {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "PAYLOAD_TOO_LARGE",
            "Body is too large",
            &request_id,
            &[],
        );
    }
    let content_type = headers.get("content-type").cloned().unwrap_or_default();
    let body = if bytes.is_empty() {
        Value::Null
    } else if matches!(method.as_str(), "POST" | "PUT")
        && !content_type
            .to_ascii_lowercase()
            .starts_with("application/json")
    {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "Content-Type must be application/json",
            &request_id,
            &[],
        );
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "BAD_REQUEST",
                    "Body is not JSON",
                    &request_id,
                    &[],
                );
            }
        }
    };
    if let Err(fail) = validate_request(found.route, &body, &query, &found.params) {
        return finish(
            found.route,
            &state,
            started,
            fail_response(found.route, fail, &request_id),
        )
        .await;
    }
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
        match authenticate(&state.auth, found.route, &headers) {
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
    if found.route.auth_mode == "api-key" {
        let definition = found.route.feature_id.split('/').next().unwrap_or("");
        let write = found.route.method != "GET";
        if !scope_allows(&principal.scopes, definition, write) {
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
    if found.route.public
        && found.route.challenge_type != "none"
        && found.route.challenge_when == "always"
        && headers
            .get("x-challenge-proof")
            .map(|v| v.is_empty())
            .unwrap_or(true)
    {
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::BAD_REQUEST,
                "CHALLENGE_REQUIRED",
                "A challenge proof is required",
                &request_id,
                &[],
            ),
        )
        .await;
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
    let input = match crud::eval_expr(&found.route.transform_in, &vars) {
        Ok(value) => value,
        Err(err) => {
            return finish(
                found.route,
                &state,
                started,
                error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "VALIDATION_FAILED",
                    &err,
                    &request_id,
                    &[],
                ),
            )
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
                            return finish(
                                found.route,
                                &state,
                                started,
                                error_response(
                                    StatusCode::UNPROCESSABLE_ENTITY,
                                    "VALIDATION_FAILED",
                                    &err,
                                    &request_id,
                                    &[],
                                ),
                            )
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
                        tenant_id: &principal.tenant_id,
                        role,
                        caller: crud::Caller {
                            subject: &principal.subject,
                            admin: principal.roles.iter().any(|role| role == "admin"),
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
            "validate" | "transform" => {}
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
        route.min_response_ms
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

fn validate_request(
    route: &RouteSpec,
    body: &Value,
    query: &BTreeMap<String, String>,
    path: &BTreeMap<String, String>,
) -> Result<(), MethodFail> {
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
            "header" => true,
            _ => false,
        };
        if field.required && field.location != "header" && !present {
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
        _ => {}
    }
    Ok(())
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
        "boolean" => Value::Bool(raw == "true"),
        _ => Value::String(raw.into()),
    }
}

fn authenticate(
    auth: &AuthConfig,
    route: &RouteSpec,
    headers: &BTreeMap<String, String>,
) -> Result<Principal, AuthError> {
    if route.auth_mode == "api-key" {
        let key = headers
            .get("x-api-key")
            .cloned()
            .ok_or_else(|| AuthError::unauthorized("Bearer"))?;
        return verify_api_key(auth, &key);
    }
    let header = headers.get("authorization").cloned().unwrap_or_default();
    let token = header
        .strip_prefix("Bearer ")
        .ok_or_else(|| AuthError::unauthorized("Bearer"))?;
    verify_jwt(auth, token)
}

fn verify_jwt(auth: &AuthConfig, token: &str) -> Result<Principal, AuthError> {
    if auth.public_pem.is_empty() {
        return Err(AuthError::unauthorized("Bearer"));
    }
    let header =
        jsonwebtoken::decode_header(token).map_err(|_| AuthError::unauthorized("Bearer"))?;
    if header.kid.as_deref() != Some(auth.kid.as_str()) || header.alg != auth.alg {
        return Err(AuthError::unauthorized("Bearer"));
    }
    let mut validation = Validation::new(auth.alg);
    validation.set_issuer(&[&auth.iss]);
    validation.set_audience(&[&auth.aud]);
    validation.leeway = 60;
    validation.validate_nbf = true;
    let key = if auth.alg == Algorithm::RS256 {
        DecodingKey::from_rsa_pem(auth.public_pem.as_bytes())
            .map_err(|_| AuthError::unauthorized("Bearer"))?
    } else {
        DecodingKey::from_ec_pem(auth.public_pem.as_bytes())
            .map_err(|_| AuthError::unauthorized("Bearer"))?
    };
    let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)
        .map_err(|_| AuthError::unauthorized("Bearer"))?;
    if data.claims.exp.saturating_sub(data.claims.iat) > 3600 {
        return Err(AuthError::unauthorized("Bearer"));
    }
    Ok(Principal {
        tenant_id: data.claims.tenant_id,
        subject: data.claims.sub,
        roles: data.claims.roles,
        scopes: Vec::new(),
    })
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
    let Some(record) = auth.api_keys.iter().find(|key| key.key_id == key_id) else {
        return Err(AuthError::unauthorized("ApiKey"));
    };
    if !ct_eq(&record.secret_hash, &hash) || record.revoked || record.env != env {
        return Err(AuthError::unauthorized("ApiKey"));
    }
    if record.expires_unix < chrono::Utc::now().timestamp() {
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

pub fn hash_api_secret(pepper: &[u8], secret: &str) -> String {
    hmac_hex(pepper, secret)
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

/// Maximum number of rate/quota buckets kept in memory. When full, expired
/// buckets are pruned; if still full, new buckets are refused (fail closed).
pub const MAX_BUCKETS: usize = 100_000;
const WINDOW: Duration = Duration::from_secs(60);

fn rate_bucket(
    key: &str,
    route: &RouteSpec,
    ip: &str,
    principal: &Principal,
    body: &Value,
    pepper: &[u8],
) -> String {
    let tenant = &principal.tenant_id;
    let feature = &route.feature_id;
    let field = match key {
        "tenant" => return format!("tenant:{tenant}:{feature}"),
        "user" | "api-key" => return format!("{key}:{tenant}:{}:{feature}", principal.subject),
        "account" => Some("email"),
        _ => key.strip_prefix("input."),
    };
    match field {
        Some(field) => {
            // Normalize, then HMAC so the plaintext is never stored (§4.3).
            let raw = body
                .get(field)
                .map(|v| match v {
                    Value::String(s) => s.trim().to_lowercase(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            format!(
                "input:{tenant}:{field}:{}:{feature}",
                hmac_hex(pepper, &raw)
            )
        }
        None => format!("ip:{ip}:{feature}"),
    }
}

/// Seconds until the oldest hit leaves the window (at least 1).
fn retry_after(oldest: Instant, now: Instant) -> u64 {
    let elapsed = now.duration_since(oldest);
    let remaining = WINDOW.saturating_sub(elapsed);
    (remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)).max(1)
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
    let Ok(mut rates) = state.rates.lock() else {
        return Some("1".into());
    };
    let now = Instant::now();
    let buckets: Vec<(String, u32)> = route
        .rates
        .iter()
        .map(|rate| {
            (
                rate_bucket(&rate.key, route, ip, principal, body, &state.auth.pepper),
                rate.per_minute,
            )
        })
        .collect();
    let mut worst: Option<u64> = None;
    for (bucket, limit) in &buckets {
        if let Some(hits) = rates.get_mut(bucket) {
            hits.retain(|at| now.duration_since(*at) < WINDOW);
            if hits.len() as u32 >= *limit {
                let wait = hits.first().map(|t| retry_after(*t, now)).unwrap_or(1);
                worst = Some(worst.map_or(wait, |w| w.max(wait)));
            }
        }
    }
    if let Some(wait) = worst {
        return Some(wait.to_string());
    }
    if rates.len() + buckets.len() > MAX_BUCKETS {
        rates.retain(|_, hits| {
            hits.retain(|at| now.duration_since(*at) < WINDOW);
            !hits.is_empty()
        });
        if rates.len() + buckets.len() > MAX_BUCKETS {
            return Some(WINDOW.as_secs().to_string());
        }
    }
    for (bucket, _) in buckets {
        rates.entry(bucket).or_default().push(now);
    }
    None
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
    let tenant_key = format!("tenant:{tenant}:{day}");
    let ip_key = format!("ip:{ip}:{day}");
    let tenant_count = *quotas.get(&tenant_key).unwrap_or(&0);
    let ip_count = *quotas.get(&ip_key).unwrap_or(&0);
    if tenant_count >= 1000 || ip_count >= 50 {
        return true;
    }
    if quotas.len() + 2 > MAX_BUCKETS {
        let suffix = format!(":{day}");
        quotas.retain(|key, _| key.ends_with(&suffix));
        if quotas.len() + 2 > MAX_BUCKETS {
            return true;
        }
    }
    *quotas.entry(tenant_key).or_insert(0) += 1;
    *quotas.entry(ip_key).or_insert(0) += 1;
    false
}

/// Client IP: the peer address, unless the peer is a trusted proxy, in which
/// case the rightmost `X-Forwarded-For` hop that is not a trusted proxy.
fn client_ip(
    headers: &BTreeMap<String, String>,
    peer: Option<IpAddr>,
    trusted: &[IpAddr],
    honor_test_ip: bool,
) -> String {
    if honor_test_ip {
        if let Some(ip) = headers.get("x-vibe-test-ip") {
            return ip.clone();
        }
    }
    let Some(peer) = peer else {
        return "0.0.0.0".into();
    };
    if !trusted.contains(&peer) {
        return peer.to_string();
    }
    let Some(xff) = headers.get("x-forwarded-for") else {
        return peer.to_string();
    };
    let mut last = peer;
    for hop in xff.rsplit(',') {
        match hop.trim().parse::<IpAddr>() {
            Ok(ip) if trusted.contains(&ip) => last = ip,
            Ok(ip) => return ip.to_string(),
            Err(_) => return last.to_string(),
        }
    }
    last.to_string()
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
    #[serde(default)]
    roles: Vec<String>,
}

struct AuthError {
    scheme: String,
}

impl AuthError {
    fn unauthorized(scheme: &str) -> Self {
        Self {
            scheme: scheme.into(),
        }
    }

    fn with_request(self, request_id: &str) -> HttpResponse {
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

fn fail_response(route: &RouteSpec, fail: MethodFail, request_id: &str) -> HttpResponse {
    let mut status = platform_status(&fail.code);
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
        _ => StatusCode::INTERNAL_SERVER_ERROR,
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
    fn xff_rightmost_untrusted_hop() {
        let trusted = [ip("10.0.0.1"), ip("10.0.0.2")];
        let got = client_ip(
            &xff("6.6.6.6, 5.5.5.5, 10.0.0.2"),
            Some(ip("10.0.0.1")),
            &trusted,
            false,
        );
        assert_eq!(got, "5.5.5.5", "spoofed leftmost entry must be ignored");
    }

    #[test]
    fn retry_after_counts_down() {
        let now = Instant::now();
        let oldest = now - Duration::from_secs(45);
        assert_eq!(retry_after(oldest, now), 15);
        assert_eq!(retry_after(now, now), 60);
        assert_eq!(retry_after(now - Duration::from_secs(70), now), 1);
    }

    #[test]
    fn buckets_follow_the_key() {
        let route = RouteSpec {
            feature_id: "f".into(),
            ..test_route()
        };
        let p = Principal {
            tenant_id: "tenant0001".into(),
            subject: "u1".into(),
            roles: vec![],
            scopes: vec![],
        };
        let body = json!({"email": " A@X.COM "});
        let pep = b"p";
        assert!(rate_bucket("user", &route, "1.1.1.1", &p, &body, pep).contains(":u1:"));
        assert!(rate_bucket("tenant", &route, "1.1.1.1", &p, &body, pep).starts_with("tenant:"));
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
        assert!(!a.contains("a@x.com"), "plaintext must not be stored");
        assert_eq!(a, rate_bucket("account", &route, "3.3.3.3", &p, &body, pep));
        assert!(rate_bucket("ip", &route, "1.1.1.1", &p, &body, pep).starts_with("ip:1.1.1.1"));
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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
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
    let ip = client_ip(&headers, state.honor_test_ip);
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
    if let Some(retry) = rate_limited(&state, found.route, &ip, &principal.tenant_id) {
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
        return finish(
            found.route,
            &state,
            started,
            error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Public write quota is exhausted",
                &request_id,
                &[("retry-after", "3600")],
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
            "header" => headers.contains_key(&field.name.to_ascii_lowercase()),
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
            if value
                .as_str()
                .is_none_or(|s| uuid::Uuid::parse_str(s).is_err())
            {
                return Err(MethodFail::code(
                    "VALIDATION_FAILED",
                    format!("{} must be a uuid", field.name),
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

fn rate_limited(state: &AppState, route: &RouteSpec, ip: &str, tenant: &str) -> Option<String> {
    if route.rates.is_empty() {
        return None;
    }
    let mut rates = state.rates.lock().ok()?;
    let now = Instant::now();
    for rate in &route.rates {
        let bucket = match rate.key.as_str() {
            "tenant" => format!("tenant:{tenant}:{}", route.feature_id),
            "account" => format!("account:{tenant}:{}", route.feature_id),
            _ => format!("ip:{ip}:{}", route.feature_id),
        };
        let hits = rates.entry(bucket).or_default();
        hits.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        if hits.len() as u32 >= rate.per_minute {
            return Some("60".into());
        }
        hits.push(now);
    }
    None
}

fn quota_exhausted(state: &AppState, tenant: &str, ip: &str) -> bool {
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut quotas = match state.quotas.lock() {
        Ok(quotas) => quotas,
        Err(_) => return false,
    };
    let tenant_key = format!("tenant:{tenant}:{day}");
    let ip_key = format!("ip:{ip}:{day}");
    let tenant_count = *quotas.get(&tenant_key).unwrap_or(&0);
    let ip_count = *quotas.get(&ip_key).unwrap_or(&0);
    if tenant_count >= 1000 || ip_count >= 50 {
        return true;
    }
    *quotas.entry(tenant_key).or_insert(0) += 1;
    *quotas.entry(ip_key).or_insert(0) += 1;
    false
}

fn client_ip(headers: &BTreeMap<String, String>, honor_test_ip: bool) -> String {
    if honor_test_ip {
        if let Some(ip) = headers.get("x-vibe-test-ip") {
            return ip.clone();
        }
    }
    headers
        .get("x-forwarded-for")
        .cloned()
        .unwrap_or_else(|| "0.0.0.0".into())
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

    #[test]
    fn required_headers_are_enforced_case_insensitively() {
        let r = route(vec![field("header", "Idempotency-Key", "string", true)]);
        assert!(check(&r, Value::Null, &[]).is_err());
        assert!(check(&r, Value::Null, &[("idempotency-key", "k")]).is_ok());
    }

    #[test]
    fn non_object_bodies_are_rejected() {
        let r = route(vec![field("body", "sku", "string", false)]);
        for body in [json!([{"sku": "x"}]), json!("x"), json!(1), json!(true)] {
            assert!(check(&r, body.clone(), &[]).is_err(), "{body}");
        }
        assert!(check(&r, json!({"sku": "x"}), &[]).is_ok());
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

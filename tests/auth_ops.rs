//! Method ops behind auth flows (#26): query, verify-password, hash-password,
//! random-token, hash-token, sign-jwt. Fixture: tests/fixtures/auth-app.
//! Postgres tests run when VIBE_TEST_PG_ADMIN_URL points at a superuser.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{json, Value};
use tower::ServiceExt;
use vibe::manifest::Manifest;
use vibe::runtime::{
    router, run_method, AppState, AuthConfig, CallCtx, Caller, MemoryExec, PgExec, QueryExec, Slug,
};

const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

const PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE0mqNqll14aIJEhYqCiRLWJX63Ty6\n\
Q0UxBzbmJ2wI9FMVJLw4XEhuIJFqIdA/P6b6ozSNGyOUZK7OjIfGKtpNYw==\n\
-----END PUBLIC KEY-----\n";

fn fixture() -> Manifest {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/auth-app/manifest.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn auth() -> AuthConfig {
    let mut auth = AuthConfig::anonymous();
    auth.public_pem = PUBLIC_PEM.into();
    auth.signing_keys = vibe::runtime::load_signing_keys(
        &fixture(),
        &|name| (name == "JWT_SIGNING_KEY").then(|| PRIVATE_PEM.to_string()),
        "local",
    )
    .unwrap();
    auth
}

fn ctx<'a>(auth: &'a AuthConfig, tenant: &'a str) -> CallCtx<'a> {
    CallCtx {
        auth,
        tenant_id: tenant,
        role: "public_writer",
        caller: Caller {
            subject: "",
            roles: &[],
        },
        refresh_token: None,
    }
}

/// A one-method manifest built from fixture step JSON.
fn method_manifest(capabilities: &[&str], steps: Value) -> Manifest {
    let mut m = fixture();
    m.routes.clear();
    m.methods = vec![serde_json::from_value(json!({
        "target": "t.run", "writes": false, "input": [], "errors": [],
        "capabilities": capabilities, "crud_model": "", "crud_action": "", "steps": steps
    }))
    .unwrap()];
    m
}

fn step(op: &str, out: &str, extra: Value) -> Value {
    let mut s = json!({"id": "s", "op": op, "target": "", "args": "", "value": "", "status": 0,
        "sql": "", "params": [], "out": out, "fail": "", "when_expr": "", "writes": false,
        "table": "", "opts": {}});
    for (k, v) in extra.as_object().unwrap() {
        s[k] = v.clone();
    }
    s
}

fn respond(value: &str) -> Value {
    step("respond", "", json!({"value": value, "status": 200}))
}

async fn run(m: &Manifest, exec: &dyn QueryExec, input: Value) -> Result<Value, (String, String)> {
    let auth = auth();
    run_method(m, exec, "t.run", &input, ctx(&auth, "tenant0001"), 0)
        .await
        .map(|r| r.value)
        .map_err(|f| (f.code, f.message))
}

#[tokio::test]
async fn random_token_hash_matches_hash_token_and_email_normalizes() {
    let m = method_manifest(
        &[],
        json!([
            step("random-token", "vt", json!({"opts": {"bytes": "32"}})),
            step("hash-token", "h1", json!({"value": "vt.value"})),
            step(
                "hash-token",
                "e1",
                json!({"value": "input.a", "opts": {"normalize": "email"}})
            ),
            step(
                "hash-token",
                "e2",
                json!({"value": "input.b", "opts": {"normalize": "email"}})
            ),
            respond("{vt: vt, h1: h1, e1: e1, e2: e2}"),
        ]),
    );
    let out = run(
        &m,
        &MemoryExec::new(),
        json!({"a": " Ada@Example.COM ", "b": "ada@example.com"}),
    )
    .await
    .unwrap();
    assert_eq!(out["vt"]["value"].as_str().unwrap().len(), 43);
    assert_eq!(out["vt"]["hash"], out["h1"]);
    assert_eq!(out["e1"], out["e2"]);
    assert_eq!(out["h1"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn hash_and_verify_password_with_dummy_hash_for_missing_users() {
    let m = method_manifest(
        &[],
        json!([
            step(
                "hash-password",
                "pw",
                json!({"opts": {"plain": "input.pw"}})
            ),
            step(
                "verify-password",
                "good",
                json!({"opts": {"hash": "pw", "plain": "input.pw", "missing-user": "dummy-hash"}})
            ),
            step(
                "verify-password",
                "bad",
                json!({"opts": {"hash": "pw", "plain": "input.other", "missing-user": "dummy-hash"}})
            ),
            step(
                "verify-password",
                "missing",
                json!({"opts": {"hash": "input.none", "plain": "input.pw", "missing-user": "dummy-hash"}})
            ),
            respond("{good: good, bad: bad, missing: missing}"),
        ]),
    );
    let out = run(
        &m,
        &MemoryExec::new(),
        json!({"pw": "correct horse battery", "other": "wrong horse battery"}),
    )
    .await
    .unwrap();
    assert_eq!(out, json!({"good": true, "bad": false, "missing": false}));
    let weak = run(&m, &MemoryExec::new(), json!({"pw": "short"}))
        .await
        .unwrap_err();
    assert_eq!(weak.0, "WEAK_PASSWORD");
}

#[tokio::test]
async fn sign_jwt_requires_capability_and_sets_platform_claims() {
    let steps = |claims: &str| {
        json!([
            step(
                "sign-jwt",
                "tok",
                json!({"opts": {"alg": "ES256", "key-ref": "JWT_SIGNING_KEY",
                "claims": claims, "roles": "input.roles", "ttl": "15m"}})
            ),
            respond("tok"),
        ])
    };
    let no_cap = method_manifest(&[], steps("{ sub: input.sub }"));
    assert!(run(&no_cap, &MemoryExec::new(), json!({"sub": "u1"}))
        .await
        .is_err());
    for reserved in [
        "{ sub: input.sub, tenant_id: input.t }",
        "{ sub: input.sub, aud: input.t }",
    ] {
        let m = method_manifest(&["sign-token"], steps(reserved));
        let err = run(&m, &MemoryExec::new(), json!({"sub": "u1", "t": "x"}))
            .await
            .unwrap_err();
        assert!(err.1.contains("set by the platform"), "{err:?}");
    }
    let m = method_manifest(&["sign-token"], steps("{ sub: input.sub }"));
    let out = run(
        &m,
        &MemoryExec::new(),
        json!({"sub": "u1", "roles": ["staff"], "tenant_id": "evil0001"}),
    )
    .await
    .unwrap();
    assert_eq!(out["token_type"], "Bearer");
    assert_eq!(out["expires_in"], 900);
    let mut v = Validation::new(Algorithm::ES256);
    v.set_issuer(&["https://auth.localhost/shop"]);
    v.set_audience(&["shop"]);
    let data = jsonwebtoken::decode::<Value>(
        out["access_token"].as_str().unwrap(),
        &DecodingKey::from_ec_pem(PUBLIC_PEM.as_bytes()).unwrap(),
        &v,
    )
    .unwrap();
    assert_eq!(
        data.claims["tenant_id"], "tenant0001",
        "tenant from ctx only"
    );
    assert_eq!(data.claims["sub"], "u1");
    assert_eq!(data.claims["roles"], json!(["staff"]));
    assert_eq!(data.header.kid.as_deref(), Some("local"));
}

#[test]
fn missing_signing_key_refuses_to_start() {
    let err = vibe::runtime::load_signing_keys(&fixture(), &|_| None, "local")
        .err()
        .unwrap();
    assert!(err.contains("JWT_SIGNING_KEY"), "{err}");
}

#[tokio::test]
async fn query_steps_need_postgres() {
    let m = method_manifest(
        &[],
        json!([
            step(
                "query",
                "u",
                json!({"sql": "select 1 as one", "table": "users"})
            ),
            respond("u")
        ]),
    );
    let err = run(&m, &MemoryExec::new(), json!({})).await.unwrap_err();
    assert!(err.1.contains("--memory"), "{err:?}");
}

// ---- PostgreSQL ----

async fn pg_app() -> Option<(axum::Router, tokio_postgres::Client, String)> {
    let admin_url = std::env::var("VIBE_TEST_PG_ADMIN_URL").ok()?;
    let (admin, conn) = tokio_postgres::connect(&admin_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let s = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let tenant = format!("ta{s}");
    let app_role = format!("auth_app_{s}");
    admin
        .batch_execute(&format!("CREATE ROLE {app_role} LOGIN PASSWORD 'pw'"))
        .await
        .unwrap();
    let schema_sql = include_str!("fixtures/auth-app/schema.sql")
        .replace("{schema}", &format!("t_{tenant}"))
        .replace("{app}", &app_role);
    admin.batch_execute(&schema_sql).await.unwrap();
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&app_role).unwrap();
    url.set_password(Some("pw")).unwrap();
    let exec = PgExec::connect(url.as_str()).await.unwrap();
    let mut slugs = HashMap::new();
    slugs.insert(
        "acme".to_string(),
        Slug {
            tenant_id: tenant.clone(),
            public_access: true,
            public_write: true,
        },
    );
    let app = router(Arc::new(AppState {
        manifest: fixture(),
        exec: Arc::new(exec),
        auth: auth(),
        slugs,
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        trusted_proxies: Vec::new(),
        disabled: std::collections::HashSet::new(),
        min_override_ms: None,
        honor_test_ip: true,
        dev_hints: false,
    }));
    Some((app, admin, tenant))
}

async fn login(app: &axum::Router, email: &str, password: &str) -> (StatusCode, Value, u128) {
    let started = std::time::Instant::now();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/v1/acme/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"email": email, "password": password}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let mut body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let Some(obj) = body.as_object_mut() {
        obj.remove("request_id");
        if let Some(err) = obj.get_mut("error").and_then(Value::as_object_mut) {
            err.remove("request_id");
        }
    }
    (status, body, started.elapsed().as_millis())
}

#[tokio::test]
async fn login_requires_verified_email_and_hides_why_it_failed() {
    let Some((app, admin, tenant)) = pg_app().await else {
        eprintln!("VIBE_TEST_PG_ADMIN_URL not set; skipping Postgres test");
        return;
    };
    let hash = vibe::runtime::hash_password_for_tests("correct horse battery");
    let schema = format!("t_{tenant}");
    admin
        .execute(
            &format!(
                "INSERT INTO {schema}.users (tenant_id, email, password, roles, email_verified_at) VALUES \
                 ($1, 'ada@example.com', $2, '{{staff}}', now()), \
                 ($1, 'new@example.com', $2, '{{}}', NULL)"
            ),
            &[&tenant, &hash],
        )
        .await
        .unwrap();
    let (status, body, ms) = login(&app, "ada@example.com", "correct horse battery").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(ms >= 500, "login-class floor: {ms}ms");
    let token = body["access_token"].as_str().unwrap();
    let data = jsonwebtoken::decode::<Value>(
        token,
        &DecodingKey::from_ec_pem(PUBLIC_PEM.as_bytes()).unwrap(),
        &{
            let mut v = Validation::new(Algorithm::ES256);
            v.set_issuer(&["https://auth.localhost/shop"]);
            v.set_audience(&["shop"]);
            v
        },
    )
    .unwrap();
    assert_eq!(data.claims["tenant_id"], tenant.as_str());
    assert_eq!(data.claims["roles"], json!(["staff"]));

    let (s1, wrong, ms1) = login(&app, "ada@example.com", "wrong horse battery").await;
    let (s2, unverified, ms2) = login(&app, "new@example.com", "correct horse battery").await;
    let (s3, unknown, ms3) = login(&app, "nobody@example.com", "correct horse battery").await;
    for (s, ms) in [(s1, ms1), (s2, ms2), (s3, ms3)] {
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(ms >= 500, "{ms}ms");
    }
    assert_eq!(
        wrong, unverified,
        "same body for unverified and wrong password"
    );
    assert_eq!(wrong, unknown, "same body for unknown user");
    assert!(wrong.to_string().contains("INVALID_CREDENTIALS"), "{wrong}");
}

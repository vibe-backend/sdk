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
    router, run_method, AppState, AuthConfig, CallCtx, Caller, ExecError, MemoryExec, PgExec,
    QueryExec, Row, Slug, Stmt,
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
    }
}

/// A one-method manifest built from fixture step JSON.
fn method_manifest(capabilities: &[&str], steps: Value) -> Manifest {
    let mut m = fixture();
    m.routes.clear();
    m.methods = vec![serde_json::from_value(json!({
        "target": "t.run", "writes": steps.as_array().is_some_and(|a| a.iter().any(|s| s["writes"] == true)), "input": [], "errors": [],
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
    // bytea literal of a 32-byte HMAC, keyed by a key derived from the
    // pepper (not the pepper itself).
    let h1 = out["h1"].as_str().unwrap();
    assert!(h1.starts_with("\\x") && h1.len() == 66, "{h1}");
    let plain = {
        use hmac::{Hmac, KeyInit, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&auth().pepper).unwrap();
        mac.update(out["vt"]["value"].as_str().unwrap().as_bytes());
        hex::encode(mac.finalize().into_bytes())
    };
    assert_ne!(
        &h1[2..],
        plain,
        "token hashes are domain-separated from the pepper"
    );
}

#[tokio::test]
async fn random_token_is_always_32_bytes() {
    let m = method_manifest(
        &[],
        json!([
            step("random-token", "vt", json!({"opts": {"bytes": "16"}})),
            respond("vt")
        ]),
    );
    let err = run(&m, &MemoryExec::new(), json!({})).await.unwrap_err();
    assert!(err.1.contains("32 bytes"), "{err:?}");
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

fn sign_steps(opts: Value) -> Value {
    let mut all = json!({"alg": "ES256", "key-ref": "JWT_SIGNING_KEY", "ttl": "15m"});
    for (k, v) in opts.as_object().unwrap() {
        all[k] = v.clone();
    }
    json!([
        step(
            "query",
            "u",
            json!({"table": "users", "sql": "select id::text as id from users where id::text = $1",
                   "params": ["input.sub"]})
        ),
        step("sign-jwt", "tok", json!({ "opts": all })),
        respond("tok")
    ])
}

/// Memory store whose `query` steps return one row `{id: <first param>}`
/// (stands in for a users lookup; the memory executor does not run SQL).
struct Lookup(MemoryExec);

#[async_trait::async_trait]
impl QueryExec for Lookup {
    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: Stmt,
    ) -> Result<Vec<Row>, ExecError> {
        self.0.execute(tenant_id, role, stmt).await
    }

    async fn raw(
        &self,
        _tenant_id: &str,
        _role: &str,
        _writes: bool,
        _sql: &str,
        params: Vec<Option<String>>,
    ) -> Result<Vec<Row>, ExecError> {
        let mut row = Row::new();
        row.insert("id".into(), params.into_iter().next().flatten().into());
        Ok(vec![row])
    }

    async fn user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, ExecError> {
        self.0.user_roles(tenant_id, user_id).await
    }
}

/// A store with user u1 holding roles [staff].
fn users() -> Lookup {
    let exec = MemoryExec::new();
    exec.seed(
        "tenant0001",
        "users",
        json!({"id": "u1", "tenant_id": "tenant0001", "roles": ["staff"]})
            .as_object()
            .unwrap()
            .clone(),
    );
    Lookup(exec)
}

fn decode(token: &str) -> jsonwebtoken::TokenData<Value> {
    let mut v = Validation::new(Algorithm::ES256);
    v.set_issuer(&["https://auth.localhost/shop"]);
    v.set_audience(&["shop"]);
    jsonwebtoken::decode::<Value>(
        token,
        &DecodingKey::from_ec_pem(PUBLIC_PEM.as_bytes()).unwrap(),
        &v,
    )
    .unwrap()
}

#[tokio::test]
async fn sign_jwt_requires_capability_and_sets_platform_claims() {
    let no_cap = method_manifest(&[], sign_steps(json!({"claims": "{ sub: u.id }"})));
    assert!(run(&no_cap, &users(), json!({"sub": "u1"})).await.is_err());
    for reserved in [
        "{ sub: u.id, tenant_id: input.t }",
        "{ sub: u.id, aud: input.t }",
    ] {
        let m = method_manifest(
            &["sign-token"],
            sign_steps(json!({"claims": reserved, "custom-claims": "[tenant_id, aud]"})),
        );
        let err = run(&m, &users(), json!({"sub": "u1", "t": "x"}))
            .await
            .unwrap_err();
        assert!(err.1.contains("set by the platform"), "{err:?}");
    }
    let m = method_manifest(
        &["sign-token"],
        sign_steps(json!({"claims": "{ sub: u.id }"})),
    );
    let out = run(
        &m,
        &users(),
        json!({"sub": "u1", "roles": ["admin"], "tenant_id": "evil0001"}),
    )
    .await
    .unwrap();
    assert_eq!(out["token_type"], "Bearer");
    assert_eq!(out["expires_in"], 900);
    let expires_at = chrono::DateTime::parse_from_rfc3339(out["expires_at"].as_str().unwrap())
        .unwrap()
        .timestamp();
    assert!((expires_at - chrono::Utc::now().timestamp() - 900).abs() <= 5);
    let data = decode(out["access_token"].as_str().unwrap());
    assert_eq!(
        data.claims["tenant_id"], "tenant0001",
        "tenant from ctx only"
    );
    assert_eq!(data.claims["sub"], "u1");
    assert_eq!(
        data.claims["roles"],
        json!(["staff"]),
        "roles from users.roles"
    );
    assert_eq!(data.header.kid.as_deref(), Some("local"));
}

#[tokio::test]
async fn sign_jwt_roles_never_come_from_the_lock_or_input() {
    // A lock that tries to pass roles through is refused outright.
    let m = method_manifest(
        &["sign-token"],
        sign_steps(json!({"claims": "{ sub: u.id }", "roles": "input.roles"})),
    );
    let err = run(&m, &users(), json!({"sub": "u1", "roles": ["admin"]}))
        .await
        .unwrap_err();
    assert!(err.1.contains("users.roles"), "{err:?}");
    // A sub that is not a user gets no token.
    let m = method_manifest(
        &["sign-token"],
        sign_steps(json!({"claims": "{ sub: u.id }"})),
    );
    let err = run(&m, &users(), json!({"sub": "ghost"}))
        .await
        .unwrap_err();
    assert!(err.1.contains("not a user"), "{err:?}");
}

#[tokio::test]
async fn sign_jwt_custom_claims_are_an_allowlist_without_roles_claims() {
    let claims =
        |c: &str, declared: &str| sign_steps(json!({"claims": c, "custom-claims": declared}));
    // Undeclared claim.
    let m = method_manifest(
        &["sign-token"],
        claims("{ sub: u.id, plan: input.p }", "[]"),
    );
    let err = run(&m, &users(), json!({"sub": "u1", "p": "pro"}))
        .await
        .unwrap_err();
    assert!(err.1.contains("not declared"), "{err:?}");
    // Declared claim is signed.
    let m = method_manifest(
        &["sign-token"],
        claims("{ sub: u.id, plan: input.p }", "[plan]"),
    );
    let out = run(&m, &users(), json!({"sub": "u1", "p": "pro"}))
        .await
        .unwrap();
    assert_eq!(
        decode(out["access_token"].as_str().unwrap()).claims["plan"],
        "pro"
    );
    // The configured roles claim (VIBE_JWT_ROLES_CLAIM) can never be set,
    // even when declared.
    let mut a = auth();
    a.roles_claim = "groups".into();
    let m = method_manifest(
        &["sign-token"],
        claims("{ sub: u.id, groups: input.g }", "[groups]"),
    );
    let err = run_method(
        &m,
        &users(),
        "t.run",
        &json!({"sub": "u1", "g": ["admin"]}),
        ctx(&a, "tenant0001"),
        0,
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("roles claim"), "{err:?}");
    // Nor a per-route roles-claim (first segment of a dotted path).
    let mut m = method_manifest(
        &["sign-token"],
        claims("{ sub: u.id, realm_access: input.g }", "[realm_access]"),
    );
    let mut route = fixture().routes[0].clone();
    route.roles_claim = "realm_access.roles".into();
    m.routes.push(route);
    let err = run(
        &m,
        &users(),
        json!({"sub": "u1", "g": {"roles": ["admin"]}}),
    )
    .await
    .unwrap_err();
    assert!(err.1.contains("roles claim"), "{err:?}");
}

#[tokio::test]
async fn sign_jwt_refuses_issue_refresh_until_supported() {
    let m = method_manifest(
        &["sign-token"],
        sign_steps(json!({"claims": "{ sub: u.id }", "issue-refresh": "platform-decided"})),
    );
    let err = run(&m, &users(), json!({"sub": "u1"})).await.unwrap_err();
    assert!(err.1.contains("issue-refresh"), "{err:?}");
}

/// The runtime's own JWT verification accepts what sign-jwt issues: a
/// role-gated route lets the signed token through.
#[tokio::test]
async fn signed_token_round_trips_through_runtime_verification() {
    let m = method_manifest(
        &["sign-token"],
        sign_steps(json!({"claims": "{ sub: u.id }"})),
    );
    let exec = users();
    let out = run(&m, &exec, json!({"sub": "u1"})).await.unwrap();
    let token = out["access_token"].as_str().unwrap().to_string();
    let mut gated = fixture();
    gated.routes = vec![serde_json::from_value(json!({
        "method": "GET", "path": "/v1/me", "feature_id": "me/v1/get",
        "public": false, "auth_removed": false, "auth_mode": "jwt", "roles": ["staff"],
        "scopes": [], "rates": [], "body_limit": 4096, "request": [],
        "transform_in": "{}", "transform_out": "result",
        "steps": [step("respond", "", json!({"status": 204}))],
        "errors": [], "min_response_ms": 0, "public_writes": false, "challenge_type": "none",
        "challenge_when": "always", "deprecated": false, "sunset": ""
    }))
    .unwrap()];
    let app = router(Arc::new(AppState {
        manifest: gated,
        exec: Arc::new(exec),
        auth: auth(),
        slugs: HashMap::new(),
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        trusted_proxies: Vec::new(),
        disabled: std::collections::HashSet::new(),
        min_override_ms: None,
        honor_test_ip: true,
        dev_hints: false,
        challenge: None,
    }));
    let call = |bearer: String| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .uri("/v1/me")
                    .header("authorization", bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    assert_eq!(
        call(format!("Bearer {token}")).await,
        StatusCode::NO_CONTENT
    );
    let mut tampered = token.clone();
    tampered.pop();
    tampered.push(if token.ends_with('A') { 'B' } else { 'A' });
    assert_eq!(
        call(format!("Bearer {tampered}")).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn query_rejects_data_modifying_with_and_unmarked_writes() {
    for (sql, writes) in [
        (
            "with x as (insert into users (email) values ($1) returning id) select id from x",
            false,
        ),
        (
            "with x as (update users set email = $1 where id = $1 returning id) select * from x",
            false,
        ),
        ("insert into users (email) values ($1)", false),
    ] {
        let m = method_manifest(
            &[],
            json!([
                step(
                    "query",
                    "u",
                    json!({"sql": sql, "params": ["input.e"], "table": "users", "writes": writes})
                ),
                respond("u")
            ]),
        );
        let err = run(&m, &MemoryExec::new(), json!({"e": "x"}))
            .await
            .unwrap_err();
        assert!(
            !err.1.contains("--memory"),
            "rejected before execution: {sql} {err:?}"
        );
    }
    assert!(vibe::validate::writable_cte(
        "WITH x AS (DELETE FROM users RETURNING id) SELECT id FROM x"
    ));
    assert!(!vibe::validate::writable_cte(
        "select id from users where email = $1"
    ));
    assert!(!vibe::validate::writable_cte(
        "update users set email = $1 where id = $2"
    ));
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

/// Serialises fixture setup: the query roles are cluster-wide.
static PG_SETUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `None` locally without VIBE_TEST_PG_ADMIN_URL; in CI (`CI` set) a missing
/// database is a failure, not a skip.
async fn pg_app() -> Option<(axum::Router, tokio_postgres::Client, String, String)> {
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        if std::env::var_os("CI").is_some() {
            panic!("VIBE_TEST_PG_ADMIN_URL must be set in CI; Postgres tests cannot be skipped");
        }
        return None;
    };
    let _setup = PG_SETUP.lock().await;
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
    let app_url = url.to_string();
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
        challenge: None,
    }));
    Some((app, admin, tenant, app_url))
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
    let Some((app, admin, tenant, _)) = pg_app().await else {
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

#[tokio::test]
async fn query_runs_as_public_reader_in_a_read_only_transaction() {
    let Some((_app, _admin, tenant, url)) = pg_app().await else {
        eprintln!("VIBE_TEST_PG_ADMIN_URL not set; skipping Postgres test");
        return;
    };
    // The executor applies the same allowlist as validation.
    let exec = PgExec::connect(&url).await.unwrap();
    let err = exec
        .raw(
            &tenant,
            "public_reader",
            false,
            "select current_user::text as who",
            vec![],
        )
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("not allowed"), "{err:?}");
    let rows = exec
        .raw(
            &tenant,
            "public_reader",
            false,
            "select count(*) as n from users",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let err = exec
        .raw(
            &tenant,
            "public_reader",
            false,
            "update users set email = email where email = $1 returning id",
            vec![Some("nobody@example.com".into())],
        )
        .await
        .unwrap_err();
    let text = format!("{err:?}");
    assert!(
        text.contains("read-only") || text.contains("permission denied"),
        "{text}"
    );
}

/// Query steps with a disallowed function or a schema-qualified table are
/// rejected before they run.
#[tokio::test]
async fn query_step_rejects_disallowed_sql() {
    let Some((_app, _admin, tenant, url)) = pg_app().await else {
        return;
    };
    let exec = PgExec::connect(&url).await.unwrap();
    let auth = auth();
    for sql in [
        "select forbidden_fn(1) as x",
        "select id from other_schema.users",
    ] {
        let m = method_manifest(
            &[],
            json!([
                step("query", "u", json!({"table": "users", "sql": sql})),
                respond("u")
            ]),
        );
        let out = run_method(&m, &exec, "t.run", &json!({}), ctx(&auth, &tenant), 0).await;
        assert!(out.is_err(), "accepted: {sql}: {out:?}");
    }
}

/// The validate step from docs/best-practices/user-login.md, verbatim
/// (`when: `ok != true``, with the code-span backticks).
fn docs_login_check() -> Value {
    json!([
        step(
            "verify-password",
            "ok",
            json!({"opts": {"hash": "input.hash", "plain": "input.password", "missing-user": "dummy-hash"}})
        ),
        step(
            "validate",
            "",
            json!({"when_expr": "`ok != true`", "fail": "INVALID_CREDENTIALS"})
        ),
        respond("{ok: ok}"),
    ])
}

#[tokio::test]
async fn docs_login_when_with_backticks_rejects_wrong_and_missing_users() {
    let m = method_manifest(&[], docs_login_check());
    let hash = vibe::runtime::hash_password_for_tests("correct horse battery");
    let ok = run(
        &m,
        &MemoryExec::new(),
        json!({"hash": hash, "password": "correct horse battery"}),
    )
    .await
    .unwrap();
    assert_eq!(ok, json!({"ok": true}));
    for input in [
        json!({"hash": hash, "password": "wrong horse battery"}),
        json!({"password": "correct horse battery"}),
    ] {
        let err = run(&m, &MemoryExec::new(), input).await.unwrap_err();
        assert_eq!(err.0, "INVALID_CREDENTIALS");
    }
}

#[tokio::test]
async fn unparseable_when_fails_closed() {
    for when in ["ok is maybe", "`ok`", "", "ok != 1"] {
        assert!(vibe::runtime::when_problem(when).is_some(), "{when}");
        let m = method_manifest(
            &[],
            json!([
                step("validate", "", json!({"when_expr": when, "fail": "NOPE"})),
                respond("{ran: input.x}")
            ]),
        );
        let err = run(&m, &MemoryExec::new(), json!({"x": 1}))
            .await
            .unwrap_err();
        assert_eq!(err.0, "NOPE", "{when}: must fail closed");
    }
    for when in [
        "ok != true",
        "`ok == false`",
        "u is empty",
        "u.id is not empty",
        "x == null",
    ] {
        assert!(vibe::runtime::when_problem(when).is_none(), "{when}");
    }
}

#[tokio::test]
async fn steps_can_read_ctx_user() {
    let m = method_manifest(
        &[],
        json!([respond(
            "{sub: ctx.user.sub, roles: ctx.user.roles, tenant: ctx.user.tenant}"
        )]),
    );
    let auth = auth();
    let roles = vec!["staff".to_string()];
    let mut c = ctx(&auth, "tenant0001");
    c.caller = Caller {
        subject: "u1",
        roles: &roles,
    };
    let out = run_method(&m, &MemoryExec::new(), "t.run", &json!({}), c, 0)
        .await
        .unwrap()
        .value;
    assert_eq!(
        out,
        json!({"sub": "u1", "roles": ["staff"], "tenant": "tenant0001"})
    );
}

#[tokio::test]
async fn duplicate_insert_is_conflict_not_internal() {
    let Some((_app, _admin, tenant, url)) = pg_app().await else {
        eprintln!("VIBE_TEST_PG_ADMIN_URL not set; skipping Postgres test");
        return;
    };
    let exec = PgExec::connect(&url).await.unwrap();
    let m = method_manifest(
        &[],
        json!([
            step(
                "query",
                "(none)",
                json!({"table": "users", "writes": true,
                "sql": "insert into users (tenant_id, email) values ($1, $2)",
                "params": ["ctx.user.tenant", "input.email"]})
            ),
            respond("{done: input.email}")
        ]),
    );
    let auth = auth();
    let input = json!({"email": "dup@example.com"});
    run_method(&m, &exec, "t.run", &input, ctx(&auth, &tenant), 0)
        .await
        .unwrap();
    let err = run_method(&m, &exec, "t.run", &input, ctx(&auth, &tenant), 0)
        .await
        .unwrap_err();
    assert_eq!(err.code, "CONFLICT", "{err:?}");
}

#[tokio::test]
async fn verify_password_only_accepts_platform_hashes_within_bounds() {
    let verify = |hash: &str| {
        method_manifest(
            &[],
            json!([
                step(
                    "verify-password",
                    "ok",
                    json!({"opts": {"hash": hash, "plain": "input.pw", "missing-user": "dummy-hash"}})
                ),
                respond("{ok: ok}"),
            ]),
        )
    };
    // A valid argon2id hash of the same password, but with other parameters.
    let weak_params = {
        use argon2::password_hash::PasswordHasher;
        let argon = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(8, 1, 1, Some(32)).unwrap(),
        );
        argon
            .hash_password_with_salt(b"correct horse battery", b"0123456789abcdef")
            .unwrap()
            .to_string()
    };
    let out = run(
        &verify("input.h"),
        &MemoryExec::new(),
        json!({"pw": "correct horse battery", "h": weak_params}),
    )
    .await
    .unwrap();
    assert_eq!(out, json!({"ok": false}));
    // Over the 256-character bound never matches, even with the right hash.
    let long = "a".repeat(300);
    let m = method_manifest(
        &[],
        json!([
            step(
                "hash-password",
                "pw",
                json!({"opts": {"plain": "input.short"}})
            ),
            step(
                "verify-password",
                "ok",
                json!({"opts": {"hash": "pw", "plain": "input.pw", "missing-user": "dummy-hash"}})
            ),
            respond("{ok: ok}"),
        ]),
    );
    let out = run(
        &m,
        &MemoryExec::new(),
        json!({"short": "correct horse battery", "pw": long}),
    )
    .await
    .unwrap();
    assert_eq!(out, json!({"ok": false}));
}

#[tokio::test]
async fn sign_jwt_sub_follows_the_claims_source_rules() {
    for claims in [
        "{ sub: input.sub }",
        "{ sub: ctx.user.sub }",
        "{ sub: 'u1' }",
    ] {
        let m = method_manifest(&["sign-token"], sign_steps(json!({"claims": claims})));
        let err = run(&m, &users(), json!({"sub": "u1"})).await.unwrap_err();
        assert!(err.1.contains("query step result"), "{claims}: {err:?}");
    }
}

#[tokio::test]
async fn writing_query_needs_a_method_marked_writes() {
    let mut m = method_manifest(
        &[],
        json!([
            step(
                "query",
                "u",
                json!({"table": "users", "writes": true,
                       "sql": "update users set name = $1 where id::text = $1", "params": ["input.n"]})
            ),
            respond("u")
        ]),
    );
    m.methods[0].writes = false;
    let err = run(&m, &users(), json!({"n": "x"})).await.unwrap_err();
    assert!(err.1.contains("method is not marked writes"), "{err:?}");
}

fn fixture_with_alg(alg: Option<&str>) -> Manifest {
    let mut m = fixture();
    for method in &mut m.methods {
        for step in method.steps.iter_mut().filter(|s| s.op == "sign-jwt") {
            match alg {
                Some(alg) => {
                    step.opts.insert("alg".into(), alg.into());
                }
                None => {
                    step.opts.remove("alg");
                }
            }
        }
    }
    m
}

#[test]
fn rs256_signing_key_under_2048_bits_refuses_to_start() {
    // Decision 139 (#222).
    let short = include_str!("../src/runtime/testdata/rsa-k1024.pem");
    let ok = include_str!("../src/runtime/testdata/rsa-k2048.pem");
    let m = fixture_with_alg(Some("RS256"));
    let err = vibe::runtime::load_signing_keys(
        &m,
        &|name| (name == "JWT_SIGNING_KEY").then(|| short.to_string()),
        "local",
    )
    .err()
    .unwrap();
    assert!(err.contains("at least 2048 bits"), "{err}");
    assert!(
        !err.contains("MII"),
        "key material must not be printed: {err}"
    );
    let keys = vibe::runtime::load_signing_keys(
        &m,
        &|name| (name == "JWT_SIGNING_KEY").then(|| ok.to_string()),
        "local",
    )
    .unwrap();
    assert_eq!(keys["JWT_SIGNING_KEY"].alg, jsonwebtoken::Algorithm::RS256);
}

#[test]
fn sign_jwt_without_alg_defaults_to_es256() {
    // Decision 139 (#226).
    let keys = vibe::runtime::load_signing_keys(
        &fixture_with_alg(None),
        &|name| (name == "JWT_SIGNING_KEY").then(|| PRIVATE_PEM.to_string()),
        "local",
    )
    .unwrap();
    assert_eq!(keys["JWT_SIGNING_KEY"].alg, jsonwebtoken::Algorithm::ES256);
}

#[test]
fn es256_signing_key_must_be_p256() {
    // Decision 139: other curves refuse to start.
    for pem in [
        include_str!("../src/runtime/testdata/ec-p384-k.pem"),
        include_str!("../src/runtime/testdata/ec-p384-sec1.pem"),
    ] {
        for alg in [Some("ES256"), None] {
            let err = vibe::runtime::load_signing_keys(
                &fixture_with_alg(alg),
                &|name| (name == "JWT_SIGNING_KEY").then(|| pem.to_string()),
                "local",
            )
            .err()
            .unwrap();
            assert!(err.contains("P-256"), "{err}");
            assert!(
                !err.contains("MI"),
                "key material must not be printed: {err}"
            );
        }
    }
}

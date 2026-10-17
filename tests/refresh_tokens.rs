//! Refresh-token rotation (#26 part 2, spec §4A.1) over HTTP on the memory
//! executor. Fixture: tests/fixtures/auth-app plus the routes below.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use vibe::manifest::Manifest;
use vibe::runtime::{router, run_method, AppState, AuthConfig, CallCtx, Caller, MemoryExec, Slug};

const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

const PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE0mqNqll14aIJEhYqCiRLWJX63Ty6\n\
Q0UxBzbmJ2wI9FMVJLw4XEhuIJFqIdA/P6b6ozSNGyOUZK7OjIfGKtpNYw==\n\
-----END PUBLIC KEY-----\n";

const TENANT: &str = "tenant0001";

fn step(op: &str, out: &str, extra: Value) -> Value {
    let mut s = json!({"id": "s", "op": op, "target": "", "args": "", "value": "", "status": 0,
        "sql": "", "params": [], "out": out, "fail": "", "when_expr": "", "writes": false,
        "table": "", "opts": {}});
    for (k, v) in extra.as_object().unwrap() {
        s[k] = v.clone();
    }
    s
}

fn route(method: &str, path: &str, public: bool, extra: Value) -> Value {
    let mut r = json!({
        "method": method, "path": path, "feature_id": format!("auth/v1{path}"),
        "public": public, "auth_removed": public, "auth_mode": "jwt", "roles": [], "scopes": [],
        "rates": [], "body_limit": 4096,
        "request": [{"location": "body", "name": "refresh_token", "ty": "string",
                     "required": false, "min": null, "max": null, "format": ""}],
        "transform_in": "{}", "transform_out": "result", "steps": [], "errors": [],
        "min_response_ms": if public { 300 } else { 0 }, "public_writes": false,
        "challenge_type": "none", "challenge_when": "always", "deprecated": false, "sunset": ""
    });
    for (k, v) in extra.as_object().unwrap() {
        r[k] = v.clone();
    }
    r
}

fn manifest(grace: i64) -> Manifest {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/auth-app/manifest.json"
    );
    let mut m: Manifest = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    m.methods.push(
        serde_json::from_value(json!({
            "target": "auth.sign", "writes": false, "input": [], "errors": [],
            "capabilities": ["sign-token"], "crud_model": "", "crud_action": "",
            "steps": [
                step("query", "u", json!({"table": "users",
                    "sql": "select id::text as id from users where id::text = $1",
                    "params": ["input.user_id"]})),
                step("sign-jwt", "tok", json!({"opts": {"alg": "ES256", "key-ref": "JWT_SIGNING_KEY",
                    "claims": "{ sub: u.id }", "ttl": "15m", "issue-refresh": "platform-decided"}})),
                step("respond", "", json!({"value": "tok", "status": 200}))
            ]
        }))
        .unwrap(),
    );
    m.routes = vec![
        serde_json::from_value(route(
            "POST",
            "/auth/v1/{slug}/token/refresh",
            true,
            json!({
                "rates": [{"per_minute": 10, "key": "family"}],
                "refresh": {"sign_method": "auth.sign", "refresh_ttl_s": 2_592_000,
                            "idle_ttl_s": 604_800, "reuse_grace_s": grace}
            }),
        ))
        .unwrap(),
        serde_json::from_value(route(
            "POST",
            "/v1/logout",
            false,
            json!({"revoke": {"scope": "family"}}),
        ))
        .unwrap(),
        serde_json::from_value(route(
            "POST",
            "/v1/logout-all",
            false,
            json!({"revoke": {"scope": "all"}}),
        ))
        .unwrap(),
    ];
    m
}

fn auth(m: &Manifest) -> AuthConfig {
    let mut auth = AuthConfig::anonymous();
    auth.public_pem = PUBLIC_PEM.into();
    auth.signing_keys = vibe::runtime::load_signing_keys(
        m,
        &|name| (name == "JWT_SIGNING_KEY").then(|| PRIVATE_PEM.to_string()),
        "local",
    )
    .unwrap();
    auth
}

struct App {
    app: axum::Router,
    manifest: Manifest,
    exec: Arc<MemoryExec>,
    state: Arc<AppState>,
}

fn app(grace: i64) -> App {
    app_with(manifest(grace))
}

fn app_with(m: Manifest) -> App {
    app_wrapped(m, |exec| exec)
}

fn app_wrapped(
    m: Manifest,
    wrap: impl FnOnce(Arc<MemoryExec>) -> Arc<dyn vibe::runtime::QueryExec>,
) -> App {
    let exec = Arc::new(MemoryExec::new());
    exec.stub_queries("id");
    exec.seed(
        TENANT,
        "users",
        json!({"id": "u1", "tenant_id": TENANT, "roles": ["staff"]})
            .as_object()
            .unwrap()
            .clone(),
    );
    let mut slugs = HashMap::new();
    slugs.insert(
        "acme".to_string(),
        Slug {
            tenant_id: TENANT.into(),
            public_access: true,
            public_write: true,
        },
    );
    let state = Arc::new(AppState {
        manifest: m.clone(),
        exec: wrap(exec.clone()),
        auth: auth(&m),
        dev_slugs: None,
        slugs,
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        trusted_proxies: Vec::new(),
        disabled: std::collections::HashSet::new(),
        min_override_ms: None,
        honor_test_ip: true,
        dev_hints: false,
        challenge: None,
    });
    let app = router(state.clone());
    App {
        state,
        app,
        manifest: m,
        exec,
    }
}

impl App {
    /// What login does: sign with `issue-refresh` and no rotation → new family.
    async fn login(&self) -> Value {
        let auth = auth(&self.manifest);
        run_method(
            &self.manifest,
            self.exec.as_ref(),
            "auth.sign",
            &json!({"user_id": "u1"}),
            CallCtx {
                auth: &auth,
                tenant_id: TENANT,
                role: "public_reader",
                caller: Caller {
                    subject: "",
                    roles: &[],
                },
                refresh_token: None,
            },
            0,
        )
        .await
        .unwrap()
        .value
    }

    async fn post(
        &self,
        path: &str,
        bearer: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value, u128) {
        let started = std::time::Instant::now();
        let mut req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = bearer {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let res = self
            .app
            .clone()
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(res.headers()["cache-control"], "no-store");
        assert_eq!(res.headers()["pragma"], "no-cache");
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let mut v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if let Some(e) = v.get_mut("error").and_then(Value::as_object_mut) {
            e.remove("request_id");
        }
        if let Some(o) = v.as_object_mut() {
            o.remove("request_id");
        }
        (status, v, started.elapsed().as_millis())
    }

    async fn raw_refresh(
        &self,
        token: &str,
        ip: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>, u128) {
        let started = std::time::Instant::now();
        let res = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/v1/acme/token/refresh")
                    .header("content-type", "application/json")
                    .header("x-vibe-test-ip", ip)
                    .body(Body::from(json!({"refresh_token": token}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body, started.elapsed().as_millis())
    }

    async fn refresh(&self, token: &str) -> (StatusCode, Value, u128) {
        self.post(
            "/auth/v1/acme/token/refresh",
            None,
            json!({"refresh_token": token}),
        )
        .await
    }
}

fn tok(v: &Value, k: &str) -> String {
    v[k].as_str()
        .unwrap_or_else(|| panic!("no {k} in {v}"))
        .to_string()
}

#[tokio::test]
async fn login_issues_a_family_and_refresh_rotates_it() {
    let a = app(0);
    let login = a.login().await;
    let r0 = tok(&login, "refresh_token");
    assert_eq!(r0.len(), 43, "32 random bytes, base64url");
    let (s, body, ms) = a.refresh(&r0).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(ms >= 300, "refresh floor: {ms}ms");
    assert_eq!(body["token_type"], "Bearer");
    assert!(body["expires_at"].is_string());
    let r1 = tok(&body, "refresh_token");
    assert_ne!(r1, r0);
    // The new access token works on an authenticated route.
    let (s, _, _) = a
        .post(
            "/v1/logout",
            Some(&tok(&body, "access_token")),
            json!({"refresh_token": r1}),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // Logged out: the family is gone.
    let (s, _, _) = a.refresh(&r1).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn replaying_a_spent_token_revokes_the_family() {
    let a = app(0);
    let r0 = tok(&a.login().await, "refresh_token");
    let (_, body, _) = a.refresh(&r0).await;
    let r1 = tok(&body, "refresh_token");
    let (s, replay, _) = a.refresh(&r0).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, after, _) = a.refresh(&r1).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "whole family revoked");
    let (_, unknown, _) = a.refresh("not-a-token").await;
    assert_eq!(replay, unknown, "same 401 for reuse and unknown");
    assert_eq!(after, unknown);
    assert!(
        unknown.to_string().contains("INVALID_REFRESH_TOKEN"),
        "{unknown}"
    );
    let (s, missing, _) = a.post("/auth/v1/acme/token/refresh", None, json!({})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(missing, unknown);
}

#[tokio::test]
async fn retry_after_ten_second_grace_revokes_family_over_http() {
    let a = app(10);
    let parent = tok(&a.login().await, "refresh_token");
    let (status, first, _) = a.refresh(&parent).await;
    assert_eq!(status, StatusCode::OK);
    let child = tok(&first, "refresh_token");

    // Start the wait after completion and exceed the whole-second grace boundary.
    tokio::time::sleep(std::time::Duration::from_secs(11)).await;
    let (status, replay, _) = a.refresh(&parent).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(replay["error"]["code"], "INVALID_REFRESH_TOKEN");
    let (status, _, _) = a.refresh(&child).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "whole family revoked");
}

#[tokio::test]
async fn retry_within_grace_reissues_once() {
    let a = app(10);
    let r0 = tok(&a.login().await, "refresh_token");
    let (_, first, _) = a.refresh(&r0).await;
    let lost = tok(&first, "refresh_token");
    let (s, retry, _) = a.refresh(&r0).await;
    assert_eq!(s, StatusCode::OK, "grace reissue");
    let r1 = tok(&retry, "refresh_token");
    let (s, _, _) = a.refresh(&r0).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "only once");
    let (s, _, _) = a.refresh(&r1).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "second retry was reuse: family revoked"
    );
    let (s, _, _) = a.refresh(&lost).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn family_rate_limit_is_capped() {
    let a = app(0);
    let mut cur = tok(&a.login().await, "refresh_token");
    let mut limited = None;
    for i in 0..11 {
        let (s, body, _) = a.refresh(&cur).await;
        if s != StatusCode::OK {
            // Family limit response.
            let (_, unknown, _) = a.refresh("not-a-token").await;
            assert_eq!(s, StatusCode::UNAUTHORIZED);
            assert_eq!(body, unknown);
            limited = Some(i);
            break;
        }
        cur = tok(&body, "refresh_token");
    }
    assert_eq!(limited, Some(10), "10 per family per minute");
}

#[tokio::test]
async fn refresh_limit_responses() {
    let a = app(10);
    let mut cur = tok(&a.login().await, "refresh_token");
    for _ in 0..10 {
        let (status, body, _) = a.refresh(&cur).await;
        assert_eq!(status, StatusCode::OK);
        cur = tok(&body, "refresh_token");
    }
    let limited = a.raw_refresh(&cur, "192.0.2.1").await;
    let invalid = a.raw_refresh("", "192.0.2.1").await;
    let unknown = a.raw_refresh("nonexistent", "192.0.2.1").await;
    for response in [&limited, &invalid, &unknown] {
        assert_eq!(response.0, StatusCode::UNAUTHORIZED);
        assert!(response.1.contains_key("x-request-id"));
        assert_eq!(response.1["cache-control"], "no-store");
        assert_eq!(response.1["pragma"], "no-cache");
        let mut headers = response.1.clone();
        let mut expected = limited.1.clone();
        headers.remove("x-request-id");
        expected.remove("x-request-id");
        assert_eq!(headers, expected);
        assert_eq!(response.2, limited.2);
        let cookies: Vec<_> = response.1.get_all("set-cookie").iter().collect();
        assert!(!cookies.is_empty());
        assert_eq!(
            cookies,
            limited.1.get_all("set-cookie").iter().collect::<Vec<_>>()
        );
        assert!(!response.1.contains_key("retry-after"));
        assert!(response.3 >= 300);
    }
    *a.state.rates.lock().unwrap() = Default::default();
    assert_eq!(a.refresh(&cur).await.0, StatusCode::OK);
    for _ in 0..30 {
        a.raw_refresh("", "192.0.2.2").await;
    }
    let ip_limited = a.raw_refresh("", "192.0.2.2").await;
    assert_eq!(ip_limited.0, StatusCode::TOO_MANY_REQUESTS);
    assert!(ip_limited.1.contains_key("retry-after"));
    assert_eq!(ip_limited.1["cache-control"], "no-store");
    assert_eq!(ip_limited.1["pragma"], "no-cache");
    assert!(String::from_utf8(ip_limited.2)
        .unwrap()
        .contains("RATE_LIMITED"));
}

#[tokio::test]
async fn revoked_child_reuse_revokes_family_over_http() {
    let a = app(10);
    let parent = tok(&a.login().await, "refresh_token");
    let (status, first, _) = a.refresh(&parent).await;
    assert_eq!(status, StatusCode::OK);
    let (status, reissued, _) = a.refresh(&parent).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        a.refresh(&tok(&first, "refresh_token")).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        a.refresh(&tok(&reissued, "refresh_token")).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn refresh_rejections_are_available_to_internal_metrics() {
    let a = app(10);
    let before = vibe::runtime::refresh_rejections()
        .get("invalid")
        .copied()
        .unwrap_or_default();
    assert_eq!(
        a.raw_refresh("", "192.0.2.10").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(vibe::runtime::refresh_rejections()["invalid"] > before);
    let exported = vibe::runtime::internal_metrics();
    assert!(exported.contains("vibe_refresh_rejections_total{reason=\"invalid\"}"));
    assert!(exported.contains("vibe_rate_limit_saturation_refusals_total"));
}

#[tokio::test]
async fn revoked_tokens_leave_other_families_available() {
    let a = app(0);
    let r0 = tok(&a.login().await, "refresh_token");
    let (_, body, _) = a.refresh(&r0).await;
    let r1 = tok(&body, "refresh_token");
    let (s, _, _) = a
        .post(
            "/v1/logout",
            Some(&tok(&body, "access_token")),
            json!({"refresh_token": r1}),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    for _ in 0..12 {
        let (s, _, _) = a.refresh(&r1).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let fresh = tok(&a.login().await, "refresh_token");
    let (s, body, _) = a.refresh(&fresh).await;
    assert_eq!(s, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn grace_retry_is_refused_after_the_child_was_used() {
    let a = app(10);
    let r0 = tok(&a.login().await, "refresh_token");
    let (_, b1, _) = a.refresh(&r0).await;
    let (s, b2, _) = a.refresh(&tok(&b1, "refresh_token")).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = a.refresh(&r0).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "child already used: reuse");
    let (s, _, _) = a.refresh(&tok(&b2, "refresh_token")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "family revoked");
}

#[tokio::test]
async fn bad_refresh_body_is_401_not_422() {
    // A strict request table must not turn a bad or missing token into 422.
    let mut m = manifest(0);
    m.routes[0].request = serde_json::from_value(json!([{"location": "body",
        "name": "refresh_token", "ty": "string", "required": true, "min": 43, "max": 43,
        "format": ""}]))
    .unwrap();
    let a = app_with(m);
    let (_, unknown, _) = a.refresh("not-a-token").await;
    for body in [
        json!({}),
        json!({"refresh_token": 5}),
        json!({"refresh_token": "short"}),
    ] {
        let (s, v, _) = a.post("/auth/v1/acme/token/refresh", None, body).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
        assert_eq!(v, unknown);
    }
}

#[tokio::test]
async fn refresh_response_states_the_refresh_token_expiry() {
    let a = app(0);
    let login = a.login().await;
    assert_eq!(login["refresh_expires_in"], 604_800);
    let (_, body, _) = a.refresh(&tok(&login, "refresh_token")).await;
    assert_eq!(body["refresh_expires_in"], 604_800);
}

#[tokio::test]
async fn logout_all_revokes_every_family_of_the_caller_only() {
    let a = app(0);
    let s1 = a.login().await;
    let s2 = a.login().await;
    let (s, _, _) = a
        .post("/v1/logout-all", Some(&tok(&s1, "access_token")), json!({}))
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    for session in [&s1, &s2] {
        let (s, _, _) = a.refresh(&tok(session, "refresh_token")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let (s, _, _) = a
        .post("/v1/logout", None, json!({"refresh_token": "x"}))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "revoke needs auth");
}

#[tokio::test]
async fn sign_jwt_issue_refresh_needs_a_refresh_route() {
    let a = app(0);
    let mut m = a.manifest.clone();
    m.routes.retain(|r| r.refresh.is_none());
    let auth = auth(&m);
    let err = run_method(
        &m,
        a.exec.as_ref(),
        "auth.sign",
        &json!({"user_id": "u1"}),
        CallCtx {
            auth: &auth,
            tenant_id: TENANT,
            role: "public_reader",
            caller: Caller {
                subject: "",
                roles: &[],
            },
            refresh_token: None,
        },
        0,
    )
    .await
    .unwrap_err();
    assert!(err.message.contains("token-refresh route"), "{err:?}");
}

#[test]
fn pipeline_blocks_parse_and_check() {
    use vibe::lock::Bullet;
    use vibe::manifest::{check_refresh_route, parse_refresh_block, parse_revoke_block};
    let b = |pairs: &[(&str, &str)]| -> Vec<Bullet> {
        pairs
            .iter()
            .map(|(k, v)| Bullet {
                key: (*k).into(),
                value: (*v).into(),
            })
            .collect()
    };
    let spec = parse_refresh_block(&b(&[
        ("sign-method", "user.sign-token"),
        ("idle-ttl", "1d"),
    ]))
    .unwrap();
    assert_eq!(
        (spec.refresh_ttl_s, spec.idle_ttl_s, spec.reuse_grace_s),
        (2_592_000, 86_400, 10)
    );
    assert!(
        parse_refresh_block(&b(&[])).is_err(),
        "sign-method required"
    );
    assert!(parse_refresh_block(&b(&[("sign-method", "m"), ("refresh-ttl", "31d")])).is_err());
    assert!(parse_refresh_block(&b(&[("sign-method", "m"), ("idle-ttl", "8d")])).is_err());
    // reuse-grace: default and maximum 10s (spec §4A.1); 0 is not a valid
    // window (the setting is 1s..=10s).
    let grace = |v: &str| {
        parse_refresh_block(&b(&[("sign-method", "m"), ("reuse-grace", v)]))
            .map(|s| s.reuse_grace_s)
    };
    assert_eq!(grace("10s").unwrap(), 10);
    assert_eq!(grace("10000ms").unwrap(), 10);
    assert_eq!(grace("1s").unwrap(), 1);
    for bad in [
        "10001ms",
        "10.001s",
        "10.000000000000000000001s",
        "11s",
        "60s",
        "1m",
        "0s",
        "0",
    ] {
        let err = grace(bad).unwrap_err();
        assert!(err.contains("between 1s and 10s"), "{bad}: {err}");
    }
    assert!(parse_refresh_block(&b(&[("sign-method", "m"), ("transport", "cookie")])).is_err());
    assert!(parse_refresh_block(&b(&[("sign-method", "m"), ("bogus", "1")])).is_err());
    assert!(parse_revoke_block(&b(&[("scope", "everyone")])).is_err());
    let revoke = parse_revoke_block(&b(&[])).unwrap();
    assert_eq!(revoke.scope, "family");
    assert!(check_refresh_route(Some(&spec), None, true, true).is_ok());
    assert!(
        check_refresh_route(Some(&spec), None, false, false).is_err(),
        "must be public, no auth"
    );
    assert!(
        check_refresh_route(None, Some(&revoke), true, true).is_err(),
        "revoke needs auth"
    );
    assert!(check_refresh_route(None, Some(&revoke), false, false).is_ok());
    assert!(
        vibe::manifest::check_rate_key("family", false, "jwt").is_err(),
        "family only on refresh routes"
    );
}

async fn check_postgres_reservation(mode: &str) {
    use vibe::runtime::refresh::RefreshPolicy;
    use vibe::runtime::{PgExec, QueryExec};
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        return;
    };
    let (admin, conn) = tokio_postgres::connect(&admin_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let s = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let tenant = format!("tr{s}");
    let schema = format!("t_{tenant}");
    let role = format!("refresh_app_{s}");
    admin
        .batch_execute(&format!("CREATE ROLE {role} LOGIN PASSWORD 'pw'"))
        .await
        .unwrap();
    for stmt in vibe::runtime::refresh::table_statements(&schema).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    for stmt in vibe::runtime::refresh::runtime_grant_statements(&schema, &role).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&role).unwrap();
    url.set_password(Some("pw")).unwrap();
    let exec = PgExec::connect_with(
        url.as_str(),
        vibe::runtime::PgOptions {
            pool_size: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    check_issuance_without_store_connection(&exec, &tenant, mode).await;
    if mode == "finalize-failure" {
        exec.refresh_issue(
            &tenant,
            "finalize-user",
            "finalize-parent",
            RefreshPolicy::default(),
        )
        .await
        .unwrap();
        admin.batch_execute(&format!("CREATE FUNCTION {schema}.fail_finalize() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.token_hash = 'finalize-child' THEN RAISE EXCEPTION 'finalize unavailable'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_finalize BEFORE INSERT ON {schema}._platform_refresh_tokens FOR EACH ROW EXECUTE FUNCTION {schema}.fail_finalize()" )).await.unwrap();
        assert!(exec
            .refresh_rotate(
                &tenant,
                "finalize-parent",
                "finalize-child",
                RefreshPolicy::default()
            )
            .await
            .is_err());
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                match exec
                    .refresh_rotate(
                        &tenant,
                        "finalize-parent",
                        "finalize-retry",
                        RefreshPolicy::default(),
                    )
                    .await
                {
                    Err(vibe::runtime::ExecError::Conflict) => tokio::task::yield_now().await,
                    Ok(vibe::runtime::refresh::Rotated::Ok { .. }) => break,
                    other => panic!("finalize failure rotated: {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            exec.refresh_family(&tenant, "finalize-child")
                .await
                .unwrap(),
            None
        );
    }
    if mode == "expiry-during-issuance" {
        struct ExpiringIssuer<'a> {
            exec: &'a PgExec,
            admin: &'a tokio_postgres::Client,
            tenant: &'a str,
            schema: &'a str,
        }
        #[async_trait::async_trait]
        impl vibe::runtime::RefreshIssuer for ExpiringIssuer<'_> {
            async fn issue(&self, _: &str) -> Result<Value, vibe::runtime::ExecError> {
                self.admin.batch_execute(&format!("UPDATE {}._platform_refresh_tokens SET pending_until = clock_timestamp() - interval '1 second' WHERE token_hash = 'expiry-parent'", self.schema)).await.unwrap();
                let policy = RefreshPolicy::default();
                self.exec
                    .refresh_issue(self.tenant, "other", "expiry-other", policy)
                    .await?;
                assert!(matches!(
                    self.exec
                        .refresh_rotate(self.tenant, "expiry-other", "expiry-other-child", policy)
                        .await?,
                    vibe::runtime::refresh::Rotated::Ok { .. }
                ));
                Ok(json!({}))
            }
        }
        exec.refresh_issue(
            &tenant,
            "expiry-user",
            "expiry-parent",
            RefreshPolicy::default(),
        )
        .await
        .unwrap();
        let expired = exec
            .refresh_rotate_authorized(
                &tenant,
                "expiry-parent",
                "expiry-child",
                RefreshPolicy::default(),
                Some(&ExpiringIssuer {
                    exec: &exec,
                    admin: &admin,
                    tenant: &tenant,
                    schema: &schema,
                }),
            )
            .await;
        assert!(matches!(expired, Err(vibe::runtime::ExecError::Timeout)));
        assert!(matches!(
            exec.refresh_rotate(
                &tenant,
                "expiry-parent",
                "expiry-retry",
                RefreshPolicy::default()
            )
            .await
            .unwrap(),
            vibe::runtime::refresh::Rotated::Ok { .. }
        ));
        assert_eq!(
            exec.refresh_family(&tenant, "expiry-child").await.unwrap(),
            None
        );
    }
    if mode == "expired" {
        let lease_policy = RefreshPolicy::default();
        exec.refresh_issue(&tenant, "lease-user", "lease-parent", lease_policy)
            .await
            .unwrap();
        admin.batch_execute(&format!("UPDATE {schema}._platform_refresh_tokens SET pending_rotation = 'expired', pending_until = clock_timestamp() - interval '1 second' WHERE token_hash = 'lease-parent'")).await.unwrap();
        assert!(matches!(
            exec.refresh_rotate(&tenant, "lease-parent", "lease-child", lease_policy)
                .await
                .unwrap(),
            vibe::runtime::refresh::Rotated::Ok { .. }
        ));
    }
    let policy = RefreshPolicy {
        family_ttl: 3600,
        idle_ttl: 600,
        grace: 10,
    };
    if ["slow", "http-pool-timeout", "http-reservation-expiry"].contains(&mode) {
        // A child insert waits inside the rotation transaction beyond the response floor.
        admin
            .batch_execute(&format!(
                "CREATE FUNCTION {schema}.delay_rotation() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF NEW.parent_id IS NOT NULL THEN PERFORM pg_sleep(0.4); END IF; RETURN NEW; END $$;
         CREATE TRIGGER delay_rotation BEFORE INSERT ON {schema}._platform_refresh_tokens
         FOR EACH ROW EXECUTE FUNCTION {schema}.delay_rotation();"
            ))
            .await
            .unwrap();
        let m = manifest(10);
        let _ = admin
            .batch_execute("CREATE ROLE public_reader NOLOGIN")
            .await;
        admin
            .batch_execute(&format!(
                "CREATE TABLE {schema}.users (id text PRIMARY KEY, roles text[]);
         INSERT INTO {schema}.users VALUES ('u1', ARRAY['staff']);
         GRANT USAGE ON SCHEMA {schema} TO public_reader;
         GRANT SELECT ON {schema}.users TO public_reader;"
            ))
            .await
            .unwrap();
        let base = app_with(m.clone());
        let slugs = HashMap::from([(
            "acme".into(),
            Slug {
                tenant_id: tenant.clone(),
                public_access: true,
                public_write: true,
            },
        )]);
        let http_exec = Arc::new(
            PgExec::connect_with(
                &admin_url,
                vibe::runtime::PgOptions {
                    pool_size: 1,
                    pool_timeout_ms: 100,
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let state = Arc::new(AppState {
            manifest: m,
            exec: http_exec.clone(),
            auth: base.state.auth.clone(),
            dev_slugs: None,
            slugs,
            rates: std::sync::Mutex::new(Default::default()),
            quotas: std::sync::Mutex::new(Default::default()),
            trusted_proxies: Vec::new(),
            disabled: std::collections::HashSet::new(),
            min_override_ms: None,
            honor_test_ip: true,
            dev_hints: false,
            challenge: None,
        });
        use hmac::{Hmac, KeyInit, Mac};
        let token = uuid::Uuid::new_v4().to_string();
        let mut derive = Hmac::<sha2::Sha256>::new_from_slice(&state.auth.pepper).unwrap();
        derive.update(b"vibe/refresh-token/v1");
        let mut mac =
            Hmac::<sha2::Sha256>::new_from_slice(&derive.finalize().into_bytes()).unwrap();
        mac.update(token.as_bytes());
        let hash = hex::encode(mac.finalize().into_bytes());
        let family = exec
            .refresh_issue(&tenant, "u1", &hash, policy)
            .await
            .unwrap();
        let a = App {
            app: router(state.clone()),
            manifest: state.manifest.clone(),
            exec: base.exec,
            state,
        };
        if mode == "http-pool-timeout" {
            let held = http_exec.pool().get().await.unwrap();
            let (status, body, _) = a.refresh(&token).await;
            assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
            assert_eq!(body["error"]["code"], "TX_TIMEOUT");
            drop(held);
        }
        if mode == "http-reservation-expiry" {
            admin.batch_execute(&format!("CREATE FUNCTION {schema}.issuance_delay() RETURNS text LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.3); RETURN 'u1'; END $$; ALTER TABLE {schema}.users RENAME TO users_data; CREATE VIEW {schema}.users AS SELECT {schema}.issuance_delay() AS id, roles FROM {schema}.users_data; GRANT SELECT ON {schema}.users TO public_reader")).await.unwrap();
            let expire = async {
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    loop {
                        let count = admin.execute(&format!("UPDATE {schema}._platform_refresh_tokens SET pending_until = clock_timestamp() - interval '1 second' WHERE token_hash = $1 AND pending_rotation IS NOT NULL"), &[&hash]).await.unwrap();
                        if count == 1 { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.unwrap();
            };
            tokio::join!(assert_http_refresh_timeout(&a, &token), expire);
        }
        let (status, body, elapsed) = a.refresh(&token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(elapsed >= 400);
        assert!(body.get("access_token").is_some());
        let (status, body, _) = a.refresh(&tok(&body, "refresh_token")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let revoked: i64 = admin.query_one(&format!(
        "SELECT count(*) FROM {schema}._platform_refresh_tokens WHERE family_id::text = $1 AND revoked_at IS NOT NULL"
    ), &[&family]).await.unwrap().get(0);
        assert_eq!(revoked, 0);
        admin
            .batch_execute(&format!(
                "DROP TRIGGER delay_rotation ON {schema}._platform_refresh_tokens"
            ))
            .await
            .unwrap();
    }
    if mode == "pool" {
        admin.batch_execute(&format!("CREATE TABLE {schema}.users (id text PRIMARY KEY, roles text[]); INSERT INTO {schema}.users VALUES ('u1', ARRAY['staff'])")).await.unwrap();
        struct PoolIssuer {
            exec: Arc<PgExec>,
            tenant: String,
            entered: Arc<tokio::sync::Barrier>,
            resume: Arc<tokio::sync::Semaphore>,
        }
        #[async_trait::async_trait]
        impl vibe::runtime::RefreshIssuer for PoolIssuer {
            async fn issue(&self, _: &str) -> Result<Value, vibe::runtime::ExecError> {
                self.entered.wait().await;
                let _permit = self.resume.acquire().await.unwrap();
                self.exec.user_roles(&self.tenant, "u1").await?;
                Ok(json!({}))
            }
        }
        let small = Arc::new(
            PgExec::connect_with(
                &admin_url,
                vibe::runtime::PgOptions {
                    pool_size: 2,
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let entered = Arc::new(tokio::sync::Barrier::new(21));
        let resume = Arc::new(tokio::sync::Semaphore::new(0));
        let mut refreshes = tokio::task::JoinSet::new();
        for n in 0..20 {
            let hash = format!("pool-parent-{n}");
            small
                .refresh_issue(&tenant, "u1", &hash, policy)
                .await
                .unwrap();
            let exec = small.clone();
            let tenant = tenant.clone();
            let entered = entered.clone();
            let resume = resume.clone();
            refreshes.spawn(async move {
                let issuer = PoolIssuer {
                    exec: exec.clone(),
                    tenant: tenant.clone(),
                    entered,
                    resume,
                };
                exec.refresh_rotate_authorized(
                    &tenant,
                    &hash,
                    &format!("pool-child-{n}"),
                    policy,
                    Some(&issuer),
                )
                .await
            });
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.wait())
            .await
            .expect("all 20 refresh reservations must commit with pool_size 2");
        for _ in 0..5 {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                small.user_roles(&tenant, "u1"),
            )
            .await
            .expect("non-refresh requests must complete with a small pool")
            .unwrap();
        }
        resume.add_permits(20);
        while let Some(result) = refreshes.join_next().await {
            assert!(matches!(
                result.unwrap().unwrap().0,
                vibe::runtime::refresh::Rotated::Ok { .. }
            ));
        }
    }
    let failed_parent = match mode {
        "cancel" => Some("cancel-parent"),
        "finalize-failure" => Some("finalize-parent"),
        "expired" => Some("lease-parent"),
        "expiry-during-issuance" => Some("expiry-parent"),
        "past-grace" => Some("past-grace-parent"),
        _ => None,
    };
    if let Some(parent) = failed_parent {
        let row = admin.query_one(&format!("SELECT revoked_at IS NOT NULL, replaced_by IS NULL FROM {schema}._platform_refresh_tokens WHERE token_hash = $1"), &[&parent]).await.unwrap();
        let revoked = mode == "past-grace";
        assert_eq!(row.get::<_, bool>(0), revoked);
        let live: i64 = admin.query_one(&format!("SELECT count(*) FROM {schema}._platform_refresh_tokens WHERE family_id = (SELECT family_id FROM {schema}._platform_refresh_tokens WHERE token_hash = $1) AND revoked_at IS NULL"), &[&parent]).await.unwrap().get(0);
        assert_eq!(live, if revoked { 0 } else { 2 });
    }
    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE; DROP ROLE {role}"))
        .await
        .unwrap();
}

/// The PostgreSQL store (row lock, grace, family revoke) against a real
/// database. Skips when VIBE_TEST_PG_ADMIN_URL is missing.
#[tokio::test]
async fn postgres_store_rotates_detects_reuse_and_revokes() {
    use vibe::runtime::refresh::{RefreshPolicy, RevokeTarget, Rotated};
    use vibe::runtime::{PgExec, QueryExec};
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        return;
    };
    let (admin, conn) = tokio_postgres::connect(&admin_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let s = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let tenant = format!("tr{s}");
    let schema = format!("t_{tenant}");
    let role = format!("refresh_app_{s}");
    admin
        .batch_execute(&format!("CREATE ROLE {role} LOGIN PASSWORD 'pw'"))
        .await
        .unwrap();
    for stmt in vibe::runtime::refresh::table_statements(&schema).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    for stmt in vibe::runtime::refresh::runtime_grant_statements(&schema, &role).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&role).unwrap();
    url.set_password(Some("pw")).unwrap();
    let exec = PgExec::connect(url.as_str()).await.unwrap();

    let policy = RefreshPolicy {
        family_ttl: 3600,
        idle_ttl: 600,
        grace: 10,
    };
    let fam = exec
        .refresh_issue(&tenant, "u1", "h0", policy)
        .await
        .unwrap();
    assert_eq!(
        exec.refresh_family(&tenant, "h0").await.unwrap(),
        Some(fam.clone())
    );
    let ok = exec
        .refresh_rotate(&tenant, "h0", "h1", policy)
        .await
        .unwrap();
    assert_eq!(
        ok,
        Rotated::Ok {
            user_id: "u1".into(),
            family_id: fam.clone(),
            expires_in: 600
        }
    );
    // Within grace: one reissue, which revokes the previous child h1.
    assert!(matches!(
        exec.refresh_rotate(&tenant, "h0", "h1b", policy)
            .await
            .unwrap(),
        Rotated::Ok { .. }
    ));
    assert_eq!(
        exec.refresh_rotate(&tenant, "h1", "x", policy)
            .await
            .unwrap(),
        Rotated::Reuse {
            family_id: fam.clone()
        }
    );
    assert_eq!(
        exec.refresh_rotate(&tenant, "h1b", "y", policy)
            .await
            .unwrap(),
        Rotated::Invalid
    );
    let reasons = admin.query(&format!("SELECT revoke_reason FROM {schema}._platform_refresh_tokens WHERE family_id = $1::text::uuid"), &[&fam]).await.unwrap();
    assert!(reasons
        .iter()
        .any(|row| row.get::<_, Option<String>>(0).as_deref() == Some("reuse-detected")));
    // Revoke: only the owner's family; then all of a user's families.
    exec.refresh_issue(&tenant, "u1", "a0", policy)
        .await
        .unwrap();
    exec.refresh_issue(&tenant, "u1", "b0", policy)
        .await
        .unwrap();
    let n = exec
        .refresh_revoke(
            &tenant,
            RevokeTarget::FamilyOf {
                hash: "a0",
                user: "u2",
            },
        )
        .await
        .unwrap();
    assert_eq!(n, 0);
    let n = exec
        .refresh_revoke(
            &tenant,
            RevokeTarget::FamilyOf {
                hash: "a0",
                user: "u1",
            },
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        exec.refresh_rotate(&tenant, "a0", "z", policy)
            .await
            .unwrap(),
        Rotated::Invalid
    );
    assert_eq!(
        exec.refresh_revoke(&tenant, RevokeTarget::User("u1"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        exec.refresh_rotate(&tenant, "b0", "z", policy)
            .await
            .unwrap(),
        Rotated::Invalid
    );
    // A real second tenant (own schema, same role) cannot see these rows,
    // and its own tokens stay in its schema.
    let tenant2 = format!("ts{s}");
    let schema2 = format!("t_{tenant2}");
    for stmt in vibe::runtime::refresh::table_statements(&schema2).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    for stmt in vibe::runtime::refresh::runtime_grant_statements(&schema2, &role).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    assert_eq!(exec.refresh_family(&tenant2, "h1b").await.unwrap(), None);
    assert_eq!(
        exec.refresh_rotate(&tenant2, "h1b", "w", policy)
            .await
            .unwrap(),
        Rotated::Invalid
    );
    let fam_t2 = exec
        .refresh_issue(&tenant2, "u1", "t2tok", policy)
        .await
        .unwrap();
    assert_eq!(
        exec.refresh_family(&tenant2, "t2tok").await.unwrap(),
        Some(fam_t2)
    );
    assert_eq!(exec.refresh_family(&tenant, "t2tok").await.unwrap(), None);
    // Only hashes are stored.
    let row = admin
        .query_one(
            &format!(
                "SELECT count(*) FROM {schema}._platform_refresh_tokens WHERE token_hash = 'h0'"
            ),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 1);

    // Refresh family hardening: no grace reissue once the child was used.
    let fam2 = exec
        .refresh_issue(&tenant, "u1", "g0", policy)
        .await
        .unwrap();
    for (from, to) in [("g0", "g1"), ("g1", "g2")] {
        assert!(matches!(
            exec.refresh_rotate(&tenant, from, to, policy)
                .await
                .unwrap(),
            Rotated::Ok { .. }
        ));
    }
    assert_eq!(
        exec.refresh_rotate(&tenant, "g0", "gx", policy)
            .await
            .unwrap(),
        Rotated::Reuse {
            family_id: fam2.clone()
        }
    );
    assert_eq!(
        exec.refresh_rotate(&tenant, "g2", "gy", policy)
            .await
            .unwrap(),
        Rotated::Invalid
    );
    // Revoked tokens remain discoverable for reuse detection.
    assert_eq!(
        exec.refresh_family(&tenant, "g2").await.unwrap(),
        Some(fam2)
    );

    // Concurrent reuse and rotation on two connections: afterwards no live
    // token may remain in the family.
    let exec2 = PgExec::connect(url.as_str()).await.unwrap();
    for round in 0..20 {
        let p = format!("c{round}_");
        exec.refresh_issue(&tenant, "u1", &format!("{p}0"), policy)
            .await
            .unwrap();
        exec.refresh_rotate(&tenant, &format!("{p}0"), &format!("{p}1"), policy)
            .await
            .unwrap();
        exec.refresh_rotate(&tenant, &format!("{p}1"), &format!("{p}2"), policy)
            .await
            .unwrap();
        let (t0, tx, t2, t3) = (
            format!("{p}0"),
            format!("{p}x"),
            format!("{p}2"),
            format!("{p}3"),
        );
        let (a, b) = tokio::join!(
            exec.refresh_rotate(&tenant, &t0, &tx, policy),
            exec2.refresh_rotate(&tenant, &t2, &t3, policy)
        );
        assert!(matches!(a.unwrap(), Rotated::Reuse { .. }));
        let _ = b.unwrap();
        let live = admin
            .query_one(
                &format!(
                    "SELECT count(*) FROM {schema}._platform_refresh_tokens \
                     WHERE token_hash LIKE '{p}%' AND revoked_at IS NULL"
                ),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(live.get::<_, i64>(0), 0, "round {round}");
    }

    // logout-all racing rotations on another connection: afterwards no
    // live token of the user may remain.
    for round in 0..20 {
        let user = format!("la{round}");
        let mut tokens = Vec::new();
        for f in 0..3 {
            let t = format!("la{round}_{f}_0");
            exec.refresh_issue(&tenant, &user, &t, policy)
                .await
                .unwrap();
            tokens.push(t);
        }
        let rotate = async {
            for t in &tokens {
                let _ = exec2
                    .refresh_rotate(&tenant, t, &format!("{t}n"), policy)
                    .await
                    .unwrap();
            }
        };
        let (_, n) = tokio::join!(
            rotate,
            exec.refresh_revoke(&tenant, RevokeTarget::User(&user))
        );
        n.unwrap();
        let live = admin
            .query_one(
                &format!(
                    "SELECT count(*) FROM {schema}._platform_refresh_tokens \
                     WHERE user_id = '{user}' AND revoked_at IS NULL"
                ),
                &[],
            )
            .await
            .unwrap();
        assert_eq!(live.get::<_, i64>(0), 0, "round {round}");
    }

    // App roles cannot reach the table, even after a schema-wide grant.
    for app_role in ["public_reader", "public_writer", "app_rw"] {
        let _ = admin
            .batch_execute(&format!("CREATE ROLE {app_role} NOLOGIN"))
            .await;
        admin
            .batch_execute(&format!(
                "GRANT ALL ON ALL TABLES IN SCHEMA {schema} TO {app_role}"
            ))
            .await
            .unwrap();
    }
    for stmt in vibe::runtime::refresh::table_statements(&schema).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    for app_role in ["public_reader", "public_writer", "app_rw"] {
        let row = admin
            .query_one(
                &format!(
                    "SELECT has_table_privilege('{app_role}', '{schema}._platform_refresh_tokens', 'SELECT')"
                ),
                &[],
            )
            .await
            .unwrap();
        assert!(!row.get::<_, bool>(0), "{app_role}");
    }
    admin
        .batch_execute(&format!(
            "DROP SCHEMA {schema} CASCADE; DROP SCHEMA {schema2} CASCADE; DROP ROLE {role}"
        ))
        .await
        .unwrap();
}

#[test]
fn app_sql_cannot_reach_the_refresh_table() {
    for sql in [
        "SELECT token_hash FROM _platform_refresh_tokens",
        "SELECT token_hash FROM \"_platform_refresh_tokens\"",
        "SELECT token_hash FROM t_tenant0001._platform_refresh_tokens",
        "UPDATE _platform_refresh_tokens SET revoked_at = NULL WHERE id = $1",
        "SELECT id FROM users WHERE id IN (SELECT user_id FROM _platform_refresh_tokens)",
        "TABLE _platform_refresh_tokens",
        "SELECT id FROM users UNION TABLE _platform_refresh_tokens",
        "SELECT id FROM users UNION ALL TABLE _platform_refresh_tokens",
        "SELECT id FROM users EXCEPT TABLE _platform_refresh_tokens",
        "INSERT INTO users (id) TABLE _platform_refresh_tokens",
    ] {
        assert!(vibe::sqlguard::query_problem(sql).is_some(), "{sql}");
    }
}

/// Memory store whose family lookup can be switched to fail (for example a
/// pool timeout); everything else is delegated.
struct FamilyLookupFails {
    inner: Arc<MemoryExec>,
    down: Arc<std::sync::atomic::AtomicBool>,
    lookup_delay_ms: u64,
    rotate_delay_ms: u64,
    signer_delay_ms: u64,
    signer_timeout: bool,
    reservation_overrun: bool,
    signer_down: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl vibe::runtime::QueryExec for FamilyLookupFails {
    async fn execute(
        &self,
        tenant_id: &str,
        role: &str,
        stmt: vibe::runtime::Stmt,
    ) -> Result<Vec<vibe::runtime::Row>, vibe::runtime::ExecError> {
        self.inner.execute(tenant_id, role, stmt).await
    }

    async fn raw(
        &self,
        tenant_id: &str,
        role: &str,
        writes: bool,
        sql: &str,
        params: Vec<Option<String>>,
    ) -> Result<Vec<vibe::runtime::Row>, vibe::runtime::ExecError> {
        if self.reservation_overrun
            && self
                .signer_down
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            // Synchronous issuance can finish after the reservation lifetime.
            std::thread::sleep(std::time::Duration::from_secs(16));
        }
        tokio::time::sleep(std::time::Duration::from_millis(self.signer_delay_ms)).await;
        if self.signer_down.load(std::sync::atomic::Ordering::SeqCst) {
            if self.signer_timeout {
                return Err(vibe::runtime::ExecError::Timeout);
            }
            return Err(vibe::runtime::ExecError::Message(
                "issuance unavailable".into(),
            ));
        }
        if writes {
            self.inner.seed(
                tenant_id,
                "issuance_audit",
                json!({"id": "issued"}).as_object().unwrap().clone(),
            );
        }
        self.inner.raw(tenant_id, role, writes, sql, params).await
    }

    async fn user_roles(
        &self,
        tenant_id: &str,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, vibe::runtime::ExecError> {
        self.inner.user_roles(tenant_id, user_id).await
    }

    async fn refresh_issue(
        &self,
        tenant_id: &str,
        user_id: &str,
        token_hash: &str,
        policy: vibe::runtime::refresh::RefreshPolicy,
    ) -> Result<String, vibe::runtime::ExecError> {
        self.inner
            .refresh_issue(tenant_id, user_id, token_hash, policy)
            .await
    }

    async fn refresh_rotate(
        &self,
        tenant_id: &str,
        token_hash: &str,
        new_hash: &str,
        policy: vibe::runtime::refresh::RefreshPolicy,
    ) -> Result<vibe::runtime::refresh::Rotated, vibe::runtime::ExecError> {
        tokio::time::sleep(std::time::Duration::from_millis(self.rotate_delay_ms)).await;
        self.inner
            .refresh_rotate(tenant_id, token_hash, new_hash, policy)
            .await
    }

    async fn refresh_rotate_authorized(
        &self,
        tenant_id: &str,
        token_hash: &str,
        new_hash: &str,
        policy: vibe::runtime::refresh::RefreshPolicy,
        issuer: Option<&dyn vibe::runtime::RefreshIssuer>,
    ) -> Result<(vibe::runtime::refresh::Rotated, Option<Value>), vibe::runtime::ExecError> {
        tokio::time::sleep(std::time::Duration::from_millis(self.rotate_delay_ms)).await;
        self.inner
            .refresh_rotate_authorized(tenant_id, token_hash, new_hash, policy, issuer)
            .await
    }

    async fn refresh_revoke(
        &self,
        tenant_id: &str,
        target: vibe::runtime::refresh::RevokeTarget<'_>,
    ) -> Result<u64, vibe::runtime::ExecError> {
        self.inner.refresh_revoke(tenant_id, target).await
    }

    async fn refresh_subject(
        &self,
        tenant_id: &str,
        token_hash: &str,
    ) -> Result<Option<String>, vibe::runtime::ExecError> {
        self.inner.refresh_subject(tenant_id, token_hash).await
    }

    async fn refresh_family(
        &self,
        tenant_id: &str,
        token_hash: &str,
    ) -> Result<Option<String>, vibe::runtime::ExecError> {
        tokio::time::sleep(std::time::Duration::from_millis(self.lookup_delay_ms)).await;
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(vibe::runtime::ExecError::Timeout);
        }
        self.inner.refresh_family(tenant_id, token_hash).await
    }
}

#[tokio::test]
async fn refresh_timeout_returns_504_tx_timeout_and_preserves_token() {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = down.clone();
    let a = app_wrapped(manifest(0), move |inner| {
        Arc::new(FamilyLookupFails {
            inner,
            down: flag,
            lookup_delay_ms: 0,
            rotate_delay_ms: 0,
            signer_delay_ms: 0,
            signer_timeout: false,
            reservation_overrun: false,
            signer_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    });
    let r0 = tok(&a.login().await, "refresh_token");
    let (s, body, _) = a.refresh(&r0).await;
    assert_eq!(s, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert!(body.to_string().contains("TX_TIMEOUT"), "{body}");
    assert!(body.get("access_token").is_none());
    // Unknown tokens get the same retryable answer while the store is down.
    let (s, _, _) = a.refresh("not-a-token").await;
    assert_eq!(s, StatusCode::GATEWAY_TIMEOUT);
    // The token was not consumed: it rotates once the store answers again.
    down.store(false, std::sync::atomic::Ordering::SeqCst);
    let (s, body, _) = a.refresh(&r0).await;
    assert_eq!(s, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn refresh_waits_complete_and_new_tokens_remain_usable() {
    for (lookup_delay_ms, rotate_delay_ms, signer_delay_ms) in
        [(400, 0, 0), (0, 400, 0), (0, 0, 400)]
    {
        let a = app_wrapped(manifest(10), move |inner| {
            Arc::new(FamilyLookupFails {
                inner,
                down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                lookup_delay_ms,
                rotate_delay_ms,
                signer_delay_ms,
                signer_timeout: false,
                reservation_overrun: false,
                signer_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            })
        });
        let token = tok(&a.login().await, "refresh_token");
        let (status, body, elapsed) = a.refresh(&token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(elapsed >= 400);
        assert!(body.get("access_token").is_some());
        let next = tok(&body, "refresh_token");
        let (status, body, _) = a.refresh(&next).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.get("access_token").is_some());
    }
}

/// Cleanup of finished families against a real database, run as a
/// non-superuser table owner (FORCE RLS applies).
#[tokio::test]
async fn postgres_cleanup_removes_finished_families_in_batches() {
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        return;
    };
    let (admin, conn) = tokio_postgres::connect(&admin_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let s = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let tenant = format!("tc{s}");
    let schema = format!("t_{tenant}");
    let owner = format!("refresh_owner_{s}");
    admin
        .batch_execute(&format!("CREATE ROLE {owner} LOGIN PASSWORD 'pw'"))
        .await
        .unwrap();
    for stmt in vibe::runtime::refresh::table_statements(&schema).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    let t = format!("{schema}._platform_refresh_tokens");
    admin
        .batch_execute(&format!(
            "ALTER SCHEMA {schema} OWNER TO {owner}; ALTER TABLE {t} OWNER TO {owner};"
        ))
        .await
        .unwrap();
    // 2.5 batches of finished families, one family inside the retention
    // window and one live family.
    let finished = vibe::runtime::refresh::CLEANUP_BATCH * 5 / 2;
    admin
        .batch_execute(&format!(
            "INSERT INTO {t} (id, tenant_id, family_id, user_id, token_hash, created_at, family_expires_at, idle_expires_at) \
             SELECT gen_random_uuid(), '{tenant}', gen_random_uuid(), 'u1', 'old' || g, now() - interval '40 days', \
                    now() - interval '2 days', now() - interval '9 days' FROM generate_series(1, {finished}) g; \
             INSERT INTO {t} (id, tenant_id, family_id, user_id, token_hash, created_at, family_expires_at, idle_expires_at) VALUES \
             (gen_random_uuid(), '{tenant}', gen_random_uuid(), 'u1', 'recent', now() - interval '30 days', now() - interval '1 hour', now() - interval '1 hour'), \
             (gen_random_uuid(), '{tenant}', gen_random_uuid(), 'u1', 'live', now(), now() + interval '30 days', now() + interval '7 days');"
        ))
        .await
        .unwrap();
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&owner).unwrap();
    url.set_password(Some("pw")).unwrap();
    let (as_owner, conn) = tokio_postgres::connect(url.as_str(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    for stmt in vibe::runtime::refresh::cleanup_statements(&schema).unwrap() {
        as_owner.batch_execute(&stmt).await.unwrap();
    }
    let left: Vec<String> = admin
        .query(
            &format!("SELECT token_hash FROM {t} ORDER BY token_hash"),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(left, vec!["live".to_string(), "recent".to_string()]);
    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE; DROP ROLE {owner}"))
        .await
        .unwrap();
}

#[test]
fn startup_rejects_reuse_grace_outside_the_spec_limits() {
    use vibe::manifest::{refresh_spec_problem, RefreshSpec};
    let spec = |grace: i64| RefreshSpec {
        sign_method: "auth.sign".into(),
        refresh_ttl_s: 2_592_000,
        idle_ttl_s: 604_800,
        reuse_grace_s: grace,
    };
    assert_eq!(refresh_spec_problem("auth.refresh", &spec(10)), None);
    assert_eq!(refresh_spec_problem("auth.refresh", &spec(1)), None);
    for bad in [11, 60, 0, -1] {
        let err = refresh_spec_problem("auth.refresh", &spec(bad)).unwrap();
        assert!(
            err.contains("reuse-grace") && err.contains("between 1s and 10s"),
            "{err}"
        );
    }
}

/// Refresh table hardening against a real database: column-level UPDATE,
/// final revocation, immutable identity columns, and the startup check.
#[tokio::test]
async fn postgres_refresh_table_grants_and_guard() {
    use vibe::runtime::refresh::{check_store, runtime_grant_statements, table_statements};
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        return;
    };
    let (admin, conn) = tokio_postgres::connect(&admin_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let s = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let tenant = format!("tg{s}");
    let schema = format!("t_{tenant}");
    let role = format!("refresh_rt_{s}");
    let t = format!("{schema}._platform_refresh_tokens");
    admin
        .batch_execute(&format!("CREATE ROLE {role} LOGIN PASSWORD 'pw'"))
        .await
        .unwrap();
    let setup = || async {
        for stmt in table_statements(&schema)
            .unwrap()
            .into_iter()
            .chain(runtime_grant_statements(&schema, &role).unwrap())
        {
            admin.batch_execute(&stmt).await.unwrap();
        }
    };
    setup().await;
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&role).unwrap();
    url.set_password(Some("pw")).unwrap();
    let (rt, conn) = tokio_postgres::connect(url.as_str(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let tenants = vec![tenant.clone()];
    check_store(&rt, &tenants).await.unwrap();

    let in_tenant = |sql: String| {
        format!("BEGIN; SELECT set_config('app.tenant_id', '{tenant}', true); {sql}; COMMIT;")
    };
    rt.batch_execute(&in_tenant(format!(
        "INSERT INTO {t} (id, tenant_id, family_id, user_id, token_hash, created_at, family_expires_at, idle_expires_at) \
         VALUES (gen_random_uuid(), '{tenant}', gen_random_uuid(), 'u1', 'h1', now(), now() + interval '1 day', now() + interval '1 hour')"
    )))
    .await
    .unwrap();
    rt.batch_execute(&in_tenant(format!(
        "UPDATE {t} SET revoked_at = now(), revoke_reason = 'logout' WHERE token_hash = 'h1'"
    )))
    .await
    .unwrap();
    let _ = rt.batch_execute("ROLLBACK").await;
    // Revocation is final: no clearing, no moving the time, no new reason.
    for change in [
        "revoked_at = NULL",
        "revoked_at = now() + interval '1 day'",
        "revoke_reason = 'other'",
    ] {
        let err = rt
            .batch_execute(&in_tenant(format!(
                "UPDATE {t} SET {change} WHERE token_hash = 'h1'"
            )))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("final") || format!("{err:?}").contains("final"),
            "{change}: {err:?}"
        );
        let _ = rt.batch_execute("ROLLBACK").await;
    }
    // Identity columns: the runtime role has no UPDATE on them, and the
    // trigger refuses changes from any role (here the superuser).
    for change in [
        "token_hash = 'h2'",
        "family_id = gen_random_uuid()",
        "tenant_id = 'other'",
        "user_id = 'u2'",
    ] {
        assert!(
            rt.batch_execute(&in_tenant(format!(
                "UPDATE {t} SET {change} WHERE token_hash = 'h1'"
            )))
            .await
            .is_err(),
            "{change}"
        );
        let _ = rt.batch_execute("ROLLBACK").await;
        let err = admin
            .batch_execute(&format!("UPDATE {t} SET {change} WHERE token_hash = 'h1'"))
            .await
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("immutable"),
            "{change}: {err:?}"
        );
    }
    assert!(
        rt.batch_execute(&in_tenant(format!(
            "DELETE FROM {t} WHERE token_hash = 'h1'"
        )))
        .await
        .is_err(),
        "no DELETE for the runtime role"
    );
    let _ = rt.batch_execute("ROLLBACK").await;

    // Startup check fails closed on each drift, and passes again after migrate.
    for drift in [
        format!("GRANT DELETE ON {t} TO {role}"),
        format!("GRANT UPDATE ON {t} TO {role}"),
        format!("GRANT UPDATE (token_hash) ON {t} TO {role}"),
        format!("REVOKE UPDATE (revoked_at) ON {t} FROM {role}"),
        format!("REVOKE INSERT ON {t} FROM {role}"),
        format!("DROP TRIGGER _platform_refresh_guard ON {t}"),
        format!("ALTER TABLE {t} DISABLE TRIGGER _platform_refresh_guard"),
        format!("DROP TRIGGER _platform_refresh_guard ON {t}; CREATE TRIGGER _platform_refresh_guard AFTER UPDATE ON {t} FOR EACH ROW EXECUTE FUNCTION {schema}._platform_refresh_guard()"),
        format!("CREATE OR REPLACE FUNCTION {schema}._platform_refresh_guard() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$"),
    ] {
        admin.batch_execute(&drift).await.unwrap();
        assert!(check_store(&rt, &tenants).await.is_err(), "{drift}");
        setup().await;
        check_store(&rt, &tenants).await.unwrap();
    }
    // A tenant without the table also fails.
    assert!(check_store(&rt, &[format!("tz{s}")]).await.is_err());
    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE; DROP ROLE {role}"))
        .await
        .unwrap();
}

fn app_with_issuance_control(
    grace: i64,
    writes: bool,
) -> (App, Arc<std::sync::atomic::AtomicBool>) {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = down.clone();
    let mut m = manifest(grace);
    if writes {
        let method = m
            .methods
            .iter_mut()
            .find(|m| m.target == "auth.sign")
            .unwrap();
        method.writes = true;
        method.steps.insert(
            0,
            serde_json::from_value(step(
                "query",
                "audit",
                json!({
                    "table": "issuance_audit", "sql": "insert into issuance_audit (id) values ($1)",
                    "params": ["input.user_id"], "writes": true
                }),
            ))
            .unwrap(),
        );
    }
    let a = app_wrapped(m, move |inner| {
        Arc::new(FamilyLookupFails {
            inner,
            down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            lookup_delay_ms: 0,
            rotate_delay_ms: 0,
            signer_delay_ms: 0,
            signer_timeout: false,
            reservation_overrun: false,
            signer_down: flag,
        })
    });
    (a, down)
}

#[tokio::test]
async fn reuse_revokes_the_family_when_issuance_is_unavailable() {
    for grace in [0, 10] {
        let (a, down) = app_with_issuance_control(grace, false);
        let parent = tok(&a.login().await, "refresh_token");
        let (status, first, _) = a.refresh(&parent).await;
        assert_eq!(status, StatusCode::OK);
        let (presented, current) = if grace == 0 {
            (parent, tok(&first, "refresh_token"))
        } else {
            let (status, retry, _) = a.refresh(&parent).await;
            assert_eq!(status, StatusCode::OK);
            (tok(&first, "refresh_token"), tok(&retry, "refresh_token"))
        };
        down.store(true, std::sync::atomic::Ordering::SeqCst);
        // A current token reaches issuance and remains available on failure.
        assert_eq!(a.refresh(&current).await.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(a.refresh(&presented).await.0, StatusCode::UNAUTHORIZED);
        down.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(a.refresh(&current).await.0, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn rejected_tokens_do_not_run_sign_method_writes() {
    use vibe::runtime::{Col, QueryExec, Stmt};
    for state in ["invalid", "revoked", "expired", "reused"] {
        let (a, _) = app_with_issuance_control(0, true);
        let login = a.login().await;
        let token = tok(&login, "refresh_token");
        let presented = match state {
            "invalid" => "unknown".to_string(),
            "revoked" => {
                a.post(
                    "/v1/logout",
                    Some(&tok(&login, "access_token")),
                    json!({"refresh_token": token}),
                )
                .await;
                token
            }
            "expired" => {
                use hmac::{Hmac, KeyInit, Mac};
                use sha2::Sha256;
                let mut mac = Hmac::<Sha256>::new_from_slice(&a.state.auth.pepper).unwrap();
                mac.update(b"expired-token");
                let hash = hex::encode(mac.finalize().into_bytes());
                a.exec
                    .refresh_issue(
                        TENANT,
                        "u1",
                        &hash,
                        vibe::runtime::refresh::RefreshPolicy {
                            family_ttl: -1,
                            idle_ttl: -1,
                            grace: 0,
                        },
                    )
                    .await
                    .unwrap();
                "expired-token".to_string()
            }
            _ => {
                assert_eq!(a.refresh(&token).await.0, StatusCode::OK);
                token
            }
        };
        let audit_rows = || {
            a.exec.execute(
                TENANT,
                "public_reader",
                Stmt::Select {
                    table: "issuance_audit".into(),
                    columns: vec![Col {
                        name: "id".into(),
                        ty: "text".into(),
                    }],
                    id: None,
                    filters: vec![],
                    sort: None,
                    limit: 100,
                    offset: 0,
                },
            )
        };
        let before = audit_rows().await.unwrap();
        assert_eq!(
            a.refresh(&presented).await.0,
            StatusCode::UNAUTHORIZED,
            "{state}"
        );
        assert_eq!(audit_rows().await.unwrap(), before, "{state}");
        if state == "reused" {
            assert_eq!(before.len(), 1, "a current token runs the write step");
        } else {
            assert!(before.is_empty());
        }
    }
}

#[tokio::test]
async fn postgres_rotation_lock_wait_is_bounded() {
    use vibe::runtime::{PgExec, PgOptions, QueryExec};
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        return;
    };
    let (mut admin, conn) = tokio_postgres::connect(&admin_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let tenant = format!("tl{suffix}");
    let schema = format!("t_{tenant}");
    let role = format!("refresh_lock_{suffix}");
    admin
        .batch_execute(&format!("CREATE ROLE {role} LOGIN PASSWORD 'pw'"))
        .await
        .unwrap();
    for stmt in vibe::runtime::refresh::table_statements(&schema).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    for stmt in vibe::runtime::refresh::runtime_grant_statements(&schema, &role).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&role).unwrap();
    url.set_password(Some("pw")).unwrap();
    let exec = PgExec::connect_with(
        url.as_str(),
        PgOptions {
            statement_timeout_ms: 150,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let policy = vibe::runtime::refresh::RefreshPolicy::default();
    let family = exec
        .refresh_issue(&tenant, "u1", "locked", policy)
        .await
        .unwrap();
    for advisory in [true, false] {
        let tx = admin.transaction().await.unwrap();
        if advisory {
            tx.execute(
                "SELECT pg_advisory_xact_lock(hashtext('vibe-refresh:' || $1))",
                &[&family],
            )
            .await
            .unwrap();
        } else {
            tx.query_one(&format!("SELECT id FROM {schema}._platform_refresh_tokens WHERE token_hash = 'locked' FOR UPDATE"), &[]).await.unwrap();
        }
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            exec.refresh_rotate(&tenant, "locked", "next", policy),
        )
        .await;
        assert!(result.is_ok(), "rotation exceeded the timeout bound");
        assert!(
            matches!(result.unwrap(), Err(vibe::runtime::ExecError::Timeout)),
            "lock timeout must be classified by SQLSTATE"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let revoke = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            exec.refresh_revoke(
                &tenant,
                vibe::runtime::refresh::RevokeTarget::FamilyOf {
                    hash: "locked",
                    user: "u1",
                },
            ),
        )
        .await
        .expect("revocation exceeded the timeout bound");
        assert!(
            revoke.is_err(),
            "revocation must fail while the lock is held"
        );
        tx.rollback().await.unwrap();
    }
    assert!(matches!(
        exec.refresh_rotate(&tenant, "locked", "next", policy)
            .await
            .unwrap(),
        vibe::runtime::refresh::Rotated::Ok { .. }
    ));
    drop(exec);
    admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE; DROP ROLE {role}"))
        .await
        .unwrap();
}

#[test]
fn refresh_cleanup_uses_wall_clock_time() {
    let sql = vibe::runtime::refresh::cleanup_statements("t_tenant0001")
        .unwrap()
        .join("\n");
    assert!(sql.contains("family_expires_at <= clock_timestamp()"));
    assert!(!sql.contains("now()"));
}

#[tokio::test]
async fn issuance_failure_keeps_the_current_token_available() {
    let (a, down) = app_with_issuance_control(0, false);
    let token = tok(&a.login().await, "refresh_token");
    down.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(a.refresh(&token).await.0, StatusCode::SERVICE_UNAVAILABLE);
    down.store(false, std::sync::atomic::Ordering::SeqCst);
    let (status, body, _) = a.refresh(&token).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        a.refresh(&tok(&body, "refresh_token")).await.0,
        StatusCode::OK
    );
}

async fn check_issuance_without_store_connection(
    exec: &dyn vibe::runtime::QueryExec,
    tenant: &str,
    scenario: &str,
) {
    use vibe::runtime::refresh::{RefreshPolicy, Rotated};
    use vibe::runtime::{ExecError, RefreshIssuer};
    struct Issuer<'a> {
        exec: &'a dyn vibe::runtime::QueryExec,
        tenant: &'a str,
        entered: tokio::sync::Notify,
        resume: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl RefreshIssuer for Issuer<'_> {
        async fn issue(&self, _: &str) -> Result<Value, ExecError> {
            self.exec
                .refresh_family(self.tenant, "pending-parent")
                .await?;
            self.entered.notify_one();
            self.resume.notified().await;
            Ok(json!({"access_token": "issued"}))
        }
    }
    struct FailingIssuer;
    #[async_trait::async_trait]
    impl RefreshIssuer for FailingIssuer {
        async fn issue(&self, _: &str) -> Result<Value, ExecError> {
            Err(ExecError::Message("issuance unavailable".into()))
        }
    }
    let policy = RefreshPolicy {
        grace: if ["past-grace", "retry"].contains(&scenario) {
            1
        } else {
            10
        },
        ..RefreshPolicy::default()
    };
    if scenario == "issuance-failure" {
        exec.refresh_issue(tenant, "failure-user", "failure-parent", policy)
            .await
            .unwrap();
        assert!(exec
            .refresh_rotate_authorized(
                tenant,
                "failure-parent",
                "failed-child",
                policy,
                Some(&FailingIssuer)
            )
            .await
            .is_err());
        assert_eq!(
            exec.refresh_family(tenant, "failed-child").await.unwrap(),
            None
        );
        assert!(matches!(
            exec.refresh_rotate(tenant, "failure-parent", "retry-child", policy)
                .await
                .unwrap(),
            Rotated::Ok { .. }
        ));
    }
    if scenario == "cancel" {
        // Dropping a request after reservation must never produce a later session.
        exec.refresh_issue(tenant, "cancel-user", "cancel-parent", policy)
            .await
            .unwrap();
        let cancelled = Issuer {
            exec,
            tenant,
            entered: Default::default(),
            resume: Default::default(),
        };
        {
            let rotation = exec.refresh_rotate_authorized(
                tenant,
                "cancel-parent",
                "cancel-child",
                policy,
                Some(&cancelled),
            );
            tokio::pin!(rotation);
            tokio::select! {
                _ = cancelled.entered.notified() => {},
                result = &mut rotation => panic!("rotation completed before cancellation: {result:?}"),
            }
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                match exec
                    .refresh_rotate(tenant, "cancel-parent", "cancel-retry", policy)
                    .await
                {
                    Err(ExecError::Conflict) => tokio::task::yield_now().await,
                    Ok(Rotated::Ok { .. }) => break,
                    other => panic!("cancelled reservation rotated: {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            exec.refresh_family(tenant, "cancel-child").await.unwrap(),
            None
        );
        assert!(exec
            .refresh_family(tenant, "cancel-retry")
            .await
            .unwrap()
            .is_some());
    }
    if scenario == "pending" {
        exec.refresh_issue(tenant, "pending-user", "pending-parent", policy)
            .await
            .unwrap();
        let issuer = Issuer {
            exec,
            tenant,
            entered: Default::default(),
            resume: Default::default(),
        };
        let rotation = exec.refresh_rotate_authorized(
            tenant,
            "pending-parent",
            "pending-child",
            policy,
            Some(&issuer),
        );
        let concurrent = async {
            issuer.entered.notified().await;
            assert!(matches!(
                exec.refresh_rotate(tenant, "pending-parent", "other-child", policy)
                    .await,
                Err(ExecError::Conflict)
            ));
            exec.refresh_issue(tenant, "other-user", "other-family", policy)
                .await
                .unwrap();
            issuer.resume.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(rotation, concurrent)
        })
        .await
        .expect("store operations must complete during issuance");
        let (rotated, value) = result.unwrap();
        assert!(matches!(rotated, Rotated::Ok { .. }));
        assert_eq!(value.unwrap()["access_token"], "issued");
        assert!(matches!(
            exec.refresh_rotate(tenant, "pending-child", "next-child", policy)
                .await
                .unwrap(),
            Rotated::Ok { .. }
        ));
    }
    if ["child-rotation-failure", "child-rotation-cancel"].contains(&scenario) {
        exec.refresh_issue(tenant, "user", "failure-parent", policy)
            .await
            .unwrap();
        exec.refresh_rotate(tenant, "failure-parent", "failure-child", policy)
            .await
            .unwrap();
        struct ChildIssuer<'a> {
            exec: &'a dyn vibe::runtime::QueryExec,
            tenant: &'a str,
            cancel: bool,
            entered: tokio::sync::Notify,
        }
        #[async_trait::async_trait]
        impl RefreshIssuer for ChildIssuer<'_> {
            async fn issue(&self, _: &str) -> Result<Value, ExecError> {
                self.exec
                    .refresh_rotate(
                        self.tenant,
                        "failure-child",
                        "failure-descendant",
                        RefreshPolicy::default(),
                    )
                    .await?;
                self.entered.notify_one();
                if self.cancel {
                    std::future::pending::<()>().await;
                }
                Err(ExecError::IssuanceFailed)
            }
        }
        let issuer = ChildIssuer {
            exec,
            tenant,
            cancel: scenario == "child-rotation-cancel",
            entered: Default::default(),
        };
        let mut rotation = Box::pin(exec.refresh_rotate_authorized(
            tenant,
            "failure-parent",
            "failure-new",
            policy,
            Some(&issuer),
        ));
        if issuer.cancel {
            tokio::select! { _ = issuer.entered.notified() => {}, result = &mut rotation => panic!("unexpected completion: {result:?}") }
            drop(rotation);
        } else {
            assert!(matches!(rotation.await.unwrap().0, Rotated::Reuse { .. }));
        }
        // PostgreSQL cancellation cleanup runs in a separate transaction.
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if matches!(
                    exec.refresh_rotate_authorized(
                        tenant,
                        "failure-descendant",
                        "failure-probe",
                        policy,
                        Some(&FailingIssuer)
                    )
                    .await,
                    Ok((Rotated::Invalid, None))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            exec.refresh_rotate(tenant, "failure-descendant", "failure-retry", policy)
                .await
                .unwrap(),
            Rotated::Invalid
        );
    }
    if scenario == "retry" {
        for late in [false, true] {
            let parent = format!("retry-parent-{late}");
            let child = format!("retry-child-{late}");
            let retry = format!("retry-result-{late}");
            exec.refresh_issue(tenant, "retry-user", &parent, policy)
                .await
                .unwrap();
            let issuer = Issuer {
                exec,
                tenant,
                entered: Default::default(),
                resume: Default::default(),
            };
            let rotation =
                exec.refresh_rotate_authorized(tenant, &parent, &child, policy, Some(&issuer));
            let concurrent = async {
                issuer.entered.notified().await;
                assert!(matches!(
                    exec.refresh_rotate(tenant, &parent, &retry, policy).await,
                    Err(ExecError::Conflict)
                ));
                issuer.resume.notify_one();
            };
            let (result, ()) = tokio::join!(rotation, concurrent);
            assert!(matches!(result.unwrap().0, Rotated::Ok { .. }));
            if late {
                tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
            }
            let result = exec
                .refresh_rotate(tenant, &parent, &retry, policy)
                .await
                .unwrap();
            if late {
                assert!(matches!(result, Rotated::Reuse { .. }));
                assert_eq!(
                    exec.refresh_rotate(tenant, &child, "late-child", policy)
                        .await
                        .unwrap(),
                    Rotated::Invalid
                );
            } else {
                assert!(matches!(result, Rotated::Ok { .. }));
                assert!(matches!(
                    exec.refresh_rotate(tenant, &retry, "retry-next", policy)
                        .await
                        .unwrap(),
                    Rotated::Ok { .. }
                ));
            }
        }
    }
    if ["revoke", "child-rotation", "concurrent-grace", "past-grace"].contains(&scenario) {
        let parent = format!("{scenario}-parent");
        let child = format!("{scenario}-child");
        exec.refresh_issue(tenant, scenario, &parent, policy)
            .await
            .unwrap();
        if scenario != "revoke" {
            exec.refresh_rotate(tenant, &parent, &child, policy)
                .await
                .unwrap();
        }
        let issuer = Issuer {
            exec,
            tenant,
            entered: Default::default(),
            resume: Default::default(),
        };
        let new_child = format!("{scenario}-new");
        let rotation =
            exec.refresh_rotate_authorized(tenant, &parent, &new_child, policy, Some(&issuer));
        let concurrent = async {
            issuer.entered.notified().await;
            match scenario {
                "revoke" => {
                    exec.refresh_revoke(
                        tenant,
                        vibe::runtime::refresh::RevokeTarget::User(scenario),
                    )
                    .await
                    .unwrap();
                }
                "child-rotation" => {
                    assert!(matches!(
                        exec.refresh_rotate(tenant, &child, "child-descendant", policy)
                            .await
                            .unwrap(),
                        Rotated::Ok { .. }
                    ));
                }
                "concurrent-grace" => {
                    assert!(matches!(
                        exec.refresh_rotate(tenant, &parent, "grace-concurrent", policy)
                            .await,
                        Err(ExecError::Conflict)
                    ));
                }
                "past-grace" => {
                    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
                    assert!(matches!(
                        exec.refresh_rotate(tenant, &parent, "past-grace-new", policy)
                            .await
                            .unwrap(),
                        Rotated::Reuse { .. }
                    ));
                }
                _ => unreachable!(),
            }
            issuer.resume.notify_one();
        };
        let (result, ()) = tokio::join!(rotation, concurrent);
        if scenario == "concurrent-grace" {
            assert!(matches!(result.unwrap().0, Rotated::Ok { .. }));
            assert_eq!(
                exec.refresh_family(tenant, "grace-concurrent")
                    .await
                    .unwrap(),
                None
            );
        } else {
            if scenario == "child-rotation" {
                assert!(matches!(result.unwrap().0, Rotated::Reuse { .. }));
                assert_eq!(
                    exec.refresh_rotate(tenant, "child-descendant", "descendant-retry", policy)
                        .await
                        .unwrap(),
                    Rotated::Invalid
                );
            } else {
                assert_eq!(result.unwrap().0, Rotated::Invalid);
            }
            assert_eq!(exec.refresh_family(tenant, &new_child).await.unwrap(), None);
            if scenario == "past-grace" {
                assert_eq!(
                    exec.refresh_rotate(tenant, &child, "revoked-child-retry", policy)
                        .await
                        .unwrap(),
                    Rotated::Invalid
                );
            }
        }
    }
}

#[tokio::test]
async fn memory_issuance_allows_concurrent_store_requests() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "pending").await;
}

#[tokio::test]
async fn authorization_errors_on_refresh_and_logout_are_not_cacheable() {
    let mut m = manifest(0);
    m.routes[0].roles = vec!["required-role".into()];
    m.routes[1].roles = vec!["required-role".into()];
    let a = app_wrapped(m, |inner| inner);
    assert_eq!(a.refresh("unknown").await.0, StatusCode::FORBIDDEN);
    let access = tok(&a.login().await, "access_token");
    assert_eq!(
        a.post("/v1/logout", Some(&access), json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        a.post("/v1/logout", None, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn invalid_slug_does_not_set_a_clearing_cookie() {
    let a = app(0);
    // Exercise the response path independently of slug file validation.
    let slugs = HashMap::from([(
        "bad;slug".into(),
        Slug {
            tenant_id: TENANT.into(),
            public_access: true,
            public_write: true,
        },
    )]);
    let state = Arc::new(AppState {
        manifest: a.manifest.clone(),
        exec: a.exec.clone(),
        auth: auth(&a.manifest),
        dev_slugs: None,
        slugs,
        rates: Default::default(),
        quotas: Default::default(),
        trusted_proxies: vec![],
        disabled: Default::default(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        dev_hints: false,
        challenge: None,
    });
    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/v1/bad;slug/token/refresh")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(!response.headers().contains_key("set-cookie"));
}

#[tokio::test]
async fn memory_revocation_during_issuance_prevents_finalization() {
    use vibe::runtime::refresh::{RefreshPolicy, RevokeTarget, Rotated};
    use vibe::runtime::{ExecError, QueryExec, RefreshIssuer};
    struct Issuer<'a>(&'a MemoryExec);
    #[async_trait::async_trait]
    impl RefreshIssuer for Issuer<'_> {
        async fn issue(&self, user: &str) -> Result<Value, ExecError> {
            self.0
                .refresh_revoke(TENANT, RevokeTarget::User(user))
                .await?;
            Ok(json!({"access_token": "issued"}))
        }
    }
    let exec = MemoryExec::new();
    let policy = RefreshPolicy::default();
    exec.refresh_issue(TENANT, "u1", "parent", policy)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        exec.refresh_rotate_authorized(TENANT, "parent", "child", policy, Some(&Issuer(&exec))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, (Rotated::Invalid, None));
    assert_eq!(exec.refresh_family(TENANT, "child").await.unwrap(), None);
}

#[tokio::test]
async fn pending_refresh_returns_retryable_response_without_clearing_cookie() {
    let a = app_wrapped(manifest(10), |inner| {
        Arc::new(FamilyLookupFails {
            inner,
            down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            lookup_delay_ms: 0,
            rotate_delay_ms: 0,
            signer_delay_ms: 600,
            signer_timeout: false,
            reservation_overrun: false,
            signer_down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    });
    let token = tok(&a.login().await, "refresh_token");
    let first = a.refresh(&token);
    let second = async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let response = a
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/v1/acme/token/refresh")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"refresh_token": token}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "TX_CONFLICT");
        assert_eq!(body["error"]["retryable"], true);
    };
    let (first, ()) = tokio::join!(first, second);
    assert_eq!(first.0, StatusCode::OK);
}

#[tokio::test]
async fn refresh_and_logout_method_errors_are_not_cacheable() {
    let a = app(10);
    for path in ["/auth/v1/acme/token/refresh", "/v1/logout"] {
        let response = a
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["pragma"], "no-cache");
    }
}

#[tokio::test]
async fn memory_concurrent_grace_requests() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "concurrent-grace").await;
}

#[tokio::test]
async fn memory_presentation_while_pending_after_grace_revokes_family() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "past-grace").await;
}

#[tokio::test]
async fn memory_dropped_reservation_allows_retry() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "cancel").await;
}

#[tokio::test]
async fn memory_revocation_during_pending_prevents_finalization() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "revoke").await;
}

#[tokio::test]
async fn memory_child_rotation_during_parent_grace_reissue() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "child-rotation").await;
}

#[tokio::test]
async fn memory_issuance_failure_preserves_token() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "issuance-failure").await;
}

#[tokio::test]
async fn postgres_concurrent_grace_requests() {
    check_postgres_reservation("concurrent-grace").await;
}

#[tokio::test]
async fn postgres_presentation_while_pending_after_grace_revokes_family() {
    check_postgres_reservation("past-grace").await;
}

#[tokio::test]
async fn postgres_dropped_reservation_allows_retry() {
    check_postgres_reservation("cancel").await;
}

#[tokio::test]
async fn postgres_revocation_during_pending_prevents_finalization() {
    check_postgres_reservation("revoke").await;
}

#[tokio::test]
async fn postgres_child_rotation_during_parent_grace_reissue() {
    check_postgres_reservation("child-rotation").await;
}

#[tokio::test]
async fn postgres_expired_reservation_allows_retry() {
    check_postgres_reservation("expired").await;
}

#[tokio::test]
async fn postgres_expired_reservation_prevents_original_finalization() {
    check_postgres_reservation("expiry-during-issuance").await;
}

#[tokio::test]
async fn postgres_finalization_failure_allows_retry() {
    check_postgres_reservation("finalize-failure").await;
}

#[tokio::test]
async fn postgres_small_pool_allows_refreshes_and_bounded_other_requests() {
    check_postgres_reservation("pool").await;
}

#[tokio::test]
async fn postgres_slow_rotation_still_returns_new_tokens() {
    check_postgres_reservation("slow").await;
}

#[tokio::test]
async fn memory_retry_after_in_flight_rotation_obeys_grace() {
    check_issuance_without_store_connection(&MemoryExec::new(), TENANT, "retry").await;
}
#[tokio::test]
async fn postgres_retry_after_in_flight_rotation_obeys_grace() {
    check_postgres_reservation("retry").await;
}

#[tokio::test]
async fn postgres_http_pool_timeout_allows_refresh_retry() {
    check_postgres_reservation("http-pool-timeout").await;
}

#[tokio::test]
async fn issuance_timeout_returns_504_and_allows_retry() {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = down.clone();
    let a = app_wrapped(manifest(0), move |inner| {
        Arc::new(FamilyLookupFails {
            inner,
            down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            lookup_delay_ms: 0,
            rotate_delay_ms: 0,
            signer_delay_ms: 0,
            signer_timeout: true,
            reservation_overrun: false,
            signer_down: flag,
        })
    });
    let token = tok(&a.login().await, "refresh_token");
    down.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, body, _) = a.refresh(&token).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(body["error"]["code"], "TX_TIMEOUT");
    assert_eq!(body["error"]["retryable"], true);
    down.store(false, std::sync::atomic::Ordering::SeqCst);
    let (status, body, _) = a.refresh(&token).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        a.refresh(&tok(&body, "refresh_token")).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn memory_child_rotation_after_failed_or_cancelled_issuance() {
    for scenario in ["child-rotation-failure", "child-rotation-cancel"] {
        check_issuance_without_store_connection(&MemoryExec::new(), TENANT, scenario).await;
    }
}
#[tokio::test]
async fn postgres_child_rotation_after_failed_or_cancelled_issuance() {
    for scenario in ["child-rotation-failure", "child-rotation-cancel"] {
        check_postgres_reservation(scenario).await;
    }
}

#[tokio::test]
async fn memory_http_reservation_lifetime_allows_same_token_retry() {
    let overrun = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = overrun.clone();
    let a = app_wrapped(manifest(10), move |inner| {
        Arc::new(FamilyLookupFails {
            inner,
            down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            lookup_delay_ms: 0,
            rotate_delay_ms: 0,
            signer_delay_ms: 0,
            signer_timeout: false,
            reservation_overrun: true,
            signer_down: flag,
        })
    });
    let token = tok(&a.login().await, "refresh_token");
    overrun.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_http_refresh_timeout(&a, &token).await;
    assert_eq!(a.refresh(&token).await.0, StatusCode::OK);
}

async fn assert_http_refresh_timeout(a: &App, token: &str) {
    let response = a
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/v1/acme/token/refresh")
                .header("content-type", "application/json")
                .body(Body::from(json!({"refresh_token": token}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("set-cookie"));
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "TX_TIMEOUT");
    assert_eq!(body["error"]["retryable"], true);
}

#[tokio::test]
async fn postgres_http_expired_reservation_allows_same_token_retry() {
    check_postgres_reservation("http-reservation-expiry").await;
}

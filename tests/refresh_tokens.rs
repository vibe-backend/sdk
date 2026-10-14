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
                step("sign-jwt", "tok", json!({"opts": {"alg": "ES256", "key-ref": "JWT_SIGNING_KEY",
                    "claims": "{ sub: input.user_id }", "ttl": "15m", "issue-refresh": "platform-decided"}})),
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
}

fn app(grace: i64) -> App {
    app_with(manifest(grace))
}

fn app_with(m: Manifest) -> App {
    let exec = Arc::new(MemoryExec::new());
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
    let app = router(Arc::new(AppState {
        manifest: m.clone(),
        exec: exec.clone(),
        auth: auth(&m),
        slugs,
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        trusted_proxies: Vec::new(),
        disabled: std::collections::HashSet::new(),
        min_override_ms: None,
        honor_test_ip: true,
        dev_hints: false,
    }));
    App {
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
            // Same 401 as an unknown token: no 429 that reveals the token exists.
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
async fn revoked_tokens_do_not_count_toward_the_family_limit() {
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

/// The PostgreSQL store (row lock, grace, family revoke) against a real
/// database. Fails in CI when VIBE_TEST_PG_ADMIN_URL is missing.
#[tokio::test]
async fn postgres_store_rotates_detects_reuse_and_revokes() {
    use vibe::runtime::refresh::{RefreshPolicy, RevokeTarget, Rotated};
    use vibe::runtime::{PgExec, QueryExec};
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "VIBE_TEST_PG_ADMIN_URL must be set in CI"
        );
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
    admin
        .batch_execute(&format!(
            "GRANT USAGE ON SCHEMA {schema} TO {role}; \
             GRANT SELECT, INSERT, UPDATE ON {schema}._platform_refresh_tokens TO {role};"
        ))
        .await
        .unwrap();
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
    // Within grace: one reissue, which kills the lost child h1.
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
    admin
        .batch_execute(&format!(
            "GRANT USAGE ON SCHEMA {schema2} TO {role}; \
             GRANT SELECT, INSERT, UPDATE ON {schema2}._platform_refresh_tokens TO {role};"
        ))
        .await
        .unwrap();
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
        Rotated::Reuse { family_id: fam2 }
    );
    assert_eq!(
        exec.refresh_rotate(&tenant, "g2", "gy", policy)
            .await
            .unwrap(),
        Rotated::Invalid
    );
    // Revoked tokens are not counted by the family limiter.
    assert_eq!(exec.refresh_family(&tenant, "g2").await.unwrap(), None);

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
}

#[test]
fn app_sql_cannot_reach_the_refresh_table() {
    for sql in [
        "SELECT token_hash FROM _platform_refresh_tokens",
        "SELECT token_hash FROM \"_platform_refresh_tokens\"",
        "SELECT token_hash FROM t_tenant0001._platform_refresh_tokens",
        "UPDATE _platform_refresh_tokens SET revoked_at = NULL WHERE id = $1",
        "SELECT id FROM users WHERE id IN (SELECT user_id FROM _platform_refresh_tokens)",
    ] {
        assert!(vibe::sqlguard::query_problem(sql).is_some(), "{sql}");
    }
}

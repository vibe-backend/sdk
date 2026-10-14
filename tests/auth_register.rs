//! Registration, email verification and admin invites (#26 part 3; spec
//! §12.11, decisions 56/66/67/69) on the tests/fixtures/auth-app project.
//! PostgreSQL only (the flows are `query` steps); fails in CI without
//! VIBE_TEST_PG_ADMIN_URL.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use vibe::manifest::Manifest;
use vibe::runtime::refresh::{RefreshPolicy, Rotated};
use vibe::runtime::{router, AppState, AuthConfig, PgExec, QueryExec, Slug};

const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

const PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE0mqNqll14aIJEhYqCiRLWJX63Ty6\n\
Q0UxBzbmJ2wI9FMVJLw4XEhuIJFqIdA/P6b6ozSNGyOUZK7OjIfGKtpNYw==\n\
-----END PUBLIC KEY-----\n";

const MESSAGE: &str = "If the details are correct, you will receive a confirmation email";
const STRONG: &str = "violet staple orbit 42";

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

struct Pg {
    app: axum::Router,
    admin: tokio_postgres::Client,
    exec: Arc<PgExec>,
    tenant: String,
    schema: String,
}

async fn pg() -> Option<Pg> {
    let Ok(admin_url) = std::env::var("VIBE_TEST_PG_ADMIN_URL") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "VIBE_TEST_PG_ADMIN_URL must be set in CI; Postgres tests cannot be skipped"
        );
        return None;
    };
    let (admin, conn) = tokio_postgres::connect(&admin_url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    let s = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let tenant = format!("tg{s}");
    let schema = format!("t_{tenant}");
    let app_role = format!("reg_app_{s}");
    admin
        .batch_execute(&format!("CREATE ROLE {app_role} LOGIN PASSWORD 'pw'"))
        .await
        .unwrap();
    let schema_sql = include_str!("fixtures/auth-app/schema.sql")
        .replace("{schema}", &schema)
        .replace("{app}", &app_role);
    admin.batch_execute(&schema_sql).await.unwrap();
    for stmt in vibe::runtime::refresh::table_statements(&schema).unwrap() {
        admin.batch_execute(&stmt).await.unwrap();
    }
    admin
        .batch_execute(&format!(
            "GRANT SELECT, INSERT, UPDATE ON {schema}._platform_refresh_tokens TO {app_role}"
        ))
        .await
        .unwrap();
    let mut url = url::Url::parse(&admin_url).unwrap();
    url.set_username(&app_role).unwrap();
    url.set_password(Some("pw")).unwrap();
    let exec = Arc::new(PgExec::connect(url.as_str()).await.unwrap());
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
        exec: exec.clone(),
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
    Some(Pg {
        app,
        admin,
        exec,
        tenant,
        schema,
    })
}

fn admin_token(tenant: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some("local".into());
    jsonwebtoken::encode(
        &header,
        &json!({"iss": "https://auth.localhost/shop", "aud": "shop", "sub": "admin1",
                "tenant_id": tenant, "roles": ["admin"], "iat": now, "nbf": now, "exp": now + 600}),
        &jsonwebtoken::EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap(),
    )
    .unwrap()
}

impl Pg {
    async fn post(
        &self,
        path: &str,
        body: Value,
        bearer: Option<&str>,
    ) -> (StatusCode, Value, u128) {
        let started = std::time::Instant::now();
        let mut req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("x-challenge-proof", "ok");
        if let Some(t) = bearer {
            req = req.header("authorization", format!("Bearer {t}"));
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
        if let Some(o) = v.as_object_mut() {
            o.remove("request_id");
        }
        if let Some(e) = v.get_mut("error").and_then(Value::as_object_mut) {
            e.remove("request_id");
        }
        (status, v, started.elapsed().as_millis())
    }

    async fn register(&self, email: &str) -> (StatusCode, Value, u128) {
        self.post(
            "/auth/v1/acme/register",
            json!({"email": email, "name": "Ada"}),
            None,
        )
        .await
    }

    async fn verify(&self, token: &str, password: &str) -> (StatusCode, Value) {
        let (s, v, _) = self
            .post(
                "/auth/v1/acme/verify-email",
                json!({"token": token, "password": password}),
                None,
            )
            .await;
        (s, v)
    }

    /// Outbox rows for `email`, oldest first: (template, token, user_id set).
    async fn outbox(&self, email: &str) -> Vec<(String, Option<String>, bool)> {
        self.admin
            .query(
                &format!(
                    "SELECT template, convert_from(token, 'UTF8'), user_id IS NOT NULL \
                     FROM {}.email_outbox WHERE email = $1 ORDER BY created_at, ctid",
                    self.schema
                ),
                &[&email],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2)))
            .collect()
    }

    async fn user(&self, email: &str) -> tokio_postgres::Row {
        self.admin
            .query_one(
                &format!(
                    "SELECT id::text, password IS NOT NULL, email_verified_at IS NOT NULL, \
                     email_verify_hash IS NOT NULL, email_verify_purpose, name \
                     FROM {}.users WHERE email = $1",
                    self.schema
                ),
                &[&email],
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn register_is_uniform_and_verification_sets_password_once() {
    let Some(pg) = pg().await else { return };
    let email = "ada@example.com";
    // New email.
    let (s, first, ms) = pg.register(email).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first, json!({"message": MESSAGE}));
    assert!(ms >= 300, "fixed floor: {ms}ms");
    let u = pg.user(email).await;
    assert!(
        !u.get::<_, bool>(1) && !u.get::<_, bool>(2),
        "no password, unverified"
    );
    // Same email again, still unverified: token rotates; nothing else changes.
    let (s, again, _) = pg.register(email).await;
    assert_eq!((s, &again), (StatusCode::ACCEPTED, &first));
    let mail = pg.outbox(email).await;
    assert_eq!(mail.len(), 2);
    assert!(mail
        .iter()
        .all(|(t, tok, uid)| t == "verify-email" && tok.is_some() && *uid));
    let old = mail[0].1.clone().unwrap();
    let new = mail[1].1.clone().unwrap();
    assert_eq!(new.len(), 43);
    assert_ne!(old, new);
    // The rotated-out token is dead; a weak password fails before the token
    // is looked at; the current token + a strong password verifies.
    assert_eq!(
        pg.verify(&old, STRONG).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (s, weak) = pg.verify(&new, "password1234").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(weak.to_string().contains("WEAK_PASSWORD"), "{weak}");
    let (s, body) = pg.verify(&new, STRONG).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{body}");
    let u = pg.user(email).await;
    assert!(
        u.get::<_, bool>(1) && u.get::<_, bool>(2),
        "password and verified together"
    );
    assert!(!u.get::<_, bool>(3), "token cleared");
    let (s, reused) = pg.verify(&new, STRONG).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(reused.to_string().contains("INVALID_TOKEN"), "{reused}");
    // Verified email: same 202, a neutral notice without a token, no change.
    let (s, third, _) = pg.register(email).await;
    assert_eq!((s, &third), (StatusCode::ACCEPTED, &first));
    let mail = pg.outbox(email).await;
    assert_eq!(
        mail.last().unwrap(),
        &("already-registered".to_string(), None, false)
    );
    // Registration never accepts a password (or other undeclared fields).
    let (s, _, _) = pg
        .post(
            "/auth/v1/acme/register",
            json!({"email": "eve@example.com", "name": "Eve", "password": STRONG}),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn verification_revokes_existing_sessions() {
    let Some(pg) = pg().await else { return };
    let email = "bob@example.com";
    pg.register(email).await;
    let user_id: String = pg.user(email).await.get(0);
    // A refresh family for this user (e.g. created before an email change).
    let policy = RefreshPolicy::default();
    pg.exec
        .refresh_issue(&pg.tenant, &user_id, "pre-verify", policy)
        .await
        .unwrap();
    let token = pg.outbox(email).await[0].1.clone().unwrap();
    assert_eq!(pg.verify(&token, STRONG).await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        pg.exec
            .refresh_rotate(&pg.tenant, "pre-verify", "x", policy)
            .await
            .unwrap(),
        Rotated::Invalid
    );
    let reason: String = pg
        .admin
        .query_one(
            &format!(
                "SELECT revoke_reason FROM {}._platform_refresh_tokens WHERE token_hash = 'pre-verify'",
                pg.schema
            ),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(reason, "email-verified");
}

#[tokio::test]
async fn admin_invite_uses_the_same_verification_and_blocks_hijack() {
    let Some(pg) = pg().await else { return };
    let email = "cy@example.com";
    let admin = admin_token(&pg.tenant);
    let (s, body, _) = pg
        .post(
            "/admin/v1/users/invite",
            json!({"email": email, "name": "Cy"}),
            Some(&admin),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let u = pg.user(email).await;
    assert!(
        !u.get::<_, bool>(1) && !u.get::<_, bool>(2),
        "invited: no password, unverified"
    );
    assert_eq!(u.get::<_, Option<String>>(4).as_deref(), Some("invite"));
    let mail = pg.outbox(email).await;
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].0, "invite");
    let invite = mail[0].1.clone().unwrap();
    // Someone registers the invited email: invite untouched, neutral notice.
    let (s, reg, _) = pg.register(email).await;
    assert_eq!(
        (s, reg),
        (StatusCode::ACCEPTED, json!({"message": MESSAGE}))
    );
    let mail = pg.outbox(email).await;
    assert_eq!(
        mail.last().unwrap(),
        &("already-registered".to_string(), None, false)
    );
    assert_eq!(
        pg.user(email).await.get::<_, Option<String>>(4).as_deref(),
        Some("invite")
    );
    // The invitee accepts through the same verify-email route.
    assert_eq!(pg.verify(&invite, STRONG).await.0, StatusCode::NO_CONTENT);
    let u = pg.user(email).await;
    assert!(u.get::<_, bool>(1) && u.get::<_, bool>(2));
    // Inviting a verified user is refused.
    let (s, _, _) = pg
        .post(
            "/admin/v1/users/invite",
            json!({"email": email, "name": "Cy"}),
            Some(&admin),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Non-admins cannot invite.
    let (s, _, _) = pg
        .post(
            "/admin/v1/users/invite",
            json!({"email": "d@example.com", "name": "D"}),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[test]
fn credential_columns_need_their_capability() {
    use vibe::validate::{credential_write_problem, written_columns};
    let verify = "update users set password = $1, email_verified_at = now(), email_verify_hash = null where email_verify_hash = $2 and email_verified_at is null returning id";
    assert_eq!(
        written_columns(verify),
        vec!["password", "email_verified_at", "email_verify_hash"]
    );
    let caps = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(
        credential_write_problem(verify, &caps(&["set-password", "confirm-verification"]))
            .is_none()
    );
    assert!(
        credential_write_problem(verify, &caps(&["confirm-verification"]))
            .unwrap()
            .contains("set-password")
    );
    assert!(credential_write_problem(verify, &caps(&["set-password"]))
        .unwrap()
        .contains("confirm-verification"));
    // Reading email_verified_at in WHERE is not a write.
    let register = "insert into users (email, name, email_verify_hash) values ($1, $2, $3) on conflict (tenant_id, email) do update set email_verify_hash = excluded.email_verify_hash where users.email_verified_at is null returning id";
    assert_eq!(
        written_columns(register),
        vec!["email", "name", "email_verify_hash"]
    );
    assert!(credential_write_problem(register, &[]).is_none());
}

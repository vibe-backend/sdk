//! Runtime enforcement of challenges on `when: after-failures` and
//! authenticated routes (decision 133, #201), through the HTTP router with a
//! real `TurnstileVerifier` against a mock siteverify.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{extract::State, routing::post, Form, Json, Router};
use serde_json::{json, Value};
use tower::ServiceExt;
use vibe::manifest::Manifest;
use vibe::runtime::challenge::{
    ChallengeContext, ChallengeError, ChallengeVerifier, ReplayCache, TurnstileConfig,
    TurnstileVerifier,
};
use vibe::runtime::{router, AppState, AuthConfig, MemoryExec, Slug};

type Answers = Arc<Mutex<Vec<(String, Value)>>>;

async fn siteverify(
    State(answers): State<Answers>,
    Form(form): Form<Vec<(String, String)>>,
) -> Json<Value> {
    let token = form
        .iter()
        .find(|(k, _)| k == "response")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let found = answers
        .lock()
        .unwrap()
        .iter()
        .find(|(t, _)| *t == token)
        .map(|(_, v)| v.clone());
    Json(
        found.unwrap_or_else(
            || json!({"success": false, "error-codes": ["invalid-input-response"]}),
        ),
    )
}

async fn mock(tokens: &[&str], action: &str) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    let answers: Answers = Arc::new(Mutex::new(
        tokens
            .iter()
            .map(|t| {
                (
                    t.to_string(),
                    json!({"success": true, "hostname": "app.example.com", "action": action, "challenge_ts": now, "error-codes": []}),
                )
            })
            .collect(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/siteverify", post(siteverify))
        .with_state(answers);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/siteverify")
}

fn turnstile(url: String) -> Arc<dyn ChallengeVerifier> {
    Arc::new(
        TurnstileVerifier::new(
            TurnstileConfig {
                secret: "secret-key".into(),
                hostnames: vec!["app.example.com".into()],
                siteverify_url: url,
            },
            reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        )
        .with_replay_cache(ReplayCache::new(Duration::from_secs(360), 1000)),
    )
}

/// Answers `Unavailable` for every token (provider down).
struct Down;
#[async_trait::async_trait]
impl ChallengeVerifier for Down {
    fn provider(&self) -> &'static str {
        "turnstile"
    }
    async fn verify(&self, _: &str, _: &ChallengeContext) -> Result<(), ChallengeError> {
        Err(ChallengeError::Unavailable("provider down".into()))
    }
}

fn step(op: &str, extra: Value) -> Value {
    let mut v = json!({"id": "s", "op": op, "target": "", "args": "", "value": "", "status": 0,
        "sql": "", "params": [], "out": "", "fail": "", "when_expr": "", "writes": false, "table": ""});
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v
}

/// One public login route answering 401 unless `ok` is true.
fn manifest(when: &str, extra: Value) -> Manifest {
    let mut route = json!({
        "method": "POST", "path": "/auth/v1/{slug}/login", "feature_id": "auth/v1/login",
        "public": true, "auth_removed": true, "auth_mode": "jwt", "roles": [], "scopes": [],
        "rates": [], "body_limit": 4096,
        "request": [{"location": "body", "name": "ok", "ty": "boolean", "required": true, "min": null, "max": null, "format": ""}],
        "transform_in": "{ok: req.body.ok}", "transform_out": "input",
        "steps": [
            step("validate", json!({"fail": "BAD", "when_expr": "`input.ok != true`"})),
            step("respond", json!({"value": "transform.out", "status": 200})),
        ],
        "errors": [{"status": 401, "code": "INVALID_CREDENTIALS", "from": "BAD"}],
        "min_response_ms": 0, "public_writes": false, "challenge_type": "captcha",
        "challenge_when": when, "deprecated": false, "sunset": ""
    });
    for (k, val) in extra.as_object().unwrap() {
        route[k] = val.clone();
    }
    serde_json::from_value(json!({
        "manifest_version": 1, "signature": "", "migrations": [], "models": [], "methods": [],
        "routes": [route]
    }))
    .unwrap()
}

fn app(m: Manifest, verifier: Option<Arc<dyn ChallengeVerifier>>) -> Router {
    app_with(m, verifier, AuthConfig::anonymous())
}

fn app_with(m: Manifest, verifier: Option<Arc<dyn ChallengeVerifier>>, auth: AuthConfig) -> Router {
    let mut slugs = HashMap::new();
    slugs.insert(
        "shopslug1".into(),
        Slug {
            tenant_id: "tenant0001".into(),
            public_access: true,
            public_write: false,
        },
    );
    router(Arc::new(AppState {
        manifest: m,
        exec: Arc::new(MemoryExec::new()),
        auth,
        slugs,
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        disabled: std::collections::HashSet::new(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        trusted_proxies: Vec::new(),
        dev_hints: false,
        challenge: verifier,
        failures: Default::default(),
    }))
}

async fn login(app: &Router, ip: &str, ok: bool, proof: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/auth/v1/shopslug1/login")
        .header("content-type", "application/json")
        .header("x-vibe-test-ip", ip);
    if let Some(p) = proof {
        req = req.header("x-challenge-proof", p);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(json!({"ok": ok}).to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn code(v: &Value) -> &str {
    v["error"]["code"]
        .as_str()
        .or(v["code"].as_str())
        .unwrap_or("")
}

#[tokio::test]
async fn after_failures_counts_failures_then_requires_a_verified_token() {
    let url = mock(&["good1", "good2"], "login").await;
    let app = app(
        manifest("after-failures", json!({"challenge_after_failures": 3})),
        Some(turnstile(url)),
    );
    // Below the threshold no proof is needed; a success does not reset.
    assert_eq!(
        login(&app, "203.0.113.7", false, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "203.0.113.7", true, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        login(&app, "203.0.113.7", false, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "203.0.113.7", false, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Third failure reached: a proof is now required.
    let (s, body) = login(&app, "203.0.113.7", true, None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(code(&body), "CHALLENGE_REQUIRED", "{body}");
    // Another client is not affected.
    assert_eq!(
        login(&app, "198.51.100.4", true, None).await.0,
        StatusCode::OK
    );
    // Forged token: 403.
    let (s, body) = login(&app, "203.0.113.7", true, Some("forged")).await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::FORBIDDEN, "CHALLENGE_FAILED"),
        "{body}"
    );
    // Verified token: accepted.
    assert_eq!(
        login(&app, "203.0.113.7", true, Some("good1")).await.0,
        StatusCode::OK
    );
    // Replay of the same token: 403.
    let (s, body) = login(&app, "203.0.113.7", true, Some("good1")).await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::FORBIDDEN, "CHALLENGE_FAILED"),
        "{body}"
    );
    // A verified token does not reset the count either.
    assert_eq!(
        login(&app, "203.0.113.7", true, None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        login(&app, "203.0.113.7", true, Some("good2")).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn after_failures_fails_closed_when_the_verifier_is_down_or_missing() {
    for verifier in [Some(Arc::new(Down) as Arc<dyn ChallengeVerifier>), None] {
        let missing = verifier.is_none();
        let app = app(
            manifest("after-failures", json!({"challenge_after_failures": 1})),
            verifier,
        );
        assert_eq!(
            login(&app, "203.0.113.8", false, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (s, body) = login(&app, "203.0.113.8", true, Some("any")).await;
        assert_eq!(
            (s, code(&body)),
            (StatusCode::SERVICE_UNAVAILABLE, "CHALLENGE_UNAVAILABLE"),
            "missing={missing} {body}"
        );
    }
}

#[tokio::test]
async fn unknown_when_value_challenges_every_request() {
    let app = app(manifest("sometimes", json!({})), None);
    assert_eq!(
        login(&app, "203.0.113.9", true, None).await.0,
        StatusCode::BAD_REQUEST
    );
}

const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgxoRRfBXmTDDBak37\n\
e7KiA/uL9uVmC6SjFqNA0iA3/eChRANCAATSao2qWXXhogkSFioKJEtYlfrdPLpD\n\
RTEHNuYnbAj0UxUkvDhcSG4gkWoh0D8/pvqjNI0bI5Rkrs6Mh8Yq2k1j\n\
-----END PRIVATE KEY-----\n";

const PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE0mqNqll14aIJEhYqCiRLWJX63Ty6\n\
Q0UxBzbmJ2wI9FMVJLw4XEhuIJFqIdA/P6b6ozSNGyOUZK7OjIfGKtpNYw==\n\
-----END PUBLIC KEY-----\n";

fn bearer() -> String {
    let now = chrono::Utc::now().timestamp();
    let claims = json!({
        "sub": "u1", "exp": now + 600, "iat": now, "nbf": now,
        "iss": "https://auth.localhost/shop", "aud": "shop", "tenant_id": "tenant0001"
    });
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some("local".into());
    let t = jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_ec_pem(PRIVATE_PEM.as_bytes()).unwrap(),
    )
    .unwrap();
    format!("Bearer {t}")
}

#[tokio::test]
async fn authenticated_opt_in_route_is_challenged_after_authentication() {
    let url = mock(&["tok-a"], "get").await;
    let m: Manifest = serde_json::from_value(json!({
        "manifest_version": 1, "signature": "", "migrations": [], "models": [], "methods": [],
        "routes": [{
            "method": "GET", "path": "/v1/me", "feature_id": "me/v1/get",
            "public": false, "auth_removed": false, "auth_mode": "jwt", "roles": [], "scopes": [],
            "rates": [], "body_limit": 4096, "request": [], "transform_in": "{}", "transform_out": "result",
            "steps": [step("respond", json!({"status": 204}))],
            "errors": [], "min_response_ms": 0, "public_writes": false,
            "challenge_type": "captcha", "challenge_when": "always", "deprecated": false, "sunset": ""
        }]
    }))
    .unwrap();
    let mut auth = AuthConfig::anonymous();
    auth.public_pem = PUBLIC_PEM.into();
    let app = app_with(m, Some(turnstile(url)), auth);
    let call = |auth: Option<String>, proof: Option<&'static str>| {
        let app = app.clone();
        async move {
            let mut req = Request::builder()
                .uri("/v1/me")
                .header("x-vibe-test-ip", "203.0.113.10");
            if let Some(a) = auth {
                req = req.header("authorization", a);
            }
            if let Some(p) = proof {
                req = req.header("x-challenge-proof", p);
            }
            app.oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };
    // Authentication runs first: no token is 401 even with a valid proof.
    assert_eq!(call(None, Some("tok-a")).await, StatusCode::UNAUTHORIZED);
    assert_eq!(call(Some(bearer()), None).await, StatusCode::BAD_REQUEST);
    assert_eq!(
        call(Some(bearer()), Some("nope")).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(Some(bearer()), Some("tok-a")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(Some(bearer()), Some("tok-a")).await,
        StatusCode::FORBIDDEN
    );
}

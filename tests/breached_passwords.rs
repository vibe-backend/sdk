//! Breached-password check on `hash-password` (#81, spec §4A.2).

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use serde_json::json;
use sha1::{Digest, Sha1};
use vibe::manifest::Manifest;
use vibe::runtime::breached::BreachedSource;
use vibe::runtime::{run_method, AuthConfig, CallCtx, Caller, MemoryExec};

fn manifest() -> Manifest {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/auth-app/manifest.json"
    );
    let mut m: Manifest = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    m.routes.clear();
    m.methods = vec![serde_json::from_value(json!({
        "target": "t.run", "writes": false, "input": [], "errors": [],
        "capabilities": [], "crud_model": "", "crud_action": "", "steps": [
            {"id": "s1", "op": "hash-password", "target": "", "args": "", "value": "", "status": 0,
             "sql": "", "params": [], "out": "pw", "fail": "", "when_expr": "", "writes": false,
             "table": "", "opts": {"plain": "input.pw"}},
            {"id": "s2", "op": "respond", "target": "", "args": "", "value": "{ok: input.pw}",
             "status": 200, "sql": "", "params": [], "out": "", "fail": "", "when_expr": "",
             "writes": false, "table": ""}
        ]
    }))
    .unwrap()];
    m
}

async fn hash(source: BreachedSource, pw: &str) -> Result<(), String> {
    let mut auth = AuthConfig::anonymous();
    auth.breached = Arc::new(source);
    let ctx = CallCtx {
        auth: &auth,
        tenant_id: "tenant0001",
        role: "public_writer",
        caller: Caller {
            subject: "",
            roles: &[],
        },
    };
    run_method(
        &manifest(),
        &MemoryExec::new(),
        "t.run",
        &json!({"pw": pw}),
        ctx,
        0,
    )
    .await
    .map(|_| ())
    .map_err(|f| f.code)
}

fn sha1_hex(s: &str) -> String {
    hex::encode_upper(Sha1::digest(s.as_bytes()))
}

#[tokio::test]
async fn builtin_list_rejects_known_breached_passwords() {
    assert_eq!(
        hash(BreachedSource::builtin(), "password1234").await,
        Err("WEAK_PASSWORD".into())
    );
    assert_eq!(
        hash(BreachedSource::builtin(), "violet staple orbit 42").await,
        Ok(())
    );
    assert_eq!(
        hash(BreachedSource::builtin(), "short").await,
        Err("WEAK_PASSWORD".into()),
        "length floor still applies"
    );
}

#[tokio::test]
async fn offline_list_matches_sha1_lines() {
    let list = format!(
        "# leaked\n{}:12\n\n{}\n",
        sha1_hex("hunter2hunter2"),
        sha1_hex("letmein-letmein").to_lowercase()
    );
    let source = || BreachedSource::offline(&list).unwrap();
    assert_eq!(
        hash(source(), "hunter2hunter2").await,
        Err("WEAK_PASSWORD".into())
    );
    assert_eq!(
        hash(source(), "letmein-letmein").await,
        Err("WEAK_PASSWORD".into())
    );
    assert_eq!(hash(source(), "violet staple orbit 42").await, Ok(()));
    assert!(BreachedSource::offline("not-a-hash").is_err());
}

/// (prefix, Add-Padding header) of each request.
type Requests = Arc<Mutex<Vec<(String, Option<String>)>>>;

#[derive(Clone, Default)]
struct Seen(Requests);

/// A local range API: answers with the breached suffix (count 3), a padding
/// entry (count 0) for the padded password, and noise.
async fn range_server(status: StatusCode) -> (String, Seen) {
    let seen = Seen::default();
    let breached = sha1_hex("hunter2hunter2");
    let padded = sha1_hex("padding-only-password");
    let app = axum::Router::new()
        .route(
            "/range/:prefix",
            get(
                move |State(seen): State<Seen>, Path(prefix): Path<String>, headers: HeaderMap| {
                    let breached = breached.clone();
                    let padded = padded.clone();
                    async move {
                        seen.0.lock().unwrap().push((
                            prefix.clone(),
                            headers
                                .get("add-padding")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string),
                        ));
                        let mut body = String::from("0000000000000000000000000000000000A:1\r\n");
                        for h in [&breached, &padded] {
                            if h.starts_with(&prefix) {
                                let count = if *h == breached { 3 } else { 0 };
                                body.push_str(&format!("{}:{count}\r\n", &h[5..]));
                            }
                        }
                        (status, body)
                    }
                },
            ),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/range/"), seen)
}

#[tokio::test]
async fn range_api_sends_only_a_five_char_prefix() {
    let (url, seen) = range_server(StatusCode::OK).await;
    let src = || BreachedSource::range(&url).unwrap();
    assert_eq!(
        hash(src(), "hunter2hunter2").await,
        Err("WEAK_PASSWORD".into())
    );
    assert_eq!(
        hash(src(), "padding-only-password").await,
        Ok(()),
        "count 0 is padding"
    );
    assert_eq!(hash(src(), "violet staple orbit 42").await, Ok(()));
    let seen = seen.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].0, &sha1_hex("hunter2hunter2")[..5]);
    for (prefix, padding) in &seen {
        assert_eq!(prefix.len(), 5, "only the prefix leaves the platform");
        assert_eq!(padding.as_deref(), Some("true"));
    }
}

#[tokio::test]
async fn unavailable_range_api_fails_closed() {
    let (url, _) = range_server(StatusCode::INTERNAL_SERVER_ERROR).await;
    assert_eq!(
        hash(
            BreachedSource::range(&url).unwrap(),
            "violet staple orbit 42"
        )
        .await,
        Err("SERVICE_UNAVAILABLE".into())
    );
    // Nothing listening.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}/range/", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        hash(
            BreachedSource::range(&dead).unwrap(),
            "violet staple orbit 42"
        )
        .await,
        Err("SERVICE_UNAVAILABLE".into())
    );
}

#[test]
fn configuration_rules() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    };
    assert!(
        BreachedSource::from_env(&env(&[])).is_ok(),
        "defaults to the range API"
    );
    assert!(BreachedSource::from_env(&env(&[
        ("VIBE_BREACHED_PASSWORDS_FILE", "/x"),
        ("VIBE_BREACHED_PASSWORDS_URL", "https://x/")
    ]))
    .is_err());
    assert!(
        BreachedSource::range("http://passwords.example/range/").is_err(),
        "https only"
    );
    assert!(
        BreachedSource::range("http://127.0.0.1:9/range/").is_ok(),
        "loopback for tests"
    );
}

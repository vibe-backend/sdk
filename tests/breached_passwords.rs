//! Breached-password check on every stored password (#81, spec §4A.2).

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;
use sha1::{Digest, Sha1};
use vibe::manifest::Manifest;
use vibe::runtime::breached::{
    non_public_host, parse_range, BreachedConfig, BreachedSource, OfflineList, RangeApi,
};
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
    // A model with a password field behind generated CRUD.
    let field = |name: &str, ty: &str, system: bool| {
        json!({"name": name, "ty": ty, "required": false,
        "unique": false, "primary": name == "id", "system": system, "sensitive": ty == "password",
        "nullable": true, "min": null, "max": null, "format": ""})
    };
    m.models = vec![serde_json::from_value(json!({
        "id": "account", "provider": "main", "table": "accounts",
        "fields": [field("id", "uuid", true), field("email", "string", false),
                   field("password", "password", false), field("updated_at", "timestamp", true)],
        "crud": ["create", "update", "get"],
        "list": {"page_size": 20, "max_page_size": 100, "sort": [], "filter": []},
        "sensitive_output": []
    }))
    .unwrap()];
    for action in ["create", "update", "get"] {
        m.methods.push(
            serde_json::from_value(json!({
                "target": format!("account.{action}"), "writes": action != "get", "input": [],
                "errors": [], "capabilities": [], "crud_model": "account", "crud_action": action,
                "steps": []
            }))
            .unwrap(),
        );
    }
    m
}

fn auth(config: BreachedConfig) -> AuthConfig {
    let mut auth = AuthConfig::anonymous();
    auth.breached = Arc::new(config);
    auth
}

fn ctx(auth: &AuthConfig) -> CallCtx<'_> {
    CallCtx {
        auth,
        tenant_id: "tenant0001",
        role: "app_rw",
        caller: Caller {
            subject: "admin",
            roles: &[],
        },
    }
}

fn cfg(source: BreachedSource) -> BreachedConfig {
    BreachedConfig {
        source,
        min_count: 1,
    }
}

async fn hash_with(config: BreachedConfig, pw: &str) -> Result<(), String> {
    let auth = auth(config);
    run_method(
        &manifest(),
        &MemoryExec::new(),
        "t.run",
        &json!({"pw": pw}),
        ctx(&auth),
        0,
    )
    .await
    .map(|_| ())
    .map_err(|f| f.code)
}

async fn hash(source: BreachedSource, pw: &str) -> Result<(), String> {
    hash_with(cfg(source), pw).await
}

fn sha1_hex(s: &str) -> String {
    hex::encode_upper(Sha1::digest(s.as_bytes()))
}

const CLEAN: &str = "violet staple orbit 42";

#[tokio::test]
async fn builtin_list_rejects_known_breached_passwords() {
    assert_eq!(
        hash(BreachedSource::builtin(), "password1234").await,
        Err("BREACHED_PASSWORD".into())
    );
    assert_eq!(hash(BreachedSource::builtin(), CLEAN).await, Ok(()));
    assert_eq!(
        hash(BreachedSource::builtin(), "short").await,
        Err("WEAK_PASSWORD".into())
    );
}

#[tokio::test]
async fn offline_list_is_sorted_digests_with_binary_search() {
    let list = format!(
        "# leaked\n{}:12\n\n{}\n{}\n",
        sha1_hex("hunter2hunter2"),
        sha1_hex("letmein-letmein").to_lowercase(),
        sha1_hex("hunter2hunter2")
    );
    let source = || BreachedSource::offline(&list).unwrap();
    assert_eq!(
        hash(source(), "hunter2hunter2").await,
        Err("BREACHED_PASSWORD".into())
    );
    assert_eq!(
        hash(source(), "letmein-letmein").await,
        Err("BREACHED_PASSWORD".into())
    );
    assert_eq!(hash(source(), CLEAN).await, Ok(()));
    assert_eq!(
        OfflineList::parse(list.as_bytes()).unwrap().len(),
        2,
        "deduplicated"
    );
    assert!(BreachedSource::offline("not-a-hash").is_err());
    assert!(
        BreachedSource::offline("# only comments\n").is_err(),
        "empty list refused"
    );
}

/// Generated CRUD stores a `password` field only through the same floor,
/// breached check and hashing as the `hash-password` op (create and update).
#[tokio::test]
async fn crud_password_fields_are_checked_and_hashed() {
    let auth = auth(BreachedConfig::builtin());
    let m = manifest();
    let exec = MemoryExec::new();
    let err = run_method(
        &m,
        &exec,
        "account.create",
        &json!({"email": "a@x", "password": "correcthorsebatterystaple"}),
        ctx(&auth),
        0,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "BREACHED_PASSWORD");
    let made = run_method(
        &m,
        &exec,
        "account.create",
        &json!({"email": "a@x", "password": CLEAN}),
        ctx(&auth),
        0,
    )
    .await
    .unwrap();
    assert!(made.value.get("password").is_none(), "never returned");
    let id = made.value["id"].as_str().unwrap().to_string();
    let etag = made.etag.clone().unwrap();
    let stored = |exec: &MemoryExec| exec.dump("tenant0001", "accounts")[0]["password"].clone();
    let first = stored(&exec);
    assert!(
        first.as_str().unwrap().starts_with("$argon2id$"),
        "hashed, not plaintext: {first}"
    );
    let err = run_method(
        &m,
        &exec,
        "account.update",
        &json!({"id": id, "if_match": etag, "password": "password1234"}),
        ctx(&auth),
        0,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "BREACHED_PASSWORD");
    run_method(
        &m,
        &exec,
        "account.update",
        &json!({"id": id, "if_match": etag, "password": "another clean phrase 7"}),
        ctx(&auth),
        0,
    )
    .await
    .unwrap();
    let second = stored(&exec);
    assert!(
        second.as_str().unwrap().starts_with("$argon2id$"),
        "update hashes too: {second}"
    );
    assert_ne!(first, second);
}

// ---- range API ----

/// (prefix, Add-Padding header) of each request.
type Requests = Arc<Mutex<Vec<(String, Option<String>)>>>;

#[derive(Clone)]
struct Mock {
    seen: Requests,
    mode: &'static str,
    redirect_to: String,
}

/// 150 padding lines plus the breached suffixes (counts 3 and 0).
fn padded_body(prefix: &str) -> String {
    let mut body = String::new();
    for i in 0..150 {
        body.push_str(&format!("{:035X}:0\r\n", i));
    }
    for (pw, count) in [("hunter2hunter2", 3), ("padding-only-password", 0)] {
        let h = sha1_hex(pw);
        if h.starts_with(prefix) {
            body.push_str(&format!("{}:{count}\r\n", h[5..].to_lowercase()));
        }
    }
    body
}

async fn handler(
    State(mock): State<Mock>,
    Path(prefix): Path<String>,
    headers: HeaderMap,
) -> Response {
    assert!(
        headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ua| ua.starts_with("vibe-sdk/")),
        "identifies itself"
    );
    mock.seen.lock().unwrap().push((
        prefix.clone(),
        headers
            .get("add-padding")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    ));
    let plain = [("content-type", "text/plain")];
    match mock.mode {
        "ok" => (StatusCode::OK, plain, padded_body(&prefix)).into_response(),
        "500" => (StatusCode::INTERNAL_SERVER_ERROR, "down").into_response(),
        "empty" => (StatusCode::OK, plain, "").into_response(),
        "html" => (
            StatusCode::OK,
            [("content-type", "text/html")],
            "<html>captive portal</html>",
        )
            .into_response(),
        "malformed" => (
            StatusCode::OK,
            plain,
            format!(
                "{}{}:12abc\r\n",
                padded_body(&prefix),
                &sha1_hex(CLEAN)[5..]
            ),
        )
            .into_response(),
        "short" => (StatusCode::OK, plain, format!("{}:0\r\n", "0".repeat(35))).into_response(),
        "huge" => (StatusCode::OK, plain, "0".repeat(300 * 1024)).into_response(),
        "slow" => {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            (StatusCode::OK, plain, padded_body(&prefix)).into_response()
        }
        "redirect" => (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", format!("{}{prefix}", mock.redirect_to))],
        )
            .into_response(),
        _ => unreachable!(),
    }
}

async fn server(mode: &'static str, redirect_to: &str) -> (String, Requests) {
    let seen = Requests::default();
    let app = axum::Router::new()
        .route("/range/:prefix", get(handler))
        .with_state(Mock {
            seen: seen.clone(),
            mode,
            redirect_to: redirect_to.to_string(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/range/"), seen)
}

#[tokio::test]
async fn range_api_sends_only_a_prefix_and_caches_it() {
    let (url, seen) = server("ok", "").await;
    let api = Arc::new(BreachedSource::range(&url, false).unwrap());
    let check = |pw: &'static str| {
        let api = api.clone();
        async move { api.is_breached(pw, 1).await }
    };
    assert_eq!(check("hunter2hunter2").await, Ok(true));
    assert_eq!(check("hunter2hunter2").await, Ok(true));
    assert_eq!(
        check("padding-only-password").await,
        Ok(false),
        "count 0 is padding"
    );
    assert_eq!(check(CLEAN).await, Ok(false));
    let seen = seen.lock().unwrap().clone();
    let prefixes: Vec<_> = seen.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(prefixes[0], &sha1_hex("hunter2hunter2")[..5]);
    assert_eq!(
        prefixes.iter().filter(|p| **p == prefixes[0]).count(),
        1,
        "second lookup of a prefix comes from the cache"
    );
    for (prefix, padding) in &seen {
        assert_eq!(prefix.len(), 5, "only the prefix leaves the platform");
        assert_eq!(padding.as_deref(), Some("true"));
    }
}

#[tokio::test]
async fn min_count_is_configurable() {
    let (url, _) = server("ok", "").await;
    let strict = BreachedConfig {
        source: BreachedSource::range(&url, false).unwrap(),
        min_count: 5,
    };
    assert_eq!(
        strict.is_breached("hunter2hunter2").await,
        Ok(false),
        "seen 3 times < 5"
    );
    let default = BreachedConfig {
        source: BreachedSource::range(&url, false).unwrap(),
        min_count: 1,
    };
    assert_eq!(default.is_breached("hunter2hunter2").await, Ok(true));
}

#[tokio::test]
async fn odd_responses_fail_closed() {
    for mode in ["500", "empty", "html", "malformed", "short", "huge"] {
        let (url, _) = server(mode, "").await;
        assert_eq!(
            hash(BreachedSource::range(&url, false).unwrap(), CLEAN).await,
            Err("SERVICE_UNAVAILABLE".into()),
            "{mode}"
        );
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}/range/", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        hash(BreachedSource::range(&dead, false).unwrap(), CLEAN).await,
        Err("SERVICE_UNAVAILABLE".into())
    );
    assert!(parse_range(b"").is_err());
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let (target, target_seen) = server("ok", "").await;
    let (url, _) = server("redirect", &target).await;
    assert_eq!(
        hash(
            BreachedSource::range(&url, false).unwrap(),
            "hunter2hunter2"
        )
        .await,
        Err("SERVICE_UNAVAILABLE".into())
    );
    assert!(
        target_seen.lock().unwrap().is_empty(),
        "redirect target never contacted"
    );
}

#[tokio::test]
async fn outage_uses_the_offline_fallback_list_and_counts_failures() {
    let (url, _) = server("500", "").await;
    let list = OfflineList::parse(sha1_hex("hunter2hunter2").as_bytes()).unwrap();
    let api = RangeApi::new(&url, false).unwrap().with_fallback(list);
    let source = BreachedSource::Range(Box::new(api));
    assert_eq!(source.is_breached("hunter2hunter2", 1).await, Ok(true));
    assert_eq!(source.is_breached(CLEAN, 1).await, Ok(false));
    let BreachedSource::Range(api) = &source else {
        unreachable!()
    };
    assert_eq!(api.failures(), 2);
    assert!(source.describe().contains("offline fallback"));
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
    let ok = BreachedConfig::from_env(&env(&[]), true).unwrap();
    assert!(ok.source.describe().contains("api.pwnedpasswords.com"));
    assert_eq!(ok.min_count, 1);
    assert!(BreachedConfig::from_env(
        &env(&[
            ("VIBE_BREACHED_PASSWORDS_FILE", "/x"),
            ("VIBE_BREACHED_PASSWORDS_URL", "https://x.example/")
        ]),
        false
    )
    .is_err());
    assert!(
        BreachedConfig::from_env(&env(&[("VIBE_BREACHED_PASSWORDS_MIN_COUNT", "0")]), false)
            .is_err()
    );
    assert_eq!(
        BreachedConfig::from_env(&env(&[("VIBE_BREACHED_PASSWORDS_MIN_COUNT", "10")]), false)
            .unwrap()
            .min_count,
        10
    );
    // Hosts.
    assert!(
        RangeApi::new("http://passwords.example/range/", false).is_err(),
        "https only"
    );
    assert!(
        RangeApi::new("http://127.0.0.1:9/range/", false).is_ok(),
        "loopback for local tests"
    );
    assert!(
        RangeApi::new("http://127.0.0.1:9/range/", true).is_err(),
        "never in production"
    );
    assert!(RangeApi::new("https://127.0.0.1/range/", true).is_err());
    for bad in [
        "https://169.254.169.254/",
        "https://10.0.0.5/",
        "https://192.168.1.1/",
        "https://[fd00::1]/",
        "https://[::ffff:169.254.169.254]/",
        "https://metadata.internal/",
        "https://user:pw@api.pwnedpasswords.com/range/",
    ] {
        assert!(RangeApi::new(bad, false).is_err(), "{bad}");
    }
    assert!(non_public_host("100.64.0.1"));
    assert!(!non_public_host("api.pwnedpasswords.com"));
    assert!(!non_public_host("8.8.8.8"));
}

#[tokio::test]
async fn timeout_is_configurable() {
    let (url, _) = server("slow", "").await;
    let fast = RangeApi::new(&url, false)
        .unwrap()
        .with_timeout(std::time::Duration::from_millis(200));
    assert!(BreachedSource::Range(Box::new(fast))
        .is_breached(CLEAN, 1)
        .await
        .is_err());
    let patient = RangeApi::new(&url, false).unwrap();
    assert_eq!(
        BreachedSource::Range(Box::new(patient))
            .is_breached(CLEAN, 1)
            .await,
        Ok(false)
    );
}

#[tokio::test]
async fn range_api_names_must_resolve_to_public_addresses() {
    use vibe::runtime::breached::PublicResolver;
    let strict = PublicResolver {
        allow_loopback: false,
    };
    let err = strict.resolve_checked("localhost").await.unwrap_err();
    assert!(err.contains("non-public"), "{err}");
    let local = PublicResolver {
        allow_loopback: true,
    };
    assert!(local.resolve_checked("localhost").await.is_ok());
}

/// Field rules apply to the password as sent, not to its (longer) hash.
#[tokio::test]
async fn crud_create_checks_field_rules_before_hashing() {
    let auth = auth(BreachedConfig::builtin());
    let mut m = manifest();
    let pw = m.models[0]
        .fields
        .iter_mut()
        .find(|f| f.name == "password")
        .unwrap();
    pw.max = Some(64);
    let exec = MemoryExec::new();
    run_method(
        &m,
        &exec,
        "account.create",
        &json!({"email": "a@x", "password": CLEAN}),
        ctx(&auth),
        0,
    )
    .await
    .unwrap();
    let err = run_method(
        &m,
        &exec,
        "account.create",
        &json!({"email": "b@x", "password": "x".repeat(65)}),
        ctx(&auth),
        0,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "VALIDATION_FAILED");
}

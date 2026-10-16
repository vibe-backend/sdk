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
    BreachedConfig { source }
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

/// 150 padding lines plus the breached suffixes (counts 1 and 0).
fn padded_body(prefix: &str) -> String {
    let mut body = String::new();
    for i in 0..150 {
        body.push_str(&format!("{:035X}:0\r\n", i));
    }
    for (pw, count) in [("hunter2hunter2", 1), ("padding-only-password", 0)] {
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
        "marked" => (
            StatusCode::OK,
            plain,
            format!("{}UPSTREAM-MARKER-TEXT\r\n", padded_body(&prefix)),
        )
            .into_response(),
        "marked500" => (StatusCode::BAD_GATEWAY, "UPSTREAM-MARKER-TEXT").into_response(),
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
        .route("/range/{prefix}", get(handler))
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
        async move { api.is_breached(pw).await }
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
async fn outage_fails_closed_and_counts_failures() {
    let (url, _) = server("500", "").await;
    let source = BreachedSource::range(&url, false).unwrap();
    assert!(source.is_breached("hunter2hunter2").await.is_err());
    assert!(source.is_breached(CLEAN).await.is_err());
    let BreachedSource::Range(api) = &source else {
        unreachable!()
    };
    assert_eq!(api.failures(), 2);
    assert!(source.describe().contains("fails closed"));
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
        .is_breached(CLEAN)
        .await
        .is_err());
    let patient = RangeApi::new(&url, false).unwrap();
    assert_eq!(
        BreachedSource::Range(Box::new(patient))
            .is_breached(CLEAN)
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

#[test]
fn lookup_timeout_setting() {
    use std::time::Duration;
    let timeout_of = |value: Option<&'static str>| {
        let get = move |name: &str| match (name, value) {
            ("VIBE_BREACHED_PASSWORDS_TIMEOUT_MS", Some(v)) => Some(v.to_string()),
            _ => None,
        };
        BreachedConfig::from_env(&get, true).map(|config| match config.source {
            BreachedSource::Range(api) => api.timeout(),
            BreachedSource::Offline(_) => unreachable!(),
        })
    };
    assert_eq!(timeout_of(None).unwrap(), Duration::from_millis(2000));
    assert_eq!(timeout_of(Some("100")).unwrap(), Duration::from_millis(100));
    assert_eq!(
        timeout_of(Some(" 1500 ")).unwrap(),
        Duration::from_millis(1500)
    );
    assert_eq!(timeout_of(Some("10000")).unwrap(), Duration::from_secs(10));
    for bad in ["0", "99", "10001", "30000", "-5", "1.5", "3s", "abc"] {
        assert!(timeout_of(Some(bad)).is_err(), "accepted {bad}");
    }
}

/// A list file in a private directory under the cargo target dir (not the
/// shared temp dir, whose sticky mode counts as too open).
fn list_file(name: &str, mode: u32) -> String {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("vibe-breached-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = dir.join("list.txt");
    std::fs::write(&path, sha1_hex("hunter2hunter2")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = mode;
    path.to_string_lossy().into_owned()
}

#[test]
fn removed_variables_stop_startup_in_every_environment() {
    for production in [false, true] {
        for removed in [
            "VIBE_BREACHED_PASSWORDS_FALLBACK_FILE",
            "VIBE_BREACHED_PASSWORDS_FALLBACK_COMPLETE",
        ] {
            for value in ["", " ", "false", "0", "true", "/missing/list", "invalid"] {
                for offline in [false, true] {
                    let get = |name: &str| {
                        if name == removed {
                            Some(value.to_string())
                        } else if offline && name == "VIBE_BREACHED_PASSWORDS_FILE" {
                            Some("/missing/sole-source".to_string())
                        } else {
                            None
                        }
                    };
                    let err = BreachedConfig::from_env(&get, production).err().unwrap();
                    assert!(
                        err.contains(removed) && err.contains("was removed"),
                        "{err}"
                    );
                }
            }
        }
    }
}

#[test]
fn removed_min_count_is_rejected() {
    for value in ["", "1", "10", "invalid"] {
        let get =
            |name: &str| (name == "VIBE_BREACHED_PASSWORDS_MIN_COUNT").then(|| value.to_string());
        let err = BreachedConfig::from_env(&get, false).err().unwrap();
        assert!(err.contains("removed"));
    }
}

#[cfg(unix)]
#[test]
fn list_writable_by_group_or_other_is_refused_in_production() {
    for (name, mode) in [("group", 0o664), ("other", 0o646)] {
        let path = list_file(name, mode);
        let err = OfflineList::load(&path, true).err().unwrap();
        assert!(err.contains("writable by group or other"), "{err}");
        assert!(OfflineList::load(&path, false).is_ok(), "dev keeps loading");
        let get = |name: &str| (name == "VIBE_BREACHED_PASSWORDS_FILE").then(|| path.clone());
        assert!(BreachedConfig::from_env(&get, true).is_err());
        assert!(BreachedConfig::from_env(&get, false).is_ok());
    }
    let path = list_file("owner", 0o644);
    assert!(OfflineList::load(&path, true).is_ok());
}

#[cfg(unix)]
#[test]
fn list_directory_writable_by_group_or_other_is_refused_in_production() {
    use std::os::unix::fs::PermissionsExt;
    for (name, mode) in [
        ("dir-group", 0o770),
        ("dir-other", 0o707),
        ("dir-sticky", 0o1777),
    ] {
        let path = list_file(name, 0o644);
        let dir = std::path::Path::new(&path).parent().unwrap().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let err = OfflineList::load(&path, true).err().unwrap();
        assert!(err.contains("directory"), "{name}: {err}");
        assert!(OfflineList::load(&path, false).is_ok(), "dev keeps loading");
        let get = |n: &str| (n == "VIBE_BREACHED_PASSWORDS_FILE").then(|| path.clone());
        assert!(BreachedConfig::from_env(&get, true).is_err());
        assert!(BreachedConfig::from_env(&get, false).is_ok());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            OfflineList::load(&path, true).is_ok(),
            "{name}: restricted again"
        );
    }
}

#[cfg(unix)]
#[test]
fn parent_directory_chain_is_checked() {
    use std::os::unix::fs::PermissionsExt;
    let path = list_file("chain", 0o644);
    let dir = std::path::Path::new(&path).parent().unwrap();
    let nested = dir.join("a/b");
    std::fs::create_dir_all(&nested).unwrap();
    let inner = nested.join("list.txt");
    std::fs::copy(&path, &inner).unwrap();
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o644)).unwrap();
    for d in [dir.join("a"), nested.clone()] {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let inner = inner.to_string_lossy().into_owned();
    assert!(OfflineList::load(&inner, true).is_ok());
    std::fs::set_permissions(dir.join("a"), std::fs::Permissions::from_mode(0o777)).unwrap();
    let err = OfflineList::load(&inner, true).err().unwrap();
    assert!(err.contains("directory") && err.contains("/a"), "{err}");
    std::fs::set_permissions(dir.join("a"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn lookup_errors_name_only_a_category() {
    for mode in ["marked", "marked500"] {
        let (url, _) = server(mode, "").await;
        let err = BreachedSource::range(&url, false)
            .unwrap()
            .is_breached(CLEAN)
            .await
            .unwrap_err();
        assert!(!err.contains("UPSTREAM-MARKER-TEXT"), "{mode}: {err}");
    }
    let body = format!("{}UPSTREAM-MARKER-TEXT\n", padded_body("00000"));
    let err = parse_range(body.as_bytes()).unwrap_err();
    assert_eq!(
        err,
        vibe::runtime::breached::LookupError::MalformedLine(151)
    );
    assert!(!err.to_string().contains("UPSTREAM"));
}

#[tokio::test]
async fn timeout_is_one_total_deadline_and_fails_closed() {
    let (url, _) = server("slow", "").await;
    let api = RangeApi::new(&url, false)
        .unwrap()
        .with_timeout(std::time::Duration::from_millis(300));
    let source = std::sync::Arc::new(BreachedSource::Range(Box::new(api)));
    // More lookups than slots, distinct prefixes: queued lookups share the
    // same deadline as the request itself.
    let started = std::time::Instant::now();
    let mut tasks = Vec::new();
    for i in 0..20 {
        let source = source.clone();
        tasks.push(tokio::spawn(async move {
            source
                .is_breached(&format!("timeout-check-{i}-password"))
                .await
        }));
    }
    for task in tasks {
        let err = task.await.unwrap().unwrap_err();
        assert!(err.contains("timed out"), "{err}");
    }
    assert!(started.elapsed() < std::time::Duration::from_millis(550));
    let (url, _) = server("slow", "").await;
    let api = RangeApi::new(&url, false)
        .unwrap()
        .with_timeout(std::time::Duration::from_millis(200));
    assert_eq!(
        hash(BreachedSource::Range(Box::new(api)), CLEAN).await,
        Err("SERVICE_UNAVAILABLE".into())
    );
}

#[tokio::test]
async fn failures_feed_the_process_wide_counter() {
    let before = vibe::runtime::breached_lookup_failures();
    let (url, _) = server("500", "").await;
    let source = BreachedSource::range(&url, false).unwrap();
    assert!(source.is_breached(CLEAN).await.is_err());
    assert!(source.is_breached(CLEAN).await.is_err());
    assert!(vibe::runtime::breached_lookup_failures() >= before + 2);
}

#[test]
fn range_parser_rejects_duplicates_and_malformed_lines() {
    use vibe::runtime::breached::LookupError;
    let body = padded_body("00000");
    assert!(
        parse_range(body.as_bytes()).is_ok(),
        "zero-count padding is valid"
    );
    assert!(parse_range(format!("{body}\r\n\n").as_bytes()).is_ok());
    for suffix in [format!("{:035X}:0", 10), format!("{:035x}:1", 10)] {
        assert_eq!(
            parse_range(format!("{body}{suffix}\n").as_bytes()).unwrap_err(),
            LookupError::DuplicateSuffix(151)
        );
    }
    for bad in [
        "",
        " ",
        "ABC:1",
        "GGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG:1",
        "00000000000000000000000000000000000:+1",
        "00000000000000000000000000000000000:1 ",
        "00000000000000000000000000000000000:18446744073709551616",
    ] {
        let input = format!("{bad}\n{body}");
        assert_eq!(
            parse_range(input.as_bytes()).unwrap_err(),
            LookupError::MalformedLine(1)
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_targets_and_link_containers_are_checked() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let path = list_file("symlinks", 0o644);
    let root = std::path::Path::new(&path).parent().unwrap();
    let target_dir = root.join("target");
    let link_dir = root.join("links");
    for dir in [&target_dir, &link_dir] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let target = target_dir.join("list");
    std::fs::copy(&path, &target).unwrap();
    let link = link_dir.join("list");
    symlink(&target, &link).unwrap();
    let directory_link = link_dir.join("directory");
    symlink(&target_dir, &directory_link).unwrap();
    let via_directory = directory_link.join("list");
    let via_directory = via_directory.to_str().unwrap();
    let link = link.to_str().unwrap();
    assert!(OfflineList::load(link, true).is_ok());
    for dir in [&target_dir, &link_dir] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        let err = OfflineList::load(link, true).err().unwrap();
        assert!(err.contains(dir.to_str().unwrap()), "{err}");
        assert!(OfflineList::load(link, false).is_ok());
        assert!(OfflineList::load(via_directory, true).is_err());
        assert!(OfflineList::load(via_directory, false).is_ok());
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

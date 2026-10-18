#![cfg(feature = "sdk")]
use axum::{body::Body, http::Request};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};
use tower::ServiceExt;
use vibe::runtime::{router, AppState, AuthConfig, MemoryExec};
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}
fn build() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-counter-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/counter"),
        &root,
    );
    fs::create_dir_all(root.join(".vibe")).unwrap();
    fs::write(
        root.join(".vibe/slugs.json"),
        "{\"dev\":{\"tenant_id\":\"devtenant\",\"public_access\":true,\"public_write\":true}}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .current_dir(&root)
        .env_clear()
        .arg("build")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("MEMORY_UNSUPPORTED_SQL"));
    root
}
async fn send(app: axum::Router, method: &str, path: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn structured_build_and_fifty_concurrent_http_increments() {
    let root = build();
    let key = vibe::sign::load_dev_key(&root).unwrap();
    let manifest = vibe::manifest::manifest_from_project(&root, &key).unwrap();
    let slugs = vibe::runtime::load_project_slugs(&root, true)
        .unwrap()
        .slugs;
    let app = router(Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth: AuthConfig::anonymous(),
        slugs,
        dev_slugs: None,
        rates: Default::default(),
        quotas: Default::default(),
        disabled: Default::default(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        trusted_proxies: vec![],
        dev_hints: true,
        challenge: None,
    }));
    assert_eq!(
        send(app.clone(), "GET", "/counter/v1/dev/counter").await["value"],
        0
    );
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..50 {
        let app = app.clone();
        tasks.spawn(async move {
            send(app, "POST", "/counter/v1/dev/counter/increment").await["value"]
                .as_i64()
                .unwrap()
        });
    }
    let mut values = Vec::new();
    while let Some(result) = tasks.join_next().await {
        values.push(result.unwrap());
    }
    values.sort_unstable();
    assert_eq!(values, (1..=50).collect::<Vec<_>>());
    assert_eq!(
        send(app, "GET", "/counter/v1/dev/counter").await["value"],
        50
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn structured_build_and_real_run_counter() {
    use std::io::{Read, Write};
    let root = build();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut child = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .current_dir(&root)
        .args(["run", "--db", "memory", "--addr", &addr.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Always reap the child, including when assertions or socket operations fail.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if std::net::TcpStream::connect(addr).is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline && child.try_wait().unwrap().is_none(),
                "server did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        for (method, path, expected) in [
            ("GET", "counter", 0),
            ("POST", "counter/increment", 1),
            ("GET", "counter", 1),
        ] {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            write!(stream,"{method} /counter/v1/dev/{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(
                response.contains(&format!("\"value\":{expected}")),
                "{response}"
            );
        }
    }));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(root).unwrap();
    if let Err(err) = result {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        std::panic::resume_unwind(err);
    }
}

#[test]
fn build_warns_only_on_stderr_for_unsupported_queries() {
    let root = build();
    let source = root.join("models/counter/methods/read.md");
    let text = fs::read_to_string(&source).unwrap();
    fs::write(
        &source,
        text.replace("COALESCE(MAX(value), 0) AS value", "ABS(value)"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .current_dir(&root)
        .arg("build")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("counter.read step s1 will fail on --db memory with MEMORY_UNSUPPORTED_SQL; fine on PostgreSQL"), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("MEMORY_UNSUPPORTED_SQL"));
    fs::remove_dir_all(root).unwrap();
}

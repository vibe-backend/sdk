//! #83: per-file lock writes and transient-error retries.

use std::fs;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vibe::compile::{compile_project, CompileOptions};
use vibe::llm::{FakeAdapter, HttpAdapter, LlmAdapter, LlmConfig, LlmRequest};
use vibe::lock::parse_lock;
use vibe::validate::{validate_project, ValidateOptions};

const GOOD: &str = "## Table\n- provider: main\n- table: products\n## Fields\n| name | type | rules | system |\n| --- | --- | --- | --- |\n| sku | string | required, max: 40 | no |\n## CRUD\n| method | options |\n| --- | --- |\n| get | - |\n## Policies\n(none)\n";

fn config(base_url: &str, max_retries: u32) -> LlmConfig {
    LlmConfig {
        provider: "openai".into(),
        base_url: base_url.into(),
        api_key: "sk-test-value".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout: Duration::from_secs(5),
        max_retries,
        seed: 0,
        endpoint_host: "127.0.0.1".into(),
        endpoint_private: false,
        api_style: "chat".into(),
    }
}

fn project() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-keep-{}", uuid::Uuid::new_v4()));
    for m in ["product", "zbroken"] {
        fs::create_dir_all(root.join(format!("models/{m}"))).unwrap();
        fs::write(
            root.join(format!("models/{m}/{m}.md")),
            format!("# {m}\n\nA {m} has a sku.\n"),
        )
        .unwrap();
    }
    fs::create_dir_all(root.join("providers")).unwrap();
    fs::write(
        root.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    root
}

#[tokio::test]
async fn any_failure_leaves_official_locks_untouched_and_caches_successes() {
    let root = project();
    // An existing official lock must survive a failed run byte for byte.
    fs::write(root.join("providers/main.lock.md"), "old lock\n").unwrap();
    let calls = std::sync::Mutex::new(Vec::<String>::new());
    let broken_ok = std::sync::atomic::AtomicBool::new(false);
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            let id = if request.input.contains("ID: zbroken") {
                "zbroken"
            } else {
                "product"
            };
            calls.lock().unwrap().push(id.into());
            if id == "zbroken" && !broken_ok.load(std::sync::atomic::Ordering::SeqCst) {
                Err("http 502 from upstream".into())
            } else {
                Ok(GOOD.replace("products", &format!("{id}s")))
            }
        },
    };
    let config = config("https://api.openai.com/v1", 0);
    let opts = CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    };
    let report = compile_project(&opts)
        .await
        .err()
        .expect("broken source fails the run");
    let text = report.to_string();
    assert!(text.contains("models/zbroken/zbroken.md"), "{text}");
    assert!(
        text.contains("no locks written; 1 file(s) failed"),
        "{text}"
    );
    assert_eq!(
        fs::read_to_string(root.join("providers/main.lock.md")).unwrap(),
        "old lock\n"
    );
    assert!(!root.join("models/product/product.lock.md").exists());
    let cache = root.join(".vibe/cache/llm");
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 1, "product cached");
    // No temp or backup files left next to the locks.
    for dir in ["providers", "models/product"] {
        for entry in fs::read_dir(root.join(dir)).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.contains(".tmp-") && !name.contains(".bak-"), "{name}");
        }
    }

    // Rerun after the upstream recovers: product comes from the cache.
    calls.lock().unwrap().clear();
    broken_ok.store(true, std::sync::atomic::Ordering::SeqCst);
    let out = compile_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(*calls.lock().unwrap(), vec!["zbroken".to_string()]);
    assert!(out.report.to_string().contains("cached LLM output"));
    for lock in [
        "providers/main.lock.md",
        "models/product/product.lock.md",
        "models/zbroken/zbroken.lock.md",
    ] {
        assert!(
            parse_lock(&fs::read_to_string(root.join(lock)).unwrap()).is_ok(),
            "{lock}"
        );
    }
    // Cached output still goes through the full validator.
    validate_project(&ValidateOptions {
        root: &root,
        deploy: false,
        verify_key: None,
    })
    .unwrap_or_else(|err| panic!("{err}"));
}

#[tokio::test]
async fn corrupt_cache_entry_is_ignored() {
    let root = project();
    fs::remove_dir_all(root.join("models/zbroken")).unwrap();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |_: &LlmRequest| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(GOOD.to_string())
        },
    };
    let config = config("https://api.openai.com/v1", 0);
    let opts = CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    };
    compile_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let cache = root.join(".vibe/cache/llm");
    for entry in fs::read_dir(&cache).unwrap() {
        fs::write(entry.unwrap().path(), "- nested\n  - bullet\n").unwrap();
    }
    compile_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Serves the given status lines (with extra headers) in order, then 200.
async fn server(
    script: Vec<(&'static str, &'static str)>,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).await;
            let resp = match script.get(n) {
                Some((status, headers)) => format!("HTTP/1.1 {status}\r\ncontent-type: text/plain\r\n{headers}content-length: 3\r\nconnection: close\r\n\r\nbad"),
                None => {
                    let body = serde_json::json!({
                        "model": "gpt-test-2026-01-01",
                        "choices": [{"message": {"content": "ok"}}]
                    })
                    .to_string();
                    format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                }
            };
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}/v1"), hits)
}

async fn flaky_server() -> String {
    server(vec![("502 Bad Gateway", "")]).await.0
}

fn request() -> LlmRequest {
    LlmRequest {
        system: "s".into(),
        input: "i".into(),
        model: "gpt-test-2026-01-01".into(),
        seed: 0,
        timeout: Duration::from_secs(5),
    }
}

#[tokio::test]
async fn bad_gateway_is_retried_with_backoff() {
    let adapter = HttpAdapter::new(config(&flaky_server().await, 2)).unwrap();
    let started = std::time::Instant::now();
    let response = adapter.compile(&request()).await.expect("retried");
    assert_eq!(response.text, "ok");
    assert!(started.elapsed() >= Duration::from_millis(900), "backoff");
}

#[tokio::test]
async fn retries_are_bounded() {
    let adapter = HttpAdapter::new(config(&flaky_server().await, 0)).unwrap();
    let Err(err) = adapter.compile(&request()).await else {
        panic!("no retry expected");
    };
    assert!(err.contains("502"), "{err}");
}

#[test]
fn retry_delay_grows_and_is_capped() {
    use vibe::llm::retry_delay;
    assert!(retry_delay(0) >= Duration::from_secs(1));
    assert!(retry_delay(1) >= Duration::from_secs(2));
    assert!(retry_delay(10) <= Duration::from_secs(20));
}

#[tokio::test]
async fn retry_after_is_honored() {
    let (url, hits) = server(vec![("429 Too Many Requests", "retry-after: 0\r\n")]).await;
    let adapter = HttpAdapter::new(config(&url, 2)).unwrap();
    let started = std::time::Instant::now();
    adapter.compile(&request()).await.expect("retried");
    // Retry-After: 0 replaces the 1s backoff.
    assert!(started.elapsed() < Duration::from_millis(900));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn auth_errors_are_not_retried() {
    for status in [
        "401 Unauthorized",
        "403 Forbidden",
        "400 Bad Request",
        "404 Not Found",
    ] {
        let (url, hits) = server(vec![(status, ""), (status, ""), (status, "")]).await;
        let adapter = HttpAdapter::new(config(&url, 3)).unwrap();
        let Err(err) = adapter.compile(&request()).await else {
            panic!("{status} must fail");
        };
        assert!(!err.contains("sk-test-value"), "{err}");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{status}"
        );
    }
}

#[test]
fn retry_after_parsing() {
    use vibe::llm::parse_retry_after;
    assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
    assert_eq!(parse_retry_after("9999"), Some(Duration::from_secs(60)));
    assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
}

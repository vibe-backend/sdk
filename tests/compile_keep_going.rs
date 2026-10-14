//! #83: per-file lock writes and transient-error retries.

use std::fs;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vibe::compile::{compile_project, CompileOptions};
use vibe::llm::{FakeAdapter, HttpAdapter, LlmAdapter, LlmConfig, LlmRequest};

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
    }
}

#[tokio::test]
async fn one_failing_source_does_not_block_the_others() {
    let root = std::env::temp_dir().join(format!("vibe-keep-{}", uuid::Uuid::new_v4()));
    for m in ["product", "broken"] {
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
        include_str!("../examples/shop/providers/main.md"),
    )
    .unwrap();
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            if request.input.contains("ID: broken") {
                Err("http 502 from upstream".into())
            } else {
                Ok(GOOD.to_string())
            }
        },
    };
    let config = config("https://api.openai.com/v1", 0);
    let report = compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .err()
    .expect("broken source fails the run");
    let text = report.to_string();
    assert!(text.contains("models/broken/broken.md"), "{text}");
    assert!(text.contains("1 file(s) failed"), "{text}");
    assert!(root.join("models/product/product.lock.md").exists());
    assert!(root.join("providers/main.lock.md").exists());
    assert!(!root.join("models/broken/broken.lock.md").exists());
}

/// Serves `502` once, then a valid OpenAI chat completion.
async fn flaky_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for n in 0.. {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).await;
            let resp = if n == 0 {
                "HTTP/1.1 502 Bad Gateway\r\ncontent-type: text/plain\r\ncontent-length: 3\r\nconnection: close\r\n\r\nbad".to_string()
            } else {
                let body = serde_json::json!({
                    "model": "gpt-test-2026-01-01",
                    "choices": [{"message": {"content": "ok"}}]
                })
                .to_string();
                format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
            };
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}/v1")
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
    let err = adapter.compile(&request()).await.err().expect("no retry");
    assert!(err.contains("502"), "{err}");
}

#[test]
fn retry_delay_grows_and_is_capped() {
    use vibe::llm::retry_delay;
    assert!(retry_delay(0) >= Duration::from_secs(1));
    assert!(retry_delay(1) >= Duration::from_secs(2));
    assert!(retry_delay(10) <= Duration::from_secs(20));
}

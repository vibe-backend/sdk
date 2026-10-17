//! #37/#83: format re-prompts draw from the same per-source LLM call budget
//! as transport retries. Own test binary because it sets process-wide env.

use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vibe::compile::{compile_project, CompileOptions};
use vibe::llm::{HttpAdapter, LlmConfig};

const NESTED: &str =
    "## Table\\n- provider: main\\n- table: products\\n## Fields\\n- sku\\n  - type: string\\n";

#[tokio::test]
async fn format_retries_stop_at_the_call_budget() {
    std::env::set_var("LLM_FORMAT_RETRIES", "5");
    std::env::set_var("LLM_MAX_CALLS_PER_SOURCE", "2");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicU32::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0u8; 65536];
            let _ = sock.read(&mut buf).await;
            let body = format!(
                "{{\"model\":\"gpt-test-2026-01-01\",\"choices\":[{{\"message\":{{\"content\":\"{NESTED}\"}}}}]}}"
            );
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes()).await;
        }
    });
    let root = std::env::temp_dir().join(format!("vibe-budget-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("models/product")).unwrap();
    fs::create_dir_all(root.join("providers")).unwrap();
    fs::write(
        root.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    fs::write(
        root.join("models/product/product.md"),
        "# product\n\nA product has a unique sku.\n",
    )
    .unwrap();
    let config = LlmConfig {
        provider: "openai".into(),
        base_url: format!("http://{addr}/v1"),
        api_key: "sk-test-value".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout: Duration::from_secs(5),
        max_retries: 0,
        seed: 0,
        endpoint_host: "127.0.0.1".into(),
        endpoint_private: false,
        send_sampling: true,
        api_style: "chat".into(),
    };
    let adapter = HttpAdapter::new(config.clone()).unwrap();
    let Err(report) = compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    else {
        panic!("must fail");
    };
    let text = format!("{report:?}");
    assert!(text.contains("call budget"), "{text}");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

//! #83: the overall retry deadline stops backoff early. Own test binary
//! because it sets a process-wide env var.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vibe::llm::{HttpAdapter, LlmAdapter, LlmConfig, LlmRequest};

#[tokio::test]
async fn retries_stop_at_the_deadline() {
    std::env::set_var("LLM_RETRY_DEADLINE", "0");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        }
    });
    let adapter = HttpAdapter::new(LlmConfig {
        provider: "openai".into(),
        base_url: format!("http://{addr}/v1"),
        api_key: "sk-test-value".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout: Duration::from_secs(5),
        max_retries: 5,
        seed: 0,
        endpoint_host: "127.0.0.1".into(),
        endpoint_private: false,
        api_style: "chat".into(),
    })
    .unwrap();
    let started = std::time::Instant::now();
    let Err(err) = adapter
        .compile(&LlmRequest {
            system: "s".into(),
            input: "i".into(),
            model: "gpt-test-2026-01-01".into(),
            seed: 0,
            timeout: Duration::from_secs(5),
            budget: None,
        })
        .await
    else {
        panic!("must fail");
    };
    assert!(err.contains("retry deadline"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(1));
}

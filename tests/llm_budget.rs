#![cfg(feature = "sdk")]

//! #83: one budget caps every LLM attempt (calls and wall clock), and no
//! attempt may overrun it by an LLM_TIMEOUT.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vibe::llm::{HttpAdapter, LlmAdapter, LlmConfig, LlmRequest};
use vibe::llm_budget::{CallBudget, Limits};

/// A server that answers 503 (or hangs) and counts requests.
async fn server(hang: bool) -> (String, Arc<AtomicU32>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicU32::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                if hang {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                let _ = sock
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nretry-after: 0\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
            });
        }
    });
    (format!("http://{addr}/v1"), hits)
}

fn adapter(base_url: String, timeout: Duration) -> HttpAdapter {
    HttpAdapter::new(LlmConfig {
        provider: "openai".into(),
        base_url,
        api_key: "sk-test-value".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout,
        max_retries: 30,
        seed: 0,
        endpoint_host: "127.0.0.1".into(),
        endpoint_private: false,
        send_sampling: true,
        api_style: "chat".into(),
    })
    .unwrap()
}

fn request(budget: &Arc<CallBudget>) -> LlmRequest {
    LlmRequest {
        system: "s".into(),
        input: "i".into(),
        model: "gpt-test-2026-01-01".into(),
        seed: 0,
        timeout: Duration::from_secs(30),
        budget: Some(budget.clone()),
    }
}

#[tokio::test]
async fn retries_stop_at_the_per_source_call_cap() {
    let (url, hits) = server(false).await;
    let budget = Arc::new(CallBudget::new(Limits {
        source_calls: 3,
        ..Limits::default()
    }));
    let err = adapter(url, Duration::from_secs(5))
        .compile(&request(&budget))
        .await
        .unwrap_err();
    assert!(err.contains("call budget"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn the_run_cap_spans_sources() {
    let (url, hits) = server(false).await;
    let budget = Arc::new(CallBudget::new(Limits {
        source_calls: 2,
        run_calls: 3,
        ..Limits::default()
    }));
    let llm = adapter(url, Duration::from_secs(5));
    budget.begin_source();
    assert!(llm.compile(&request(&budget)).await.is_err());
    budget.begin_source();
    let err = llm.compile(&request(&budget)).await.unwrap_err();
    assert!(err.contains("this run"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_hanging_call_cannot_overrun_the_time_budget() {
    let (url, _) = server(true).await;
    let budget = Arc::new(CallBudget::new(Limits {
        source_time: Duration::from_millis(500),
        ..Limits::default()
    }));
    let started = std::time::Instant::now();
    // LLM_TIMEOUT is 30s; the budget has 0.5s left.
    let err = adapter(url, Duration::from_secs(30))
        .compile(&request(&budget))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3), "{err}");
}

#[tokio::test]
async fn the_budget_holds_for_any_adapter() {
    let calls = Arc::new(AtomicU32::new(0));
    let seen = calls.clone();
    let fake = vibe::llm::FakeAdapter {
        responder: move |_: &LlmRequest| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        },
    };
    let budget = Arc::new(CallBudget::new(Limits {
        source_calls: 2,
        ..Limits::default()
    }));
    let req = request(&budget);
    assert!(vibe::llm::call_budgeted(&fake, &req).await.is_ok());
    assert!(vibe::llm::call_budgeted(&fake, &req).await.is_ok());
    let err = vibe::llm::call_budgeted(&fake, &req).await.unwrap_err();
    assert!(err.contains("call budget"), "{err}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

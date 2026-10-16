//! Turnstile verification against a mock siteverify (#118).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::State, routing::post, Form, Json, Router};
use serde_json::{json, Value};
use vibe::runtime::challenge::{
    ChallengeContext, ChallengeError, ChallengeVerifier, ReplayCache, TurnstileConfig,
    TurnstileVerifier,
};

type FormLog = Arc<Mutex<Vec<Vec<(String, String)>>>>;

#[derive(Clone, Default)]
struct Mock {
    /// Response body per token; unknown tokens get `success: false`.
    answers: Arc<Mutex<Vec<(String, Value)>>>,
    calls: FormLog,
}

async fn siteverify(
    State(mock): State<Mock>,
    Form(form): Form<Vec<(String, String)>>,
) -> Json<Value> {
    mock.calls.lock().unwrap().push(form.clone());
    let token = form
        .iter()
        .find(|(k, _)| k == "response")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let answer = mock
        .answers
        .lock()
        .unwrap()
        .iter()
        .find(|(t, _)| *t == token)
        .map(|(_, v)| v.clone());
    Json(
        answer.unwrap_or_else(
            || json!({"success": false, "error-codes": ["invalid-input-response"]}),
        ),
    )
}

async fn start(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}/siteverify")
}

fn verifier(url: String) -> TurnstileVerifier {
    TurnstileVerifier::new(
        TurnstileConfig {
            secret: "secret-key".into(),
            hostnames: vec!["shop.example.com".into()],
            siteverify_url: url,
        },
        reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap(),
    )
}

fn ctx() -> ChallengeContext {
    ChallengeContext {
        action: "register".into(),
        remote_ip: Some("203.0.113.9".into()),
    }
}

fn ok_answer(hostname: &str, action: &str, ts: chrono::DateTime<chrono::Utc>) -> Value {
    json!({"success": true, "hostname": hostname, "action": action, "challenge_ts": ts.to_rfc3339(), "error-codes": []})
}

fn rejected(r: Result<(), ChallengeError>) -> bool {
    matches!(r, Err(ChallengeError::Rejected(_)))
}

#[tokio::test]
async fn turnstile_accepts_only_a_fresh_bound_single_use_token() {
    let mock = Mock::default();
    let now = chrono::Utc::now();
    *mock.answers.lock().unwrap() = vec![
        (
            "good".into(),
            ok_answer("shop.example.com", "register", now),
        ),
        (
            "good2".into(),
            ok_answer("SHOP.example.com", "register", now),
        ),
        (
            "wrong-host".into(),
            ok_answer("evil.example.net", "register", now),
        ),
        (
            "wrong-action".into(),
            ok_answer("shop.example.com", "login", now),
        ),
        (
            "no-action".into(),
            json!({"success": true, "hostname": "shop.example.com"}),
        ),
        (
            "expired".into(),
            ok_answer(
                "shop.example.com",
                "register",
                now - chrono::Duration::seconds(301),
            ),
        ),
    ];
    let url = start(
        Router::new()
            .route("/siteverify", post(siteverify))
            .with_state(mock.clone()),
    )
    .await;
    let v = verifier(url);

    assert_eq!(v.verify("good", &ctx()).await, Ok(()));
    // The server sent the secret, the token and the client IP.
    let call = mock.calls.lock().unwrap()[0].clone();
    assert!(call.contains(&("secret".into(), "secret-key".into())));
    assert!(call.contains(&("response".into(), "good".into())));
    assert!(call.contains(&("remoteip".into(), "203.0.113.9".into())));
    // Replay is refused locally, without asking the provider again.
    assert!(rejected(v.verify("good", &ctx()).await));
    assert_eq!(mock.calls.lock().unwrap().len(), 1);
    // Hostnames compare case-insensitively.
    assert_eq!(v.verify("good2", &ctx()).await, Ok(()));

    for token in [
        "",
        "   ",
        "forged",
        "wrong-host",
        "wrong-action",
        "no-action",
        "expired",
    ] {
        assert!(rejected(v.verify(token, &ctx()).await), "{token:?}");
    }
    assert!(rejected(v.verify(&"x".repeat(2049), &ctx()).await));
    // A failed token stays used.
    assert!(rejected(v.verify("forged", &ctx()).await));
    // Same token, different route (action): already consumed, and the
    // provider-bound action would not match anyway.
    let other = ChallengeContext {
        action: "login".into(),
        remote_ip: None,
    };
    assert!(rejected(v.verify("good", &other).await));
}

#[tokio::test]
async fn turnstile_fails_closed_when_the_provider_is_unreachable_or_broken() {
    // Nothing listening.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}/siteverify", listener.local_addr().unwrap());
    drop(listener);
    assert!(matches!(
        verifier(dead).verify("t1", &ctx()).await,
        Err(ChallengeError::Unavailable(_))
    ));

    // HTTP 500, a non-JSON body, and a timeout.
    let url = start(
        Router::new()
            .route(
                "/siteverify",
                post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
            )
            .route("/garbage", post(|| async { "not json" }))
            .route(
                "/slow",
                post(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Json(json!({"success": true}))
                }),
            ),
    )
    .await;
    let base = url.trim_end_matches("/siteverify").to_string();
    for (path, token) in [("/siteverify", "t2"), ("/garbage", "t3"), ("/slow", "t4")] {
        let got = verifier(format!("{base}{path}"))
            .verify(token, &ctx())
            .await;
        assert!(
            matches!(got, Err(ChallengeError::Unavailable(_))),
            "{path}: {got:?}"
        );
    }
}

#[tokio::test]
async fn concurrent_reuse_of_one_token_passes_at_most_once() {
    let mock = Mock::default();
    *mock.answers.lock().unwrap() = vec![(
        "t".into(),
        ok_answer("shop.example.com", "register", chrono::Utc::now()),
    )];
    let url = start(
        Router::new()
            .route("/siteverify", post(siteverify))
            .with_state(mock),
    )
    .await;
    let v =
        Arc::new(verifier(url).with_replay_cache(ReplayCache::new(Duration::from_secs(360), 10)));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let v = v.clone();
            tokio::spawn(async move { v.verify("t", &ctx()).await })
        })
        .collect();
    let mut passed = 0;
    for t in tasks {
        if t.await.unwrap().is_ok() {
            passed += 1;
        }
    }
    assert_eq!(passed, 1);
}

#[tokio::test]
async fn production_guarded_client_refuses_a_loopback_siteverify() {
    // The production constructor applies the outbound guard: a siteverify
    // URL that resolves to loopback cannot be reached.
    let v = TurnstileVerifier::guarded(TurnstileConfig {
        secret: "s".into(),
        hostnames: vec!["shop.example.com".into()],
        siteverify_url: "http://localhost:9/siteverify".into(),
    })
    .unwrap();
    assert!(matches!(
        v.verify("t", &ctx()).await,
        Err(ChallengeError::Unavailable(_))
    ));
}

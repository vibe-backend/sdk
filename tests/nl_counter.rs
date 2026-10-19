#![cfg(feature = "sdk")]
use axum::{body::Body, http::Request};
use std::{fs, path::Path, process::Command, sync::Arc};
use tower::ServiceExt;
use vibe::runtime::{router, AppState, AuthConfig, MemoryExec};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn plain_language_counter_builds_and_runs_in_memory() {
    let root = std::env::temp_dir().join(format!("vibe-nl-counter-{}", uuid::Uuid::new_v4()));
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nl-counter"),
        &root,
    );
    fs::create_dir_all(root.join(".vibe")).unwrap();
    fs::write(
        root.join(".vibe/slugs.json"),
        r#"{"demo":{"tenant_id":"demo0001","public_access":true,"public_write":true}}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .current_dir(&root)
        .env_clear()
        .env("LLM_PROVIDER", "fake")
        .arg("build")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("api/web/v1/increment-counter.md: the lock cannot express \"Anyone can add one to the counter; at most 50 per day.\"; it is not enforced"), "{stderr}");
    let key = vibe::sign::load_dev_key(&root).unwrap();
    let manifest = vibe::manifest::manifest_from_project(&root, &key).unwrap();
    let app = router(Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth: AuthConfig::anonymous(),
        slugs: vibe::runtime::load_project_slugs(&root, true)
            .unwrap()
            .slugs,
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
    for (method, value) in [("GET", 0), ("POST", 1), ("GET", 1)] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/web/v1/counter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["value"], value, "{json}");
    }
    fs::remove_dir_all(root).unwrap();
}

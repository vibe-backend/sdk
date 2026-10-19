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

fn fake_config() -> vibe::llm::LlmConfig {
    vibe::llm::load_llm_config(
        &std::collections::HashMap::from([("LLM_PROVIDER".into(), "fake".into())]),
        false,
    )
    .unwrap()
}

#[tokio::test]
async fn sql_model_and_memory_errors_reach_repair() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (bad_sql, expected) in [
        (
            "SELECT value FROM invented WHERE name = $1",
            "unknown table invented",
        ),
        (
            "SELECT missing FROM counters WHERE name = $1",
            "counters.missing",
        ),
        (
            "SELECT tenant_id FROM counters WHERE name = $1",
            "counters.tenant_id",
        ),
        (
            "SELECT ABS(value) FROM counters WHERE name = $1",
            "MEMORY_UNSUPPORTED_SQL",
        ),
        (
            "INSERT INTO counters (name, value) VALUES ($1, 1) RETURNING value",
            "action create is not allowed by model counter",
        ),
    ] {
        let root = std::env::temp_dir().join(format!("vibe-sql-repair-{}", uuid::Uuid::new_v4()));
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web-counter"),
            &root,
        );
        if expected.starts_with("action create") {
            fs::remove_file(root.join("models/counter/methods/increment.md")).unwrap();
            fs::remove_file(root.join("api/web/v1/increment-counter.md")).unwrap();
            let model_path = root.join("models/counter/counter.md");
            let model = fs::read_to_string(&model_path)
                .unwrap()
                .replace("| create | - |\n", "");
            fs::write(model_path, model).unwrap();
        }
        fs::write(
            root.join("models/counter/methods/read.md"),
            "Read the counter by name; return zero if absent.",
        )
        .unwrap();
        let good_sql = "SELECT COALESCE(MAX(value), 0) AS value FROM counters WHERE name = $1";
        let body = |sql: &str| {
            format!("## Signature\n- input: {{name: string}}\n- output: {{value: integer}}\n- errors: []\n- writes: false\n## Calls\n(none)\n## Steps\n### s1\n- op: query\n- sql: `{sql}`\n- params: [input.name]\n- writes: false\n- out: result\n- source-lines: 1-1\n### s2\n- op: respond\n- value: result\n- source-lines: 1-1\n")
        };
        let calls = AtomicUsize::new(0);
        let adapter = vibe::FakeAdapter {
            responder: |request: &vibe::llm::LlmRequest| {
                assert!(request.input.contains("table=counters"));
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(body(bad_sql))
                } else {
                    assert!(request.input.contains(expected), "{}", request.input);
                    Ok(body(good_sql))
                }
            },
        };
        vibe::regenerate_project(&vibe::CompileOptions {
            root: &root,
            llm: Some(&adapter),
            llm_config: Some(&fake_config()),
            check: false,
        })
        .await
        .unwrap_or_else(|r| panic!("{r}"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn route_prefix_and_slug_errors_reach_repair() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (bad_path, bad_tenant, expected) in [
        (
            "/api/wrong/v1/counter",
            "default-slug",
            "route prefix cannot be stripped",
        ),
        (
            "/web/v1/{slug}/counter",
            "default-slug",
            "route uses {slug}",
        ),
        ("/web/v1/counter", "path.slug", "replace tenant: path.slug"),
        ("/api/web/v1/counter", "default-slug", ""),
    ] {
        let root = std::env::temp_dir().join(format!("vibe-route-repair-{}", uuid::Uuid::new_v4()));
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web-counter"),
            &root,
        );
        let source = root.join("api/web/v1/counter.md");
        let good = fs::read_to_string(&source).unwrap();
        let good = good.split_once("## Route").unwrap().1;
        let good = format!("## Route{good}");
        let bad = good
            .replace("/web/v1/counter", bad_path)
            .replace("tenant: default-slug", &format!("tenant: {bad_tenant}"));
        fs::write(
            &source,
            "Anyone can read the counter. Use GET at /web/v1/counter.",
        )
        .unwrap();
        let calls = AtomicUsize::new(0);
        let adapter = vibe::FakeAdapter {
            responder: |request: &vibe::llm::LlmRequest| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(bad.clone())
                } else {
                    assert!(request.input.contains(expected));
                    Ok(good.clone())
                }
            },
        };
        vibe::regenerate_project(&vibe::CompileOptions {
            root: &root,
            llm: Some(&adapter),
            llm_config: Some(&fake_config()),
            check: false,
        })
        .await
        .unwrap_or_else(|r| panic!("{r}"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if expected.is_empty() { 1 } else { 2 }
        );
        let lock = fs::read_to_string(root.join("api/web/v1/counter.lock.md")).unwrap();
        assert!(lock.contains("- path: /web/v1/counter\n"), "{lock}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn public_gate_repairs_both_directions() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (prose, public) in [
        ("Anyone can read the counter.", true),
        ("This is a public endpoint.", true),
        ("No login needed.", true),
        ("任何人都可以讀", true),
        ("唔使登入", true),
        ("Read my counter.", false),
        ("This endpoint is not public.", false),
        ("Not for anyone without an account.", false),
        ("Only staff can read it; it requires login.", false),
        ("唔係公開", false),
        ("任何人都唔可以睇", false),
        ("需要登入", false),
        ("Anyone can read it; login required.", false),
        (
            "This is public but not for anyone without an account.",
            false,
        ),
    ] {
        let root =
            std::env::temp_dir().join(format!("vibe-public-repair-{}", uuid::Uuid::new_v4()));
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web-counter"),
            &root,
        );
        let path = root.join("api/web/v1/counter.md");
        let original = fs::read_to_string(&path).unwrap();
        let public_body = format!("## Route{}", original.split_once("## Route").unwrap().1);
        let private_body = public_body
            .replace("- public: true", "- public: false")
            .replace("### -auth\n\n- removed: true", "### auth\n\n- mode: jwt");
        fs::write(path, prose).unwrap();
        let (bad, good) = if public {
            (&private_body, &public_body)
        } else {
            (&public_body, &private_body)
        };
        let calls = AtomicUsize::new(0);
        let adapter = vibe::FakeAdapter {
            responder: |request: &vibe::llm::LlmRequest| {
                if request.system == vibe::compile::TRANSLATE_PROMPT {
                    return Ok("Read the counter.".into());
                }
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(bad.clone())
                } else {
                    assert!(
                        request.input.contains(if public {
                            "source clearly says"
                        } else {
                            "source does not clearly say"
                        }),
                        "{prose}: {}",
                        request.input
                    );
                    Ok(good.clone())
                }
            },
        };
        vibe::regenerate_project(&vibe::CompileOptions {
            root: &root,
            llm: Some(&adapter),
            llm_config: Some(&fake_config()),
            check: false,
        })
        .await
        .unwrap_or_else(|r| panic!("{r}"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let lock = fs::read_to_string(root.join("api/web/v1/counter.lock.md")).unwrap();
        assert!(
            lock.contains(&format!("- public: {public}\n")),
            "{prose}: {lock}"
        );

        fs::remove_dir_all(root).unwrap();
    }
}

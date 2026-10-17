#![cfg(feature = "sdk")]

//! #37: on a lock grammar error the compiler re-prompts with the validator
//! errors, a bounded number of times.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use vibe::compile::{regenerate_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};

const GOOD: &str = "## Table\n- provider: main\n- table: products\n## Fields\n| name | type | rules | system |\n| --- | --- | --- | --- |\n| sku | string | required, unique, max: 40 | no |\n## CRUD\n| method | options |\n| --- | --- |\n| get | - |\n## Policies\n(none)\n";
/// Nested bullets: the shape gpt-5.5 kept producing in #37/#83.
const NESTED: &str =
    "## Table\n- provider: main\n- table: products\n## Fields\n- sku\n  - type: string\n";

fn project() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-repair-{}", uuid::Uuid::new_v4()));
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
    root
}

fn config() -> LlmConfig {
    LlmConfig {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        api_key: "sk-test-value".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        seed: 0,
        endpoint_host: "api.openai.com".into(),
        endpoint_private: false,
        send_sampling: true,
        api_style: "chat".into(),
    }
}

#[tokio::test]
async fn grammar_error_is_repaired_with_validator_feedback() {
    let root = project();
    let calls = AtomicUsize::new(0);
    let inputs = Mutex::new(Vec::new());
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            inputs.lock().unwrap().push(request.input.clone());
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(NESTED.to_string())
            } else {
                Ok(GOOD.to_string())
            }
        },
    };
    let config = config();
    regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let inputs = inputs.lock().unwrap();
    assert!(
        inputs[1].contains("bullet must be `- key: value`"),
        "{}",
        inputs[1]
    );
    assert!(inputs[1].contains("PREVIOUS OUTPUT"));
    assert!(inputs[1].starts_with(&inputs[0]));
    assert!(root.join("models/product/product.lock.md").exists());
}

#[tokio::test]
async fn repair_attempts_are_bounded() {
    let root = project();
    let calls = AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |_: &LlmRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(NESTED.to_string())
        },
    };
    let config = config();
    let err = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .err()
    .expect("fails");
    // Default LLM_FORMAT_RETRIES = 2: one call plus two repairs.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(format!("{err}").contains("after 3 attempt(s)"), "{err}");
}

#[tokio::test]
async fn model_prompt_example_passes_the_validator() {
    let root = project();
    fs::write(
        root.join("models/product/product.md"),
        vibe::prompt_guide::MODEL_EXAMPLE,
    )
    .unwrap();
    regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    let report = vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
    })
    .unwrap_or_else(|err| panic!("{err}"));
    assert!(!report.has_errors());
}

const API_GOOD: &str = "## Route\n- method: GET\n- path: /catalog/v1/hello\n## Access\n- public: false\n## Pipelines\n(none)\n## Limits\n- provider: main\n## Request\n(none)\n## Response\n- status: 200\n## Transform\n(none)\n## Steps\n### s1 respond\n- op: respond\n- value: { message: \"hello world\" }\n- source-lines: 3\n## Errors\n(none)\n";

fn api_project() -> std::path::PathBuf {
    let root = project();
    fs::remove_dir_all(root.join("models")).unwrap();
    fs::create_dir_all(root.join("api/catalog/v1/hello")).unwrap();
    fs::write(
        root.join("api/catalog/_definition.md"),
        include_str!("fixtures/shop/api/catalog/_definition.md"),
    )
    .unwrap();
    fs::write(
        root.join("api/catalog/v1/_version.md"),
        include_str!("fixtures/shop/api/catalog/v1/_version.md"),
    )
    .unwrap();
    fs::write(
        root.join("api/catalog/v1/hello/hello.md"),
        "# hello\n\nReturn a hello world message.\n",
    )
    .unwrap();
    root
}

fn invalid_api() -> String {
    API_GOOD.replace("### s1 respond", "### s0 transform\n- op: transform\n- in: req.body\n- out: input\n- source-lines: 3\n### s1 respond")
}

#[tokio::test]
async fn validate_feedback_repairs_api_steps() {
    let root = api_project();
    let calls = AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(invalid_api())
            } else {
                assert!(
                    request
                        .input
                        .contains("api step op `transform` was removed"),
                    "{}",
                    request.input
                );
                Ok(API_GOOD.into())
            }
        },
    };
    regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config()),
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invalid_api_exhausts_shared_retry_budget_without_writes() {
    let root = api_project();
    let calls = AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |_: &LlmRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(invalid_api())
        },
    };
    let err = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config()),
        check: false,
    })
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(err.contains("does not pass validate"), "{err}");
    assert!(err.contains("api step op `transform` was removed"), "{err}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1 + vibe::prompt_guide::format_retries().unwrap() as usize
    );
    assert!(!root.join("providers/main.lock.md").exists());
    assert!(!root.join("api/catalog/v1/hello/hello.lock.md").exists());
    assert!(!root.join(vibe::buildrecord::PATH).exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn api_system_prompt_explains_expression_scopes() {
    let prompt =
        vibe::compile::system_prompt(vibe::lock::Kind::Api, Some(vibe::lock::Scope::Feature));
    assert!(prompt.contains("do not emit a transform step"));
    assert!(prompt.contains("Transform in roots: req, ctx only"));
}

#[tokio::test]
async fn cached_lock_failing_validate_is_recompiled() {
    let root = api_project();
    let calls = AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |_: &LlmRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(API_GOOD.into())
        },
    };
    let config = config();
    let opts = CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    };
    regenerate_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let cache: Vec<_> = fs::read_dir(root.join(".vibe/cache/llm"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(cache.len(), 1);
    fs::write(&cache[0], invalid_api()).unwrap();
    regenerate_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!fs::read_to_string(&cache[0])
        .unwrap()
        .contains("op: transform"));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn structured_limits_pins_and_slug_normalization() {
    for slug in [true, false] {
        let root = api_project();
        let source_path = root.join("api/catalog/v1/hello/hello.md");
        let body = API_GOOD.replace("- provider: main", "- provider: main\n- timeout: 5s\n- body: 4KB")
            .replace("## Request\n(none)", "## Request\n| field | type | rules | required |\n| --- | --- | --- | --- |\n| path.slug | uuid | - | no |\n| path.slug | string | - | no |")
            .replace("/catalog/v1/hello", if slug { "/catalog/v1/{slug}/hello" } else { "/catalog/v1/hello" });
        fs::write(
            &source_path,
            body.replace("- provider: main", "- provider: other"),
        )
        .unwrap();
        let opts = CompileOptions {
            root: &root,
            llm: None,
            llm_config: None,
            check: false,
        };
        let error = regenerate_project(&opts).await.err().unwrap().to_string();
        for (key, value, resolved) in [
            ("provider", "other", "main"),
            ("timeout", "5s", "10s"),
            ("body", "4KB", "1MB"),
        ] {
            assert!(error.contains(&format!("`## Limits` {key}: source value `{value}` differs from the resolved value `{resolved}`")), "{error}");
            assert!(
                error.contains(&format!("pin it in frontmatter (`{key}: {value}`)")),
                "{error}"
            );
            assert!(
                error.contains(&format!("delete the `{key}` line from `## Limits`")),
                "{error}"
            );
        }
        assert!(!root.join("api/catalog/v1/hello/hello.lock.md").exists());
        fs::write(
            &source_path,
            format!("---\ntimeout: 5s\nbody: 4KB\n---\n{body}"),
        )
        .unwrap();
        regenerate_project(&opts)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let lock = fs::read_to_string(root.join("api/catalog/v1/hello/hello.lock.md")).unwrap();
        assert!(lock.contains("- provider: main\n- timeout: 5s\n- body: 4KB"));
        assert_eq!(lock.matches("| path.slug |").count(), usize::from(slug));
        if slug {
            assert!(lock.contains("| path.slug | string | - | yes |"));
        } else {
            assert!(lock.contains("## Request\n(none)"));
        }
        fs::remove_dir_all(root).unwrap();
    }
}

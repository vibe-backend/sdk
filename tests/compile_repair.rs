//! #37: on a lock grammar error the compiler re-prompts with the validator
//! errors, a bounded number of times.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use vibe::compile::{compile_project, CompileOptions};
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
        include_str!("../examples/shop/providers/main.md"),
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
    compile_project(&CompileOptions {
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
    let err = compile_project(&CompileOptions {
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
    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    let report = vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        root: &root,
        deploy: false,
        verify_key: None,
    })
    .unwrap_or_else(|err| panic!("{err}"));
    assert!(!report.has_errors());
}

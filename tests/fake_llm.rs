use std::fs;
use std::time::Duration;

use vibe::compile::{compile_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};
use vibe::lock::{parse_lock, SourceType};
use vibe::validate::{validate_project, ValidateOptions};

const SECTIONS: &str = r#"## Table

- provider: main
- table: products

## Fields

| name | type | rules | system |
| --- | --- | --- | --- |
| sku | string | required, unique, max: 40 | no |

## CRUD

| method | options |
| --- | --- |
| create | - |
| get | - |

## Policies

- rls: enable, force
- policy: tenant_id = current_setting('app.tenant_id', false)
"#;

#[tokio::test]
async fn fake_adapter_compiles_natural_language_without_a_network() {
    let root = std::env::temp_dir().join(format!("vibe-llm-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("models/product")).unwrap();
    fs::create_dir_all(root.join("providers")).unwrap();
    fs::write(
        root.join("providers/main.md"),
        include_str!("../examples/shop/providers/main.md"),
    )
    .unwrap();
    fs::write(
        root.join("models/product/product.md"),
        "# product\n\nStaff can create a product from a sku.\n",
    )
    .unwrap();

    let failed = compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await;
    assert!(failed.is_err());

    let config = LlmConfig {
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
    };
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            assert_eq!(request.model, "gpt-test-2026-01-01");
            assert_eq!(request.seed, 0);
            assert!(request.system.contains("No frontmatter"));
            Ok(format!("```markdown\n{SECTIONS}\n```"))
        },
    };
    compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));

    let text = fs::read_to_string(root.join("models/product/product.lock.md")).unwrap();
    let doc = parse_lock(&text).unwrap();
    assert_eq!(doc.meta.source_type, SourceType::Natural);
    assert_eq!(doc.meta.compiler.llm_provider, "openai");
    assert_eq!(doc.meta.compiler.model_snapshot, "gpt-test-2026-01-01");
    assert!(doc
        .meta
        .security_items
        .iter()
        .any(|item| item.item == "write-sql"));
    validate_project(&ValidateOptions {
        root: &root,
        deploy: false,
        verify_key: None,
    })
    .unwrap_or_else(|err| panic!("{err}"));
}

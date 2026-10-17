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
        include_str!("fixtures/shop/providers/main.md"),
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
        endpoint_private: true,
        send_sampling: true,
        api_style: "chat".into(),
    };
    let malformed = FakeAdapter {
        responder: |_: &LlmRequest| Ok("BODY-MARKER-7f3a Bearer sk-test-value".into()),
    };
    let errors = compile_project(&CompileOptions {
        root: &root,
        llm: Some(&malformed),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .err()
    .expect("malformed output must fail");
    let message = format!("{errors:?}");
    assert!(message.contains("LLM output is not lock markdown"));
    for secret in ["BODY-MARKER-7f3a", "sk-test-value", "Bearer sk-test-value"] {
        assert!(!message.contains(secret), "{message}");
    }

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

    let provider =
        parse_lock(&fs::read_to_string(root.join("providers/main.lock.md")).unwrap()).unwrap();
    assert_eq!(
        provider.meta.compiler,
        vibe::lock::CompilerStamp::deterministic()
    );
    compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: true,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let text = fs::read_to_string(root.join("models/product/product.lock.md")).unwrap();
    let doc = parse_lock(&text).unwrap();
    assert_eq!(doc.meta.source_type, SourceType::Natural);
    assert_eq!(doc.meta.compiler.llm_provider, "openai");
    assert!(doc.meta.compiler.endpoint_private);
    assert_eq!(doc.meta.compiler.model_snapshot, "gpt-test-2026-01-01");
    assert!(doc
        .meta
        .security_items
        .iter()
        .any(|item| item.item == "write-sql"));
    validate_project(&ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
        verify_key: None,
    })
    .unwrap_or_else(|err| panic!("{err}"));
}

#[tokio::test]
async fn structured_locks_ignore_llm_config_and_check_consistently() {
    let root = std::env::temp_dir().join(format!("vibe-structured-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("providers")).unwrap();
    fs::write(
        root.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    let config = LlmConfig {
        provider: "openai".into(),
        base_url: "http://localhost/v1".into(),
        api_key: "test-key".into(),
        model: "test-model".into(),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        seed: 0,
        endpoint_host: "localhost".into(),
        endpoint_private: true,
        send_sampling: true,
        api_style: "chat".into(),
    };
    for (check, config) in [(false, Some(&config)), (true, Some(&config)), (true, None)] {
        compile_project(&CompileOptions {
            root: &root,
            llm: None,
            llm_config: config,
            check,
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    }
    let text = fs::read_to_string(root.join("providers/main.lock.md")).unwrap();
    let lock = parse_lock(&text).unwrap();
    assert_eq!(
        lock.meta.compiler,
        vibe::lock::CompilerStamp::deterministic()
    );
    validate_project(&ValidateOptions {
        root: &root,
        deploy: false,
        dev: false,
        verify_key: None,
    })
    .unwrap_or_else(|e| panic!("{e}"));
}

const AUTH_REMOVAL_PROSE: &[&str] = &[
    "唔需要登入",
    "不需要登入",
    "免登入",
    "移除 auth",
    "唔使 auth",
    "公開，唔使 auth",
    "no auth",
    "remove auth",
    "auth removed",
    "public, no login required",
    "no login required",
    "does not require login",
    "without auth",
];

async fn compile_auth_feature(
    auth: &str,
    prose: &str,
    emitted_auth: &str,
    inherited_auth: bool,
) -> (
    std::path::PathBuf,
    Result<vibe::compile::CompileOutput, vibe::diag::Report>,
) {
    let root = std::env::temp_dir().join(format!("vibe-auth-removal-{}", uuid::Uuid::new_v4()));
    for dir in ["providers", "api/catalog/v1/greet"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(
        root.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    let definition = include_str!("fixtures/shop/api/catalog/_definition.md");
    fs::write(
        root.join("api/catalog/_definition.md"),
        if inherited_auth {
            definition.to_string()
        } else {
            definition.replace("### auth\n\n- mode: jwt\n- roles: [staff]", "(none)")
        },
    )
    .unwrap();
    fs::write(
        root.join("api/catalog/v1/_version.md"),
        include_str!("fixtures/shop/api/catalog/v1/_version.md"),
    )
    .unwrap();
    fs::write(
        root.join("api/catalog/v1/greet/greet.md"),
        format!("---\npublic: true\nwrites: false\nauth: {auth}\n---\n# greet\n{prose}\n"),
    )
    .unwrap();
    let body = format!(
        "## Route
- method: GET
- path: /catalog/v1/{{slug}}/greet
## Access
- public: true
- public-writes: false
- tenant: path.slug
## Pipelines
{emitted_auth}
## Limits
- provider: main
## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| path.slug | string | - | yes |
## Response
- status: 200
## Steps
### s1 respond
- op: respond
- value: {{message: \"hello\"}}
- source-lines: 7
## Errors
(none)
"
    );
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            if request.system == vibe::compile::TRANSLATE_PROMPT {
                // Translation cannot provide support or override the original prose.
                Ok("A greeting route.".into())
            } else {
                Ok(body.clone())
            }
        },
    };
    let config = LlmConfig {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        api_key: "test-key".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        seed: 0,
        endpoint_host: "api.openai.com".into(),
        endpoint_private: false,
        send_sampling: true,
        api_style: "chat".into(),
    };
    let result = compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await;
    (root, result)
}

#[tokio::test]
async fn auth_none_overrides_every_model_auth_form_and_recognizes_removal_prose() {
    for phrase in AUTH_REMOVAL_PROSE {
        for prose in [
            phrase.to_string(),
            format!("GET /greet: {}.", phrase.to_uppercase()),
        ] {
            for emitted in [
                "(none)",
                "### auth\n- mode: jwt",
                "### -auth\n- removed: true",
            ] {
                for inherited in [false, true] {
                    let (root, result) =
                        compile_auth_feature("none", &prose, emitted, inherited).await;
                    let out = result.unwrap_or_else(|err| panic!("{prose} / {emitted}: {err}"));
                    assert!(
                        !out.report.to_string().contains("ambiguous negation"),
                        "{}",
                        out.report
                    );
                    let text = fs::read_to_string(root.join("api/catalog/v1/greet/greet.lock.md"))
                        .unwrap();
                    let lock = parse_lock(&text).unwrap();
                    let pipelines = lock
                        .sections
                        .iter()
                        .find(|section| section.name == "Pipelines")
                        .unwrap();
                    assert_eq!(pipelines.blocks.len(), 1, "{text}");
                    assert_eq!(pipelines.blocks[0].heading, "-auth", "{text}");
                    assert_eq!(pipelines.blocks[0].bullets[0].key, "removed");
                    assert_eq!(pipelines.blocks[0].bullets[0].value, "true");
                    assert!(!text.contains("mode: jwt"), "{text}");
                    validate_project(&ValidateOptions {
                        root: &root,
                        dev: false,
                        deploy: false,
                        verify_key: None,
                    })
                    .unwrap_or_else(|err| panic!("{prose} / {emitted}: {err}"));
                    fs::remove_dir_all(root).unwrap();
                }
            }
        }
    }
}

#[tokio::test]
async fn disabled_auth_aliases_force_removal_even_without_removal_prose() {
    for auth in ["none", "removed", "false", "off"] {
        let (root, result) =
            compile_auth_feature(auth, "A greeting route.", "### auth\n- mode: jwt", true).await;
        result.unwrap_or_else(|err| panic!("{auth}: {err}"));
        let text = fs::read_to_string(root.join("api/catalog/v1/greet/greet.lock.md")).unwrap();
        assert!(text.contains("### -auth\n- removed: true"), "{text}");
        assert!(!text.contains("mode: jwt"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn login_requirements_and_required_frontmatter_still_reject_auth_removal() {
    for phrase in [
        "需要登入",
        "requires login",
        "login required",
        "must log in",
    ] {
        let (root, result) = compile_auth_feature("none", phrase, "(none)", true).await;
        let report = result
            .err()
            .expect("required login must conflict with auth: none");
        assert!(
            report.to_string().contains("conflicts with source wording"),
            "{phrase}: {report}"
        );
        fs::remove_dir_all(root).unwrap();
    }
    let (root, result) = compile_auth_feature(
        "jwt",
        "A greeting route.",
        "### -auth\n- removed: true",
        true,
    )
    .await;
    let report = result
        .err()
        .expect("required auth must reject model removal");
    assert!(
        report
            .to_string()
            .contains("source frontmatter pins auth: jwt"),
        "{report}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn auth_double_negations_still_warn_and_retain_the_requirement() {
    for phrase in [
        "唔可以唔登入",
        "not without login",
        "不得不登入",
        "not without auth",
        "not no auth",
    ] {
        let (root, result) =
            compile_auth_feature("jwt", phrase, "### auth\n- mode: jwt", true).await;
        let out = result.unwrap_or_else(|err| panic!("{phrase}: {err}"));
        assert!(
            out.report.to_string().contains("ambiguous negation"),
            "{phrase}: {}",
            out.report
        );
        let text = fs::read_to_string(root.join("api/catalog/v1/greet/greet.lock.md")).unwrap();
        assert!(text.contains("mode: jwt"), "{text}");
        assert!(!text.contains("### -auth"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }
}

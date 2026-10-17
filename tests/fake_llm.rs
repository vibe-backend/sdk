#![cfg(feature = "sdk")]

use std::fs;
use std::time::Duration;

use vibe::compile::{regenerate_project, CompileOptions};
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

    let failed = regenerate_project(&CompileOptions {
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
    let errors = regenerate_project(&CompileOptions {
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
    regenerate_project(&CompileOptions {
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
    let check = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: true,
    })
    .await
    .err()
    .expect("build --check applies the release deploy policy");
    assert!(check
        .to_string()
        .contains("private endpoint cannot be deployed"));
    let text = fs::read_to_string(root.join("models/product/product.lock.md")).unwrap();
    let doc = parse_lock(&text).unwrap();
    assert_eq!(doc.meta.source_type, SourceType::Natural);
    let record: vibe::buildrecord::BuildRecord =
        serde_json::from_str(&fs::read_to_string(root.join(vibe::buildrecord::PATH)).unwrap())
            .unwrap();
    let stamp = &record.compilers["models/product/product.lock.md"];
    assert_eq!(stamp.llm_provider, "openai");
    assert!(stamp.endpoint_private);
    assert_eq!(stamp.model_snapshot, "gpt-test-2026-01-01");
    assert!(doc
        .meta
        .security_items
        .iter()
        .any(|item| item.item == "write-sql"));
    validate_project(&ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
    })
    .unwrap_or_else(|err| panic!("{err}"));

    // Private-endpoint natural-language output supports the complete local
    // deploy/manifest path without any lock approval or pre-existing key.
    validate_project(&ValidateOptions {
        root: &root,
        deploy: true,
        dev: true,
    })
    .unwrap_or_else(|err| panic!("{err}"));
    let key = vibe::sign::load_or_create(&root).unwrap();
    let manifest = vibe::manifest::manifest_from_project_with_mode(&root, &key, true)
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(manifest.build_mode, Some(vibe::manifest::BuildMode::Dev));
    let path = root.join("dev.json");
    vibe::manifest::write_manifest(&path, &manifest).unwrap();
    let public = vibe::sign::load_dev_public(&root).unwrap();
    assert_eq!(
        vibe::manifest::read_manifest(&path, &public).unwrap(),
        manifest
    );

    // A hand edit of a natural-language lock (cannot be recompiled offline)
    // must not pass deploy validation or reach a manifest.
    let deploy = || {
        validate_project(&ValidateOptions {
            dev: false,
            root: &root,
            deploy: true,
        })
    };
    let before = deploy();
    if let Err(err) = &before {
        assert!(!err.to_string().contains("hand-edited"), "{err}");
    }
    let lock = root.join("models/product/product.lock.md");
    let edited =
        text.replacen("max: 40", "max: 100000", 1)
            .replacen("required, unique,", "required,", 1);
    assert_ne!(edited, text);
    fs::write(&lock, edited).unwrap();
    let err = deploy().unwrap_err().to_string();
    assert!(err.contains("models/product/product.lock.md"), "{err}");
    assert!(err.contains("hand-edited"), "{err}");
    fs::create_dir_all(root.join(".vibe")).unwrap();
    let key = vibe::sign::generate_key();
    let err = vibe::manifest::manifest_from_project(&root, &key)
        .unwrap_err()
        .to_string();
    assert!(err.contains("hand-edited"), "{err}");
}

const METHOD: &str = r#"## Signature

- input: {id: uuid}
- output: {id: uuid}
- writes: true

## Calls

(none)

## Steps

### s1

- op: query
- writes: true
- sql: UPDATE products SET sku = 'x' WHERE id = $1
- params: [input.id]
- scope: pk
"#;

#[tokio::test]
async fn natural_language_lock_with_tautological_where_cannot_deploy() {
    let root = std::env::temp_dir().join(format!("vibe-llm-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("models/product/methods")).unwrap();
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
    fs::write(
        root.join("models/product/methods/rename.md"),
        "---\nwrites: true\n---\n# rename\n\nRename one product by id (update its sku).\n",
    )
    .unwrap();
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
        api_style: "chat".into(),
        send_sampling: true,
    };
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            let body = if request.input.contains("Rename one product") {
                METHOD
            } else {
                SECTIONS
            };
            Ok(format!("```markdown\n{body}\n```"))
        },
    };
    let compiled = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await;
    let lock = root.join("models/product/methods/rename.lock.md");
    compiled.unwrap_or_else(|err| panic!("{err}"));
    let text = fs::read_to_string(&lock).unwrap();
    assert_eq!(
        parse_lock(&text).unwrap().meta.source_type,
        SourceType::Natural
    );
    // Hand-edit the natural-language lock and forge the build record, so only
    // the deterministic policy stands between the edit and a deploy.
    let edited = text.replacen("WHERE id = $1", "WHERE true OR id = $1", 1);
    assert_ne!(edited, text);
    fs::write(&lock, &edited).unwrap();
    let loaded = vibe::validate::load_project(&root).unwrap();
    let mut locks = std::collections::BTreeMap::new();
    for (source, text) in loaded.sources.iter().zip(&loaded.lock_texts) {
        if let Some(text) = text {
            let rel = source.rel.trim_end_matches(".md").to_string() + ".lock.md";
            locks.insert(rel, vibe::buildrecord::lock_hash(text));
        }
    }
    vibe::buildrecord::write(&root, locks).unwrap();
    for deploy in [false, true] {
        let err = validate_project(&ValidateOptions {
            dev: false,
            root: &root,
            deploy,
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("rename.lock.md"), "{err}");
        assert!(err.contains("WHERE is always true"), "{err}");
    }
    let key = vibe::sign::generate_key();
    let err = vibe::manifest::manifest_from_project(&root, &key)
        .unwrap_err()
        .to_string();
    assert!(err.contains("WHERE is always true"), "{err}");
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
        regenerate_project(&CompileOptions {
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
    })
    .unwrap_or_else(|e| panic!("{e}"));
}

const AUTH_REMOVAL_PROSE: &[&str] = &[
    "呢個 API 唔需要驗證",
    "唔使驗證",
    "公開，唔使登入",
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
    compile_auth_feature_output(auth, prose, emitted_auth, inherited_auth, true, None).await
}

async fn compile_auth_feature_output(
    auth: &str,
    prose: &str,
    emitted_auth: &str,
    inherited_auth: bool,
    public: bool,
    output: Option<&str>,
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
        format!("---\npublic: {public}\nwrites: false\nauth: {auth}\n---\n# greet\n{prose}\n"),
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
    let body = output.unwrap_or(&body);
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            if request.system == vibe::compile::TRANSLATE_PROMPT {
                // Translation cannot provide support or override the original prose.
                Ok("A greeting route.".into())
            } else {
                Ok(body.to_string())
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
    let result = regenerate_project(&CompileOptions {
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
async fn structured_auth_confirms_double_negations_and_retains_the_requirement() {
    for phrase in [
        "唔可以唔登入",
        "not without login",
        "不得不登入",
        "not without auth",
        "not no auth",
    ] {
        let (root, result) = compile_auth_feature(
            "jwt",
            phrase,
            "### auth\n- mode: jwt\n- roles: [staff]",
            true,
        )
        .await;
        let out = result.unwrap_or_else(|err| panic!("{phrase}: {err}"));
        assert!(
            !out.report.to_string().contains("ambiguous negation"),
            "{phrase}: {}",
            out.report
        );
        let text = fs::read_to_string(root.join("api/catalog/v1/greet/greet.lock.md")).unwrap();
        assert!(text.contains("mode: jwt"), "{text}");
        assert!(!text.contains("### -auth"), "{text}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn optional_api_parts_normalize_deterministically() {
    for slug in [true, false] {
        let path = if slug {
            "/catalog/v1/{slug}/greet"
        } else {
            "/catalog/v1/greet"
        };
        let mut locks = Vec::new();
        for (limits, rows) in [
            ("- provider: main", "| query.message | string | - | no |\n| path.slug | string | | yes |"),
            ("- body: 64kb\n- timeout: 3s\n- provider: main", "| path.slug | uuid | min: 1 | no |\n| query.message | string | - | no |\n| path.slug | string | - | no |"),
        ] {
            let body = format!(
                "## Route\n- method: GET\n- path: {path}\n## Access\n- public: {slug}\n- public-writes: false\n{tenant}## Pipelines\n(none)\n## Limits\n{limits}\n## Request\n| field | type | rules | required |\n| --- | --- | --- | --- |\n{rows}\n## Response\n- status: 200\n## Steps\n### s1 respond\n- op: respond\n- value: {{message: \"hello\"}}\n- source-lines: 7\n## Errors\n(none)\n",
                tenant = if slug { "- tenant: path.slug\n" } else { "" },
            );
            let (root, result) = compile_auth_feature_output(
                if slug { "none" } else { "jwt" }, "A greeting route.", "(none)", true, slug, Some(&body),
            ).await;
            result.unwrap_or_else(|err| panic!("{err}"));
            let bytes = fs::read(root.join("api/catalog/v1/greet/greet.lock.md")).unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            if slug {
                assert!(text.contains("| path.slug | string | - | yes |"));
            }
            let doc = parse_lock(text).unwrap();
            let limits = vibe::lock::find_section(&doc.sections, "Limits").unwrap();
            assert_eq!(
                limits.bullets.iter().map(|b| (b.key.as_str(), b.value.as_str())).collect::<Vec<_>>(),
                vec![("provider", "main"), ("timeout", "10s"), ("body", "1MB")],
            );
            let table = vibe::lock::find_section(&doc.sections, "Request").unwrap().table.as_ref().unwrap();
            let mut expected = vec![vec!["query.message", "string", "", "no"]];
            if slug {
                expected.insert(0, vec!["path.slug", "string", "", "yes"]);
            }
            assert_eq!(table.rows, expected);
            locks.push(bytes);
            fs::remove_dir_all(root).unwrap();
        }
        assert_eq!(locks[0], locks[1]);
    }
}

#[tokio::test]
async fn decision_165_cache_free_locks_and_audit_provenance() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for prose in [
        "回傳 hello world，唔需要驗證",
        "Return hello world, no auth",
    ] {
        let (root, result) = compile_auth_feature("none", prose, "(none)", false).await;
        result.unwrap_or_else(|err| panic!("{err}"));
        let rel = "api/catalog/v1/greet/greet.lock.md";
        let before = fs::read(root.join(rel)).unwrap();
        let doc = parse_lock(std::str::from_utf8(&before).unwrap()).unwrap();
        let body = vibe::lock::render_sections(&doc.sections);
        let calls = AtomicUsize::new(0);
        let adapter = FakeAdapter {
            responder: |request: &LlmRequest| {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                if request.system == vibe::compile::TRANSLATE_PROMPT {
                    Ok(format!(
                        "Return hello world without authentication. Variant {call}."
                    ))
                } else {
                    let errors = match call % 3 {
                        0 => "",
                        1 => "## Errors\n| code | status |\n| --- | --- |\n",
                        _ => "## Errors\n(none)\n",
                    };
                    Ok(format!(
                        "{}{errors}",
                        body.split("## Errors").next().unwrap()
                    ))
                }
            },
        };
        let mut config = LlmConfig {
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
        for _ in 0..3 {
            let _ = fs::remove_dir_all(root.join(".vibe/cache"));
            let output = regenerate_project(&CompileOptions {
                root: &root,
                llm: Some(&adapter),
                llm_config: Some(&config),
                check: false,
            })
            .await
            .unwrap_or_else(|err| panic!("{err}"));
            assert!(!output.report.to_string().contains("no Errors section"));
            assert_eq!(fs::read(root.join(rel)).unwrap(), before);
        }
        let record: vibe::buildrecord::BuildRecord =
            serde_json::from_slice(&fs::read(root.join(vibe::buildrecord::PATH)).unwrap()).unwrap();
        assert_eq!(record.translations.contains_key(rel), !prose.is_ascii());
        let (loaded, _) = vibe::compile::compile_in_memory(&root, Some(&adapter), Some(&config))
            .await
            .unwrap();
        let manifest = vibe::manifest::build_signed(
            &root,
            &loaded,
            None,
            &vibe::manifest::BuildInfo {
                release: true,
                version: "1.0.0".into(),
                commit: "a".repeat(40),
            },
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            manifest.provenance.translations.contains_key(rel),
            !prose.is_ascii()
        );
        // Legacy fields are accepted and dropped, with exactly one actionable warning.
        let old = std::str::from_utf8(&before).unwrap().replacen("security-items:", "translation: { prompt-version: translate-1, llm-provider: openai, model-snapshot: test, hash: sha256:0123 }\nsecurity-items:", 1);
        fs::write(root.join(rel), &old).unwrap();
        assert_eq!(
            vibe::lock::render_lock(&parse_lock(&old).unwrap()).as_bytes(),
            before
        );
        let result = validate_project(&ValidateOptions {
            root: &root,
            dev: true,
            deploy: false,
        });
        let report = result.unwrap_or_else(|report| report);
        assert_eq!(report.to_string().matches("lock contains a `translation` field, which is no longer written (decision 165); run `vibe build` to refresh it").count(), 1);
        // The old prompt's compiler hash must be stale under prompt-6.
        fs::write(
            root.join(rel),
            std::str::from_utf8(&before)
                .unwrap()
                .replace(&doc.meta.vibe_version, "sha256:old-prompt-hash"),
        )
        .unwrap();
        let result = validate_project(&ValidateOptions {
            root: &root,
            dev: true,
            deploy: false,
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("regenerate it with `vibe build`"));
        fs::write(root.join(rel), &before).unwrap();
        let source = root.join("api/catalog/v1/greet/greet.md");
        fs::write(
            &source,
            format!(
                "{}\nChanged greeting text.\n",
                fs::read_to_string(&source).unwrap()
            ),
        )
        .unwrap();
        let result = validate_project(&ValidateOptions {
            root: &root,
            dev: true,
            deploy: false,
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("source-hash does not match"));
        // Several natural-language files still yield a single sampling warning.
        fs::create_dir_all(root.join("api/catalog/v1/other")).unwrap();
        fs::write(
            root.join("api/catalog/v1/other/other.md"),
            "---\nauth: none\npublic: true\nwrites: false\n---\nAnother greeting, no auth.",
        )
        .unwrap();
        let multi = FakeAdapter {
            responder: |request: &LlmRequest| {
                if request.system == vibe::compile::TRANSLATE_PROMPT {
                    Ok("Return hello world without authentication.".into())
                } else if request.input.contains("api/catalog/v1/other/other.md") {
                    Ok(body.replace("/greet", "/other"))
                } else {
                    Ok(body.clone())
                }
            },
        };
        for send_sampling in [true, false] {
            config.send_sampling = send_sampling;
            let _ = fs::remove_dir_all(root.join(".vibe/cache"));
            let out = regenerate_project(&CompileOptions {
                root: &root,
                llm: Some(&multi),
                llm_config: Some(&config),
                check: false,
            })
            .await
            .unwrap_or_else(|err| panic!("{err}"));
            assert!(
                out.report
                    .to_string()
                    .matches("sampling parameters were not sent")
                    .count()
                    <= 1
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn decision_167_block_snap_and_validate() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (paragraph, count) in [
        (
            "提供公開端點，回傳 HTTP 狀態碼 `200`，以及固定嘅 JSON 回應：",
            5,
        ),
        ("Return HTTP status 200 and this fixed JSON response:", 3),
    ] {
        let (root, result) =
            compile_auth_feature("none", "A greeting route.", "(none)", false).await;
        result.unwrap_or_else(|err| panic!("{err}"));
        let rel = "api/catalog/v1/greet/greet.lock.md";
        let original = parse_lock(&fs::read_to_string(root.join(rel)).unwrap()).unwrap();
        let body = vibe::lock::render_sections(&original.sections);
        let source = format!("---\npublic: true\nwrites: false\nauth: none\n---\n# Hello World API\n\n{paragraph}\n\n```json\n{{\"message\":\"hello world\"}}\n```\n");
        fs::write(root.join("api/catalog/v1/greet/greet.md"), &source).unwrap();
        let calls = AtomicUsize::new(0);
        let translations = AtomicUsize::new(0);
        let adapter = FakeAdapter {
            responder: |request: &LlmRequest| {
                if request.system == vibe::compile::TRANSLATE_PROMPT {
                    let n = translations.fetch_add(1, Ordering::SeqCst);
                    Ok(format!(
                        "Return a public greeting. Translation variant {n}."
                    ))
                } else {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    let citation = ["8-12", "8-11", "8-10", "8 - 12", "8-11"][n % 5];
                    Ok(body.replace(
                        "- source-lines: 7-7",
                        &format!("- source-lines: {citation}"),
                    ))
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
        let mut before = None;
        for _ in 0..count {
            fs::remove_dir_all(root.join(".vibe/cache")).ok();
            regenerate_project(&CompileOptions {
                root: &root,
                llm: Some(&adapter),
                llm_config: Some(&config),
                check: false,
            })
            .await
            .unwrap_or_else(|err| panic!("{err}"));
            let bytes = fs::read(root.join(rel)).unwrap();
            assert!(std::str::from_utf8(&bytes)
                .unwrap()
                .contains("- source-lines: 8-12"));
            if let Some(previous) = &before {
                assert_eq!(&bytes, previous);
            }
            before = Some(bytes);
            validate_project(&ValidateOptions {
                root: &root,
                dev: true,
                deploy: false,
            })
            .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), count);
        assert_eq!(
            vibe::source_lines::snap_source_lines(&source, Some("9-12")),
            "10-12"
        );
        assert_eq!(
            vibe::source_lines::snap_source_lines(&source, Some("8")),
            "8-8"
        );
        let text = String::from_utf8(before.unwrap())
            .unwrap()
            .replace("- source-lines: 8-12", "- source-lines: 8-11");
        fs::write(root.join(rel), text).unwrap();
        let error = validate_project(&ValidateOptions {
            root: &root,
            dev: true,
            deploy: false,
        })
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("source-lines `8-11` must equal `8-12`; run `vibe build`"),
            "{error}"
        );
        assert!(error.contains("s1 respond"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }
}

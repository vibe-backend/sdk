#![cfg(feature = "sdk")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vibe::compile::{regenerate_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};
use vibe::lock::{find_section, parse_lock};
use vibe::validate::{validate_project, ValidateOptions};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}
fn project() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-create-{}", uuid::Uuid::new_v4()));
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop"),
        &root,
    );
    fs::create_dir_all(root.join("api/catalog/v1")).unwrap();
    for rel in ["api/catalog/_definition.md", "api/catalog/v1/_version.md"] {
        let path = root.join(rel);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(path, text.replace("- roles: [staff]", "")).unwrap();
    }
    root
}
fn source(root: &Path, text: &str) {
    fs::write(root.join("api/catalog/v1/example-create.md"), text).unwrap();
}
fn config() -> LlmConfig {
    LlmConfig {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        api_key: "test-placeholder".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        seed: 0,
        endpoint_host: "api.openai.com".into(),
        endpoint_private: false,
        send_sampling: false,
        api_style: "chat".into(),
    }
}
const GOOD: &str = vibe::prompt_guide::FEATURE_CREATE_EXAMPLE;

#[tokio::test]
async fn missing_create_step_and_invented_errors_are_repaired() {
    let root = project();
    source(
        &root,
        "# Create product\n\nCreate a product with sku and name using POST.\n",
    );
    let calls = AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |req: &LlmRequest| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(GOOD.split_once("## Steps").unwrap().0.to_string()
                    + "## Steps\n(none)\n## Errors\n(none)\n")
            } else if n == 1 {
                assert!(
                    req.input.contains("no step calls a writing method"),
                    "{}",
                    req.input
                );
                Ok(GOOD.replace("## Errors\n(none)", "## Errors\n| status | code | from |\n| --- | --- | --- |\n| 409 | INVENTED | INVENTED |"))
            } else {
                assert!(req.input.contains("unknown Errors code `INVENTED`"));
                Ok(GOOD.into())
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
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: false,
    })
    .unwrap_or_else(|e| panic!("{e}"));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn chinese_create_is_canonical_across_three_cache_free_compiles() {
    let root = project();
    let model_path = root.join("models/product/product.md");
    let model_output = fs::read_to_string(&model_path)
        .unwrap()
        .replace("required, min: 0", "default: 0, min: 0");
    fs::write(
        &model_path,
        "# product\n\n商品有 sku 同 name，可以新增、更新、刪除同列出。\n",
    )
    .unwrap();
    source(&root, "---\npublic: true\nauth: false\n---\n# 建立商品\n\n公開 POST 建立商品，唔使登入，sku 同 name 必填。 Use path /catalog/v1/products.\n");
    let public = GOOD
        .replace(
            "- public: false",
            "- public: true\n- public-writes: true\n- tenant: path.slug",
        )
        .replace("### auth\n- mode: jwt", "### -auth\n- removed: true")
        .replace(
            "/catalog/v1/example-products",
            "/catalog/v1/{slug}/example-products",
        );
    let list = public
        .replace("method: POST", "method: GET")
        .replace("- public-writes: true\n", "")
        .replace("- status: 201", "- status: 200")
        .replace("product.create", "product.list")
        .replace("- in: {sku: req.body.sku, name: req.body.name}", "- in: {}")
        .replace(
            "| body.sku | string | max: 40 | yes |\n| body.name | string | max: 80 | yes |\n",
            "",
        );
    fs::create_dir_all(root.join("api/catalog/v1")).unwrap();
    fs::write(
        root.join("api/catalog/v1/example-list.md"),
        format!("---\npublic: true\nauth: false\n---\n{list}"),
    )
    .unwrap();
    let mut previous = None;
    let mut warnings = 0;
    for (mapping, table, response) in [
        ("{name:req.body.name,sku:req.body.sku}", "product", "result"),
        (
            "{ sku : req . body . sku , name : req.body.name, }",
            "products",
            "transform.out",
        ),
        (
            "{sku: req.body.sku, name: req.body.name}",
            "PRODUCTS",
            "result",
        ),
    ] {
        let adapter = FakeAdapter {
            responder: |req: &LlmRequest| {
                if req.system == vibe::compile::TRANSLATE_PROMPT {
                    Ok("Create a product.".into())
                } else if req.input.contains("建立商品") {
                    Ok(public
                        .replace("{sku: req.body.sku, name: req.body.name}", mapping)
                        .replace("- value: transform.out", &format!("- value: {response}"))
                        .replace("- out: result\n## Steps", "- out:  result \n## Steps"))
                } else {
                    Ok(model_output.replace("table: products", &format!("table: {table}")))
                }
            },
        };
        let _ = fs::remove_dir_all(root.join(".vibe/cache"));
        let out = regenerate_project(&CompileOptions {
            root: &root,
            llm: Some(&adapter),
            llm_config: Some(&config()),
            check: false,
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        warnings += out
            .report
            .to_string()
            .matches("sampling parameters were not sent")
            .count();
        let feature =
            fs::read_to_string(root.join("api/catalog/v1/example-create.lock.md")).unwrap();
        let model = fs::read_to_string(root.join("models/product/product.lock.md")).unwrap();
        let doc = parse_lock(&feature).unwrap();
        assert!(doc
            .meta
            .security_items
            .iter()
            .any(|i| i.item == "write-sql"));
        assert_eq!(
            find_section(&doc.sections, "Transform").unwrap().get("in"),
            Some("{sku: req.body.sku, name: req.body.name}")
        );
        if let Some(old) = &previous {
            assert_eq!(old, &(feature.clone(), model.clone()));
        }
        previous = Some((feature, model));
    }
    assert!(warnings <= 1);
    validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: false,
    })
    .unwrap_or_else(|e| panic!("{e}"));
    let key = vibe::sign::create_dev_key(&root).unwrap();
    let manifest =
        vibe::manifest::manifest_from_project(&root, &key).unwrap_or_else(|e| panic!("{e}"));
    use axum::{body::Body, http::Request};
    use std::sync::Arc;
    use tower::ServiceExt;
    use vibe::runtime::{router, AppState, AuthConfig, MemoryExec, Slug};
    let app = router(Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth: AuthConfig::anonymous(),
        dev_slugs: None,
        slugs: [(
            "testslug1".into(),
            Slug {
                tenant_id: "tenant0001".into(),
                public_access: true,
                public_write: true,
            },
        )]
        .into(),
        rates: Default::default(),
        quotas: Default::default(),
        disabled: Default::default(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        trusted_proxies: vec![],
        dev_hints: true,
        challenge: None,
    }));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/catalog/v1/testslug1/example-products")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sku":"sku-1","name":"商品"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/catalog/v1/testslug1/example-products")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("sku-1"));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn structured_create_example_and_writes_contracts() {
    for action in ["create", "update", "delete"] {
        let root = project();
        let good = GOOD.replace("product.create", &format!("product.{action}"));
        source(&root, &format!("---\nwrites: false\n---\n{good}"));
        let err = regenerate_project(&CompileOptions {
            root: &root,
            llm: None,
            llm_config: None,
            check: false,
        })
        .await
        .err()
        .expect("writes conflict");
        assert!(
            err.to_string()
                .contains(&format!("calls product.{action}, which writes")),
            "{err}"
        );
        for front in ["", "---\nwrites: true\n---\n"] {
            source(&root, &format!("{front}{good}"));
            regenerate_project(&CompileOptions {
                root: &root,
                llm: None,
                llm_config: None,
                check: false,
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            let text =
                fs::read_to_string(root.join("api/catalog/v1/example-create.lock.md")).unwrap();
            assert!(parse_lock(&text)
                .unwrap()
                .meta
                .security_items
                .iter()
                .any(|i| i.item == "write-sql"));
            validate_project(&ValidateOptions {
                root: &root,
                dev: true,
                deploy: false,
            })
            .unwrap_or_else(|e| panic!("{e}"));
        }
        fs::remove_dir_all(root).unwrap();
    }
    let root = project();
    source(&root, GOOD);
    regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: false,
    })
    .unwrap_or_else(|e| panic!("{e}"));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn custom_writing_method_and_declared_errors_are_inherited() {
    let root = project();
    fs::create_dir_all(root.join("models/inventory/methods")).unwrap();
    fs::write(
        root.join("models/inventory/inventory.md"),
        fs::read_to_string(root.join("models/product/product.md"))
            .unwrap()
            .replace("table: products", "table: inventories"),
    )
    .unwrap();
    fs::write(root.join("models/inventory/methods/bump-stock.md"), "---\nwrites: true\n---\n## Signature\n- writes: true\n- errors: [OUT_OF_STOCK]\n- capabilities: []\n## Calls\n(none)\n## Steps\n### s1\n- op: query\n- sql: UPDATE products SET price_cents = price_cents + 1 WHERE id = $1\n- params: [input.id]\n- writes: true\n- scope: pk\n- out: row\n### s2\n- op: respond\n- value: row\n").unwrap();
    let body = GOOD.replace("product.create", "inventory.bump-stock").replace("## Errors\n(none)", "## Errors\n| status | code | from |\n| --- | --- | --- |\n| 409 | OUT_OF_STOCK | OUT_OF_STOCK |");
    source(&root, &format!("---\nwrites: true\n---\n{body}"));
    regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let text = fs::read_to_string(root.join("api/catalog/v1/example-create.lock.md")).unwrap();
    assert!(parse_lock(&text)
        .unwrap()
        .meta
        .security_items
        .iter()
        .any(|i| i.item == "write-sql"));
    validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: false,
    })
    .unwrap_or_else(|e| panic!("{e}"));
    // English natural compilation exposes the callee's contract and stays byte-identical.
    source(&root, "---\nwrites: true\n---\n# Update stock\nCall inventory.bump-stock and map OUT_OF_STOCK to HTTP 409.\n");
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            assert!(request
                .input
                .contains("callable=inventory.bump-stock errors=[OUT_OF_STOCK]"));
            Ok(body.clone())
        },
    };
    let mut previous = None;
    for _ in 0..2 {
        let _ = fs::remove_dir_all(root.join(".vibe/cache"));
        regenerate_project(&CompileOptions {
            root: &root,
            llm: Some(&adapter),
            llm_config: Some(&config()),
            check: false,
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let bytes = fs::read(root.join("api/catalog/v1/example-create.lock.md")).unwrap();
        if let Some(before) = previous {
            assert_eq!(bytes, before);
        }
        previous = Some(bytes);
    }
    source(&root, &format!("---\nwrites: false\n---\n{body}"));
    let err = regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .expect("writes conflict");
    assert!(
        err.to_string()
            .contains("step s1 calls inventory.bump-stock, which writes"),
        "{err}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn duplicate_transform_keys_and_true_without_steps_fail() {
    let root = project();
    source(
        &root,
        &GOOD.replace("name: req.body.name", "sku: req.body.name"),
    );
    let err = regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .expect("duplicate key");
    assert!(
        err.to_string().contains("duplicate Transform in key `sku`"),
        "{err}"
    );
    source(
        &root,
        &("---\nwrites: true\n---\n".to_string()
            + GOOD.split_once("## Steps").unwrap().0
            + "## Steps\n(none)\n## Errors\n(none)\n"),
    );
    let err = regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .expect("missing step");
    assert!(
        err.to_string()
            .contains("frontmatter `writes: true` but no step calls a writing method"),
        "{err}"
    );
    source(&root, &GOOD.replace("## Errors\n(none)", "## Errors\n| status | code | from |\n| --- | --- | --- |\n| 409 | INVENTED | INVENTED |"));
    let err = regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .err()
    .expect("unknown code");
    assert!(
        err.to_string().contains("unknown Errors code `INVENTED`"),
        "{err}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn missing_list_is_repaired_and_constants_are_preserved() {
    for (prose, path, target) in [
        (
            "List products using GET.",
            "/catalog/v1/example-products",
            "product.list",
        ),
        (
            "列出商品，GET。",
            "/catalog/v1/example-products",
            "product.list",
        ),
        (
            "Show a product using GET.",
            "/catalog/v1/example-products/{id}",
            "product.get",
        ),
    ] {
        let root = project();
        fs::remove_dir_all(root.join("models/cart-item")).unwrap();
        fs::remove_dir_all(root.join("api/shop")).unwrap();
        source(&root, &format!("{prose} Use path {path}."));
        let good = vibe::prompt_guide::feature_list_example()
            .replace("/catalog/v1/example-products", path)
            .replace("product.list", target);
        let calls = AtomicUsize::new(0);
        let adapter = FakeAdapter {
            responder: |req: &LlmRequest| {
                if req.system == vibe::compile::TRANSLATE_PROMPT {
                    return Ok("List products using GET.".into());
                }
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(good.split_once("## Transform").unwrap().0.to_string()
                        + "## Transform\n(none)\n## Steps\n(none)\n## Errors\n(none)\n")
                } else {
                    assert!(req.input.contains("the feature has no method step and no constant response, so it always returns null"));
                    assert!(req.input.contains(&format!("target {target}")));
                    Ok(good.clone())
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
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let lock = fs::read_to_string(root.join("api/catalog/v1/example-create.lock.md")).unwrap();
        assert!(lock.contains(&format!("- target: {target}")));
        fs::remove_dir_all(root).unwrap();
    }
    for constant in [
        r#"{"message":"hello world"}"#,
        r#""hello world""#,
        "42",
        "true",
        "false",
        "null",
        r#"{message: "hello world"}"#,
    ] {
        for respond in [false, true] {
            let root = project();
            source(&root, "Hello world using GET with a constant response.");
            let prefix = vibe::prompt_guide::feature_list_example()
                .split_once("## Transform")
                .unwrap()
                .0
                .to_string();
            let body = if respond {
                format!("{prefix}## Transform\n(none)\n## Steps\n### s1\n- op: respond\n- value: {constant}\n- status: 200\n## Errors\n(none)\n")
            } else {
                format!("{prefix}## Transform\n- out: {constant}\n## Steps\n(none)\n## Errors\n(none)\n")
            };
            let calls = AtomicUsize::new(0);
            let adapter = FakeAdapter {
                responder: |_: &LlmRequest| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(body.clone())
                },
            };
            regenerate_project(&CompileOptions {
                root: &root,
                llm: Some(&adapter),
                llm_config: Some(&config()),
                check: false,
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{constant}");
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[tokio::test]
async fn validate_rejects_source_writes_false_with_create_step() {
    let root = project();
    source(&root, GOOD);
    regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let path = root.join("api/catalog/v1/example-create.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("---\nwrites: false\n---\n{text}")).unwrap();
    let err = validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: false,
    })
    .expect_err("source writes conflict with create step");
    assert!(
        err.to_string()
            .contains("calls product.create, which writes"),
        "{err}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn validate_rejects_hand_edited_lock_unknown_errors_code() {
    let root = project();
    source(&root, GOOD);
    regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let path = root.join("api/catalog/v1/example-create.lock.md");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("## Errors\n(none)"), "{text}");
    fs::write(&path, text.replace("## Errors\n(none)", "## Errors\n| status | code | from |\n| --- | --- | --- |\n| 409 | INVENTED | INVENTED |" )).unwrap();
    let err = validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: false,
    })
    .expect_err("hand-edited unknown Errors code");
    assert!(err.to_string().contains("unknown Errors code"), "{err}");
    fs::remove_dir_all(root).unwrap();
}

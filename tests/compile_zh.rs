use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;
use vibe::compile::{regenerate_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};
use vibe::lock::{find_section, parse_lock};
use vibe::runtime::{router, AppState, AuthConfig, MemoryExec, Slug};
use vibe::validate::{validate_project, ValidateOptions};

const MODEL: &str = include_str!("fixtures/zh/model-output.md");
const FEATURE: &str = include_str!("fixtures/zh/create-output.md");
const INDEX: &str =
    "CREATE UNIQUE INDEX products_pair ON products (tenant_id, sku, name) WHERE deleted_at IS NULL";

fn fixture(structured: bool) -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-compile-zh-{}", uuid::Uuid::new_v4()));
    for dir in ["providers", "models/product", "migrations/main"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(
        root.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    fs::write(root.join("models/product/product.md"), if structured { MODEL } else { "# product\n\n商品用 main 資料庫嘅 products 表，同一租戶共用（ownership: tenant-wide）。sku 同 name 必填，最多分別 40 同 120 字；兩欄一齊先係唯一鍵。可以建立同更新商品。\n" }).unwrap();
    migration(&root, INDEX);
    root
}

fn migration(root: &Path, sql: &str) {
    fs::write(root.join("migrations/main/0001-products.md"), format!("## Target\n- provider: main\n- seq: 0001\n## Up\n### s1\n- sql: `{sql}`\n## Verify\n(none)\n## Data\n(none)\n")).unwrap();
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
        send_sampling: true,
        api_style: "chat".into(),
    }
}

async fn compile(root: &Path, output: &str) -> Result<vibe::CompileOutput, vibe::Report> {
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            if request.system == vibe::compile::TRANSLATE_PROMPT {
                Ok("Product fields and create endpoint.".into())
            } else if request.input.contains("建立商品 API") {
                Ok(FEATURE.into())
            } else {
                Ok(output.into())
            }
        },
    };
    regenerate_project(&CompileOptions {
        root,
        llm: Some(&adapter),
        llm_config: Some(&config()),
        check: false,
    })
    .await
}

fn validate(root: &Path) -> Result<vibe::Report, vibe::Report> {
    validate_project(&ValidateOptions {
        root,
        deploy: false,
        dev: true,
    })
}

#[tokio::test]
async fn chinese_headers_aliases_pipes_and_determinism() {
    for structured in [false, true] {
        let root = fixture(structured);
        compile(&root, MODEL)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let path = root.join("models/product/product.lock.md");
        let first = fs::read_to_string(&path).unwrap();
        let doc = parse_lock(&first).unwrap();
        if structured {
            assert_eq!(doc.meta.source_hash, vibe::hashutil::source_hash(MODEL));
        }
        let fields = find_section(&doc.sections, "Fields")
            .unwrap()
            .table
            .as_ref()
            .unwrap();
        assert_eq!(fields.headers, ["name", "type", "rules", "system"]);
        assert_eq!(fields.rows[1], ["sku", "string", "required, max: 40", "no"]);
        assert_eq!(
            fields.rows[2],
            ["name", "string", "required, max: 120", "no"]
        );
        validate(&root).unwrap_or_else(|e| panic!("{e}"));
        if !structured {
            fs::remove_dir_all(root.join(".vibe/cache")).unwrap();
            let alternate = MODEL
                .replace(
                    "｜ 必填 ｜ 規則 ｜ 類型 ｜ 欄位名 ｜",
                    "| 名稱 | 型別 | 規則 | 必須 |",
                )
                .replace(
                    "｜ 是 ｜ max_length: 40 ｜ string ｜ sku ｜",
                    "| sku | string | maxLength: 40 | yes |",
                )
                .replace(
                    "｜ ✓ ｜ maxLength: 120 ｜ string ｜ name ｜",
                    "| name | string | max-length: 120 | true |",
                )
                .replace('｜', "|")
                .replace("[sku, name]", "[sku,name]");
            compile(&root, &alternate)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(first, fs::read_to_string(&path).unwrap());
        }
        let alias_lock = first.replace("max: 40", "max_length: 40");
        fs::write(&path, alias_lock).unwrap();
        let error = validate(&root).unwrap_err().to_string();
        assert!(
            error.contains("max_length") && error.contains("use max:"),
            "{error}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn unknown_headers_and_constraints_require_exact_migration() {
    let root = fixture(true);
    let path = root.join("models/product/product.md");
    fs::write(&path, MODEL.replace("欄位名", "奇怪欄")).unwrap();
    let error = compile(&root, MODEL)
        .await
        .err()
        .expect("compile must fail")
        .to_string();
    assert!(
        error.contains("奇怪欄") && error.contains("name/欄位"),
        "{error}"
    );
    fs::write(&path, MODEL).unwrap();
    for sql in ["CREATE TABLE products (id uuid)", "CREATE UNIQUE INDEX u ON products (tenant_id, sku)", "CREATE UNIQUE INDEX u ON products (tenant_id, name, sku)", "CREATE UNIQUE INDEX u ON products (tenant_id, sku, name) WHERE name IS NOT NULL", "CREATE UNIQUE INDEX u ON products (tenant_id, sku, name) WHERE deleted_at IS NULL AND name IS NOT NULL"] {
        migration(&root, sql);
        let error = compile(&root, MODEL).await.err().expect("compile must fail").to_string();
        assert!(error.contains("product") && error.contains("tenant_id, sku, name"), "{error}");
    }
    migration(&root, INDEX);
    compile(&root, MODEL)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    validate(&root).unwrap_or_else(|e| panic!("{e}"));
    fs::remove_file(root.join("migrations/main/0001-products.lock.md")).unwrap();
    let error = validate(&root).unwrap_err().to_string();
    assert!(
        error.contains("requires matching unique index") && error.contains("tenant_id, sku, name"),
        "{error}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn chinese_create_api_returns_private_unique_violation_in_memory() {
    let root = fixture(false);
    fs::create_dir_all(root.join("api/catalog/v1/create-product")).unwrap();
    fs::write(root.join("api/catalog/_definition.md"), "## Providers\n- allowlist: [main]\n- default: main\n## Pipelines\n(none)\n## Defaults\n(none)\n").unwrap();
    fs::write(
        root.join("api/catalog/v1/_version.md"),
        "## Pipelines\n(none)\n## Defaults\n(none)\n## Lifecycle\n(none)\n",
    )
    .unwrap();
    fs::write(root.join("api/catalog/v1/create-product/create-product.md"), "---\npublic: true\nauth: false\nwrites: true\n---\n# 建立商品 API\n\n公開 POST /catalog/v1/{slug}/products，唔使登入。公開寫入，同一租戶可以建立商品，用 product.create，sku 同 name 必填。\n").unwrap();
    compile(&root, MODEL)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let key = vibe::sign::create_dev_key(&root).unwrap();
    let manifest =
        vibe::manifest::manifest_from_project(&root, &key).unwrap_or_else(|e| panic!("{e}"));
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
    for expected in [StatusCode::CREATED, StatusCode::CONFLICT] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/catalog/v1/testslug1/products")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"sku": "private-duplicate-sku", "name": "private-duplicate-name"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(status, expected, "{body}");
        if expected == StatusCode::CONFLICT {
            assert!(body.contains("UNIQUE_VIOLATION"), "{body}");
            assert!(!body.contains("private-duplicate"), "{body}");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#![cfg(feature = "sdk")]

//! #37/#92: Traditional Chinese sample-shop sources compile end to end with a
//! fake LLM, and the keyword checks accept CJK wording (共享, owner-key) and
//! writes inherited from called methods.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vibe::compile::{regenerate_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};
use vibe::lock::parse_lock;
use vibe::validate::{validate_project, ValidateOptions};

const PRODUCT: &str = "## Table
- provider: main
- table: products
- ownership: tenant-wide
## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| slug | string | required, unique, max: 80 | no |
| name | string | required, max: 120 | no |
| price_cents | integer | required, min: 0 | no |
| stock | integer | required, min: 0, default: 0 | no |
| active | boolean | required, default: true | no |
## CRUD
| method | options |
| --- | --- |
| create | - |
| get | - |
| list | page-size: 20, max-page-size: 100, sort: [created_at, price_cents], filter: [active] |
## Policies
(none)
";

const CART_ITEM: &str = "## Table
- provider: main
- table: cart_items
- owner-key: user_id
## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| user_id | uuid | required | no |
| product_id | uuid | required | no |
| sku_id | uuid | nullable | no |
| quantity | integer | required, min: 1, max: 99 | no |
## CRUD
| method | options |
| --- | --- |
| create | - |
| get | - |
| list | page-size: 50, max-page-size: 100, sort: [created_at] |
## Policies
(none)
";

const ADD: &str = "## Signature
- input: { product_id: { type: uuid, required: true }, sku_id: { type: uuid, required: false }, quantity: { type: integer, min: 1, max: 99, required: true } }
- output: { id: uuid, product_id: uuid, sku_id: uuid, quantity: integer }
- errors: [PRODUCT_NOT_FOUND]
- writes: true
- capabilities: []
## Calls
- methods: []
## Steps
### s1 query
- op: query
- sql: `INSERT INTO cart_items (user_id, product_id, sku_id, quantity) VALUES ($1, $2, $3, $4) RETURNING id, product_id, sku_id, quantity`
- params: [ctx.user.id, input.product_id, input.sku_id, input.quantity]
- writes: true
- out: row
- source-lines: 5
### s2 respond
- op: respond
- value: row
- source-lines: 6
";

const FEATURE: &str = "## Route
- method: POST
- path: /shop/v1/cart-items
## Access
- public: false
## Pipelines
(none)
## Limits
- provider: main
- timeout: 10s
- body: 64kb
## Request
| field | type | rules | required |
| --- | --- | --- | --- |
| body.product_id | uuid | - | yes |
| body.sku_id | uuid | - | no |
| body.quantity | integer | min: 1, max: 99 | yes |
## Response
- status: 200
## Transform
- in: {product_id: req.body.product_id, sku_id: req.body.sku_id, quantity: req.body.quantity}
- out: result
## Steps
### s1 method
- op: method
- target: cart-item.add
- args: transform.in
- out: result
- source-lines: 5
### s2 respond
- op: respond
- value: transform.out
- status: 200
- source-lines: 8
## Errors
| status | code | from |
| --- | --- | --- |
| 404 | PRODUCT_NOT_FOUND | PRODUCT_NOT_FOUND |
| 422 | VALIDATION_FAILED | VALIDATION_FAILED |
";

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.file_name() != "README.md" {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vibe-zh-hant-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zh-hant-shop"),
        &dir,
    );
    dir
}

/// English normalizations a translator would return (keyword check only).
fn translation(input: &str) -> &'static str {
    if input.contains("商品目錄") {
        "Product catalog stored in the products table of the main database. Data shared by the whole store (tenant)."
    } else if input.contains("購物車中的品項") {
        "Items in the user's cart, stored in the cart_items table. owner-key is user_id: users can only read and write their own items."
    } else if input.contains("把商品") {
        "Add a product (or its variant) to the current user's cart; insert a row or update the quantity. This method writes data (writes: true)."
    } else if input.contains("加入購物車") {
        "Add to cart. POST /shop/v1/cart-items. Requires JWT. Calls cart-item.add, then cart-item.summary."
    } else {
        "unexpected"
    }
}

fn responder(request: &LlmRequest) -> Result<String, String> {
    if request.system == vibe::compile::TRANSLATE_PROMPT {
        return Ok(translation(&request.input).to_string());
    }
    let id = request
        .input
        .lines()
        .find_map(|l| l.strip_prefix("ID: "))
        .unwrap_or("");
    Ok(match id {
        "product" => PRODUCT,
        "cart-item" => CART_ITEM,
        "cart-item/methods/add" => ADD,
        "shop/v1/add-cart-item" => FEATURE,
        other => return Err(format!("unexpected source {other}")),
    }
    .to_string())
}

#[tokio::test]
async fn zh_hant_sample_shop_compiles_and_validates() {
    let root = fixture();
    let adapter = FakeAdapter { responder };
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
    let out = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(out.files.len(), 7, "{:?}", out.files);
    // (1) product: tenant-wide ownership authorized by 共享 in the source.
    let product =
        parse_lock(&fs::read_to_string(root.join("models/product/product.lock.md")).unwrap())
            .unwrap();
    assert!(product
        .meta
        .security_items
        .iter()
        .any(|i| i.item == "ownership"));
    // (2) add-cart-item: write-sql inherited from cart-item.add.
    let feature =
        parse_lock(&fs::read_to_string(root.join("api/shop/v1/add-cart-item.lock.md")).unwrap())
            .unwrap();
    assert!(feature
        .meta
        .security_items
        .iter()
        .any(|i| i.item == "write-sql"));
    validate_project(&ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
    })
    .unwrap_or_else(|err| panic!("{err}"));
    // Every Chinese natural source was translated once and cached.
    assert_eq!(
        fs::read_dir(root.join(".vibe/cache/translate"))
            .unwrap()
            .count(),
        4
    );
    // D109/165: the build record stamps prompt version, provider/model and
    // translation hash; neither stamp nor text is in the lock.
    let text = translation("商品目錄");
    let raw = fs::read_to_string(root.join("models/product/product.lock.md")).unwrap();
    assert!(!raw.contains(text), "translated text leaked into the lock");
    assert!(!raw.contains("Product catalog"), "{raw}");
    assert!(product.meta.translation.is_none());
    assert!(!raw.contains("translation:"));
    let record: vibe::buildrecord::BuildRecord =
        serde_json::from_str(&fs::read_to_string(root.join(vibe::buildrecord::PATH)).unwrap())
            .unwrap();
    let stamp = &record.translations["models/product/product.lock.md"];
    assert_eq!(
        stamp.prompt_version,
        vibe::compile::TRANSLATE_PROMPT_VERSION
    );
    assert_eq!(stamp.llm_provider, "openai");
    assert_eq!(stamp.model_snapshot, "gpt-test-2026-01-01");
    assert_eq!(stamp.hash, vibe::hashutil::sha256_prefixed(text.as_bytes()));
    // The full text goes to the audit hook for the release audit artifact.
    let audit = fs::read_to_string(
        root.join(vibe::compile::TRANSLATION_AUDIT_DIR)
            .join("product.txt"),
    )
    .unwrap();
    assert!(
        audit.contains(text) && audit.contains(&stamp.hash),
        "{audit}"
    );
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
async fn translation_failure_fails_the_compile() {
    let root = fixture();
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            if request.system == vibe::compile::TRANSLATE_PROMPT {
                Err("http 503".into())
            } else {
                responder(request)
            }
        },
    };
    let config = config();
    let Err(report) = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    else {
        panic!("must fail");
    };
    let text = report.to_string();
    assert!(
        text.contains("cannot translate the source for the security keyword check"),
        "{text}"
    );
    assert!(!root.join("models/product/product.lock.md").exists());
}

#[tokio::test]
async fn feature_writes_declaration_is_optional() {
    let root = fixture();
    let f = root.join("api/shop/v1/add-cart-item.md");
    let text = fs::read_to_string(&f).unwrap();
    fs::write(&f, text.replace("writes: true\n", "")).unwrap();
    let adapter = FakeAdapter { responder };
    regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config()),
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    let lock =
        vibe::lock::parse_lock(&fs::read_to_string(f.with_extension("lock.md")).unwrap()).unwrap();
    assert!(lock
        .meta
        .security_items
        .iter()
        .any(|i| i.item == "write-sql"));
    fs::remove_dir_all(root).unwrap();
}

fn responder_keeps_chinese(request: &LlmRequest) -> Result<String, String> {
    if request.system == vibe::compile::TRANSLATE_PROMPT {
        // A translator that keeps Chinese names and terms in its output.
        return Ok(format!(
            "{} 「商品」「購物車」",
            translation(&request.input)
        ));
    }
    responder(request)
}

/// #92: Chinese text left in the translation must not fail the source
/// (models, methods and `_definition.md` stay in Chinese by design).
#[tokio::test]
async fn chinese_left_in_translation_is_not_an_error() {
    let root = fixture();
    let adapter = FakeAdapter {
        responder: responder_keeps_chinese,
    };
    let config = config();
    let out = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(out.files.len(), 7, "{:?}", out.files);
    validate_project(&ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
    })
    .unwrap_or_else(|err| panic!("{err}"));
}

async fn check_source_keywords_text(translated_keyword: bool, replacement: &str) {
    let root = fixture();
    let path = root.join("models/product/product.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        text.replace("屬於整個商店（租戶）共享的資料。", replacement),
    )
    .unwrap();
    let adapter = FakeAdapter {
        responder: move |request: &LlmRequest| {
            if request.system == vibe::compile::TRANSLATE_PROMPT
                && request.input.contains("商品目錄")
            {
                Ok(if translated_keyword {
                    "Product catalog shared by the whole store."
                } else {
                    "Product catalog with names and prices."
                }
                .into())
            } else {
                responder(request)
            }
        },
    };
    let config = config();
    let report = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .err()
    .expect("unsupported claim must fail");
    assert!(
        report
            .to_string()
            .contains("lock declares ownership but the source does not mention it"),
        "{report}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn keyword_check_uses_source() {
    check_source_keywords_text(false, "商品有名稱與價格。").await;
}

#[tokio::test]
async fn keyword_present_only_in_translation_fails() {
    check_source_keywords_text(true, "商品有名稱與價格。").await;
}

#[tokio::test]
async fn supported_chinese_source_compiles() {
    let root = fixture();
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            if request.system == vibe::compile::TRANSLATE_PROMPT {
                Ok("商品與購物車。".into())
            } else {
                responder(request)
            }
        },
    };
    let config = config();
    let out = regenerate_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .unwrap_or_else(|report| panic!("{report}"));
    assert_eq!(out.files.len(), 7);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn negated_and_code_keywords_do_not_support_ownership() {
    for text in [
        "唔共享",
        "不共享",
        "冇共享",
        "無共享",
        "毋共享",
        "非共享",
        "not shared",
        "no shared",
        "never shared",
        "\n```\n共享 shared\n```\n",
        "`共享` `shared`",
        "\n~~~\n共享 shared\n~~~\n",
        "``共享 ` shared``",
    ] {
        check_source_keywords_text(true, text).await;
    }
}

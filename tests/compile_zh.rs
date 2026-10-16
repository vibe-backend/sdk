//! #94: Chinese sources with a mock LLM that reproduces the observed drift.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vibe::compile::{compile_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};
use vibe::lock::{find_section, parse_lock};

/// Chinese headers, columns reordered, a separate 必填 column, `max_length`,
/// and full-width pipes on the CRUD table.
const PRODUCT_DRIFT: &str = "## Table
- provider: main
- table: products
## Fields
| 欄位 | 規則 | 類型 | 必填 |
| --- | --- | --- | --- |
| sku | unique, max_length: 40 | string | 是 |
| name | maxLength: 80 | string | 是 |
| price_cents | min: 0 | integer | 是 |
## CRUD
｜ method ｜ options ｜
｜ --- ｜ --- ｜
｜ get ｜ - ｜
｜ list ｜ page-size: 20 ｜
## Policies
(none)
";

const CART_PER_COLUMN_UNIQUE: &str = "## Table
- provider: main
- table: cart_items
## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| cart_id | uuid | required, unique | no |
| product_id | uuid | required, unique | no |
| qty | integer | min: 1, max: 99 | no |
## CRUD
| method | options |
| --- | --- |
| create | - |
## Policies
(none)
";

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vibe-zh-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zh"),
        &dir,
    );
    dir
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
        api_style: "chat".into(),
        send_sampling: true,
    }
}

fn responder(request: &LlmRequest) -> Result<String, String> {
    // The keyword check reads an English translation (#37); answer it.
    if request.system == vibe::compile::TRANSLATE_PROMPT {
        return Ok(if request.input.contains("cart_id") {
            "# cart-item\n\nCart item. Fields: cart_id (uuid, required), product_id (uuid, required), qty (integer, 1 to 99).\ncart_id and product_id are unique together (one cart cannot have two rows for the same product).\nProvides create and list.\n"
        } else {
            "# product\n\nProduct. Fields: sku (string, required, unique, at most 40 chars), name (string, required, at most 80 chars), price_cents (integer, required, at least 0).\nProvides get and list.\n"
        }
        .into());
    }
    assert!(request.system.contains("最長/最多/不超過 N 字"));
    if request.input.contains("ID: product\n") {
        Ok(PRODUCT_DRIFT.into())
    } else {
        Ok(CART_PER_COLUMN_UNIQUE.into())
    }
}

#[tokio::test]
async fn chinese_fields_are_realigned_and_length_rules_normalized() {
    let root = fixture();
    fs::remove_dir_all(root.join("models/cart-item")).unwrap();
    let adapter = FakeAdapter { responder };
    let config = config();
    compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    let doc = parse_lock(&fs::read_to_string(root.join("models/product/product.lock.md")).unwrap())
        .unwrap();
    let table = find_section(&doc.sections, "Fields")
        .unwrap()
        .table
        .clone()
        .unwrap();
    assert_eq!(table.headers, ["name", "type", "rules", "system"]);
    let row = |name: &str| table.rows.iter().find(|r| r[0] == name).unwrap().clone();
    assert_eq!(
        row("sku"),
        ["sku", "string", "required, unique, max: 40", "no"]
    );
    assert_eq!(row("name"), ["name", "string", "required, max: 80", "no"]);
    assert_eq!(row("price_cents")[1], "integer");
    let crud = find_section(&doc.sections, "CRUD").unwrap();
    assert_eq!(crud.table.as_ref().unwrap().rows.len(), 2);
}

#[tokio::test]
async fn composite_unique_is_not_compiled_as_per_column_unique() {
    let root = fixture();
    let adapter = FakeAdapter { responder };
    let config = config();
    let Err(report) = compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    else {
        panic!("per-column unique must be rejected");
    };
    let text = report.to_string();
    assert!(text.contains("composite unique"), "{text}");
    assert!(text.contains("cart_id, product_id"), "{text}");
}

// ---- D107: composite unique through `## Constraints` ----

const CART_CONSTRAINTS: &str = "## Table
- provider: main
- table: cart_items
## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| cart_id | uuid | required | no |
| product_id | uuid | required | no |
| qty | integer | min: 1, max: 99 | no |
## Constraints
- unique: [cart_id, product_id]
## CRUD
| method | options |
| --- | --- |
| create | - |
## Policies
(none)
";

const CART_NO_CONSTRAINTS: &str = "## Table
- provider: main
- table: cart_items
## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| cart_id | uuid | required | no |
| product_id | uuid | required | no |
| qty | integer | min: 1, max: 99 | no |
## CRUD
| method | options |
| --- | --- |
| create | - |
## Policies
(none)
";

const PRODUCT_OK: &str = "## Table
- provider: main
- table: products
## Fields
| name | type | rules | system |
| --- | --- | --- | --- |
| sku | string | required, unique, max: 40 | no |
| name | string | required, max: 80 | no |
| price_cents | integer | required, min: 0 | no |
## CRUD
| method | options |
| --- | --- |
| get | - |
| list | page-size: 20 |
## Policies
(none)
";

const UNIQUE_MIGRATION: &str = "# cart items unique

## Target

- provider: main
- seq: 0001

## Up

### s1

- sql: CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS cart_items_cart_product ON cart_items (tenant_id, cart_id, product_id)

## Verify

(none)

## Data

(none)
";

fn d107_responder(request: &LlmRequest, cart: &'static str) -> Result<String, String> {
    if request.system == vibe::compile::TRANSLATE_PROMPT {
        return responder(request);
    }
    Ok(if request.input.contains("ID: product\n") {
        PRODUCT_OK
    } else {
        cart
    }
    .into())
}

fn with_constraints(request: &LlmRequest) -> Result<String, String> {
    d107_responder(request, CART_CONSTRAINTS)
}

fn without_constraints(request: &LlmRequest) -> Result<String, String> {
    d107_responder(request, CART_NO_CONSTRAINTS)
}

fn validate(root: &Path) -> Result<(), String> {
    vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        root,
        deploy: false,
        verify_key: None,
    })
    .map(|_| ())
    .map_err(|report| report.to_string())
}

#[tokio::test]
async fn composite_unique_compiles_to_constraints_and_needs_a_unique_index_migration() {
    let root = fixture();
    let adapter = FakeAdapter {
        responder: with_constraints,
    };
    let config = config();
    let opts = CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    };
    compile_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let raw = fs::read_to_string(root.join("models/cart-item/cart-item.lock.md")).unwrap();
    assert!(
        raw.contains("## Constraints\n- unique: [cart_id, product_id]\n"),
        "{raw}"
    );
    let doc = parse_lock(&raw).unwrap();
    let constraints = vibe::lock::model_unique_constraints(&doc.sections).unwrap();
    assert_eq!(constraints, vec![vec!["cart_id", "product_id"]]);
    assert_eq!(
        vibe::lock::effective_unique_columns(&constraints[0]),
        ["tenant_id", "cart_id", "product_id"]
    );

    // No migration creating the index: the build fails.
    let err = validate(&root).unwrap_err();
    assert!(
        err.contains("unique [cart_id, product_id] has no migration")
            && err.contains("(tenant_id, cart_id, product_id)"),
        "{err}"
    );

    // An index without tenant_id first does not match either.
    let dir = root.join("migrations/main");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("0001-cart-items-unique.md"),
        UNIQUE_MIGRATION.replace("(tenant_id, cart_id, product_id)", "(cart_id, product_id)"),
    )
    .unwrap();
    compile_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    assert!(validate(&root).unwrap_err().contains("has no migration"));

    // The matching migration makes the build pass.
    fs::write(dir.join("0001-cart-items-unique.md"), UNIQUE_MIGRATION).unwrap();
    compile_project(&opts)
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    validate(&root).unwrap_or_else(|err| panic!("{err}"));
}

#[tokio::test]
async fn composite_unique_in_the_english_text_requires_a_constraints_entry() {
    let root = fixture();
    let adapter = FakeAdapter {
        responder: without_constraints,
    };
    let config = config();
    let Err(report) = compile_project(&CompileOptions {
        root: &root,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    })
    .await
    else {
        panic!("a composite unique without ## Constraints must be rejected");
    };
    let text = report.to_string();
    assert!(text.contains("## Constraints"), "{text}");
}

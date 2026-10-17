use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vibe::compile::{regenerate_project, CompileOptions, TRANSLATE_PROMPT};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};
use vibe::validate::{validate_project, ValidateOptions};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}
fn project() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-no-env-{}", uuid::Uuid::new_v4()));
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop"),
        &root,
    );
    fs::write(
        root.join("models/product/product.md"),
        "# product\n\nCreate, update and delete products with sku, name and price_cents. 商品可以建立、更新同刪除。\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("models/extra")).unwrap();
    fs::write(
        root.join("models/extra/extra.md"),
        "# extra\n\nCreate, update and delete extra records with sku, name and price_cents.\n",
    )
    .unwrap();
    root
}
fn config() -> LlmConfig {
    LlmConfig {
        provider: "probe-provider-74bc".into(),
        base_url: "https://probe-host-7f3a91.example.test/v1".into(),
        endpoint_host: "probe-host-7f3a91.example.test".into(),
        endpoint_private: true,
        model: "probe-model-zz91q".into(),
        api_key: "sk-probe-dummy-5c1e88".into(),
        seed: 913,
        send_sampling: true,
        api_style: "chat".into(),
        timeout: Duration::from_secs(1),
        max_retries: 0,
    }
}
fn locks(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, result: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, result);
            } else if path.to_string_lossy().ends_with(".lock.md") {
                result.insert(
                    path.strip_prefix(root).unwrap().to_string_lossy().into(),
                    fs::read_to_string(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}
async fn compile(root: &Path, config: &LlmConfig) {
    let calls = AtomicUsize::new(0);
    let translations = AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |request: &LlmRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            if request.system == TRANSLATE_PROMPT {
                translations.fetch_add(1, Ordering::SeqCst);
                Ok("Create, update and delete products with sku, name and price_cents.".into())
            } else {
                let output = include_str!("fixtures/shop/models/product/product.md");
                Ok(if request.input.contains("# extra") {
                    output.replace("products", "extras")
                } else {
                    output.into()
                })
            }
        },
    };
    regenerate_project(&CompileOptions {
        root,
        llm: Some(&adapter),
        llm_config: Some(config),
        check: false,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(calls.load(Ordering::SeqCst) >= 2);
    assert!(translations.load(Ordering::SeqCst) >= 1);
}
#[tokio::test]
async fn locks_exclude_environment_and_record_llm_provenance() {
    let root = project();
    let config = config();
    compile(&root, &config).await;
    let mut forbidden = vec![
        config.endpoint_host.clone(),
        "probe-host".into(),
        config.model.clone(),
        config.provider.clone(),
        config.api_key.clone(),
        root.display().to_string(),
        root.canonicalize().unwrap().display().to_string(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        forbidden.push(home);
    }
    if let Ok(user) = std::env::var("USER") {
        if user.len() >= 3 {
            forbidden.push(user);
        }
    }
    let written = locks(&root);
    assert!(!written.is_empty());
    for (path, text) in &written {
        for value in &forbidden {
            assert!(!text.contains(value), "{path} contains {value}");
        }
    }
    let record: vibe::buildrecord::BuildRecord =
        serde_json::from_str(&fs::read_to_string(root.join(vibe::buildrecord::PATH)).unwrap())
            .unwrap();
    assert_eq!(record.compilers.len(), 2);
    let stamp = &record.compilers["models/product/product.lock.md"];
    assert_eq!(stamp.endpoint_host, config.endpoint_host);
    assert_eq!(stamp.model_snapshot, config.model);
    assert!(stamp.endpoint_private);
    assert_eq!(record.translations.len(), 1);
    let reused = vibe::compile::build_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reused.reused, 2);
    assert_eq!(locks(&root), written);
    let reused_record: vibe::buildrecord::BuildRecord =
        serde_json::from_slice(&fs::read(root.join(vibe::buildrecord::PATH)).unwrap()).unwrap();
    assert_eq!(
        reused_record, record,
        "reuse must preserve LLM and translation provenance"
    );
    validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: true,
    })
    .unwrap_or_else(|e| panic!("{e}"));
    let key = vibe::sign::load_or_create(&root).unwrap();
    let manifest = vibe::manifest::manifest_from_project_with_mode(&root, &key, true).unwrap();
    assert_eq!(manifest.provenance.compilers, record.compilers);
    let lock_path = root.join("models/product/product.lock.md");
    let text = fs::read_to_string(&lock_path).unwrap();
    let legacy = text.replacen("security-items:", "compiler: { name: md-logic, version: 0.1.0, model-snapshot: ignored, llm-provider: ignored, endpoint-host: ignored, endpoint-private: true, llm-seed: 1, seed: ignored, sent-params: {} }\nsecurity-items:", 1);
    fs::write(&lock_path, &legacy).unwrap();
    let report = validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: false,
    })
    .unwrap();
    let warnings = report.to_string();
    assert_eq!(warnings.matches("decision 174").count(), 1);
    assert!(!warnings.contains("legacy approval fields"));
    assert!(!warnings.contains("floating alias"));
    fs::write(&lock_path, text).unwrap();
    let mut old_record = record;
    let stamp = old_record
        .compilers
        .get_mut("models/product/product.lock.md")
        .unwrap();
    stamp.model_snapshot = "floating-latest".into();
    stamp.llm_provider = "none".into();
    fs::write(
        root.join(vibe::buildrecord::PATH),
        serde_json::to_string(&old_record).unwrap(),
    )
    .unwrap();
    let error = validate_project(&ValidateOptions {
        root: &root,
        dev: true,
        deploy: true,
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("model snapshot is a floating alias"));
    assert!(error.contains("natural locks must record an LLM provider"));
    old_record.compilers.clear();
    fs::write(
        root.join(vibe::buildrecord::PATH),
        serde_json::to_string(&old_record).unwrap(),
    )
    .unwrap();
    let error = validate_project(&ValidateOptions {
        root: &root,
        dev: false,
        deploy: true,
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("no build-record LLM stamp; run `vibe build`"));
    fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn different_llm_settings_produce_identical_locks() {
    let first = project();
    let second = project();
    let one = config();
    let mut two = config();
    two.provider = "openai".into();
    two.base_url = "https://other.example.test/other".into();
    two.endpoint_host = "other.example.test".into();
    two.endpoint_private = false;
    two.model = "other-model".into();
    two.seed = 42;
    two.send_sampling = false;
    two.api_style = "responses".into();
    compile(&first, &one).await;
    compile(&second, &two).await;
    assert_eq!(locks(&first), locks(&second));
    fs::remove_dir_all(first).unwrap();
    fs::remove_dir_all(second).unwrap();
}

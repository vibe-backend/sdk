#![cfg(feature = "sdk")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vibe::compile::{regenerate_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};

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
    let root = std::env::temp_dir().join(format!("vibe-sampling-warning-{}", uuid::Uuid::new_v4()));
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
// Keep this test alone in its binary: the sampling warning is shared once per process.
#[tokio::test]
async fn sampling_warning_is_emitted_once_across_compile_calls() {
    let root = project();
    source(
        &root,
        "# Create product\n\nCreate a product with sku and name using POST.\n",
    );
    let adapter = FakeAdapter {
        responder: |_: &LlmRequest| Ok(vibe::prompt_guide::FEATURE_CREATE_EXAMPLE.into()),
    };
    let mut config = config();
    let mut warnings = 0;
    for send_sampling in [false, false, false, true] {
        config.send_sampling = send_sampling;
        if send_sampling {
            config.model = "unregistered-sampling-test-model".into();
        }
        let cache = root.join(".vibe/cache");
        if cache.exists() {
            fs::remove_dir_all(cache).unwrap();
        }
        let out = regenerate_project(&CompileOptions {
            root: &root,
            llm: Some(&adapter),
            llm_config: Some(&config),
            check: false,
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        warnings += out
            .report
            .to_string()
            .matches("sampling parameters were not sent")
            .count();
    }
    assert_eq!(warnings, 1);
    fs::remove_dir_all(root).unwrap();
}

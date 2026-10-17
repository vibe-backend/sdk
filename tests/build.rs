use std::{fs, process::Command, time::Duration};
use vibe::compile::{build_project, CompileOptions};
use vibe::llm::{FakeAdapter, LlmConfig, LlmRequest};

const BODY: &str = "## Table\n- provider: main\n- table: products\n## Fields\n| name | type | rules | system |\n| --- | --- | --- | --- |\n| sku | string | required, unique, max: 40 | no |\n## CRUD\n| method | options |\n| --- | --- |\n| create | - |\n| get | - |\n## Policies\n- rls: enable, force\n- policy: tenant_id = current_setting('app.tenant_id', false)\n";

#[test]
fn command_surface() {
    for name in ["compile", "validate", "manifest", "new"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
            .env_clear()
            .args([name, "--project", "x"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(error.contains("was removed"), "{error}");
        if name == "new" {
            assert!(error.contains("run `vibe` with no arguments"));
        } else {
            assert!(error.contains("vibe build") && error.contains("--release"));
        }
    }
    let help = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    for removed in ["serve", "compile", "validate", "manifest"] {
        assert!(!help.contains(removed), "{help}");
    }
    assert!(Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .args(["serve", "--help"])
        .output()
        .unwrap()
        .status
        .success());
}

#[tokio::test]
async fn fresh_locks_work_in_ci_without_llm_and_stale_locks_regenerate() {
    let dir = std::env::temp_dir().join(format!("vibe-build-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(dir.join("models/product")).unwrap();
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    let source = dir.join("models/product/product.md");
    fs::write(
        &source,
        "# product\nStaff can create a product from a sku.\n",
    )
    .unwrap();
    let config = LlmConfig {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        api_key: "test-only".into(),
        model: "gpt-test-2026-01-01".into(),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        seed: 0,
        endpoint_host: "api.openai.com".into(),
        endpoint_private: false,
        send_sampling: true,
        api_style: "chat".into(),
    };
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let adapter = FakeAdapter {
        responder: |_: &LlmRequest| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(BODY.into())
        },
    };
    let opts = CompileOptions {
        root: &dir,
        llm: Some(&adapter),
        llm_config: Some(&config),
        check: false,
    };
    build_project(&opts).await.unwrap_or_else(|r| panic!("{r}"));
    let lock = dir.join("models/product/product.lock.md");
    let before = fs::read_to_string(&lock).unwrap();
    assert_eq!(
        vibe::lock::render_lock(&vibe::lock::parse_lock(&before).unwrap()),
        before
    );
    let record = fs::read(dir.join(vibe::buildrecord::PATH)).unwrap();
    let cli = || {
        Command::new(env!("CARGO_BIN_EXE_vibe"))
            .env_clear()
            .env("CI", "true")
            .args(["build", "--project"])
            .arg(&dir)
            .arg("--output")
            .arg(dir.join("manifest.json"))
            .output()
            .unwrap()
    };
    let result = cli();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let manifest_text = fs::read_to_string(dir.join("manifest.json")).unwrap();
    assert!(manifest_text.contains("\"vibe_version\""));
    assert!(!manifest_text.contains("compiler_hash"));
    let manifest: vibe::manifest::Manifest = serde_json::from_str(&manifest_text).unwrap();
    assert!(manifest
        .provenance
        .locks
        .iter()
        .all(|lock| lock.vibe_version == vibe::lock::lock_stamp()));
    assert_eq!(before, fs::read_to_string(&lock).unwrap());
    assert_eq!(record, fs::read(dir.join(vibe::buildrecord::PATH)).unwrap());
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    fs::write(
        &source,
        "# product\nStaff can create a product from a sku. Changed wording.\n",
    )
    .unwrap();
    let result = cli();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("models/product/product.md"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("set LLM_*"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("LLM_API_KEY is missing or invalid"));
    // A stamp mismatch is a regeneration candidate, even with unchanged source.
    fs::write(
        &source,
        "# product\nStaff can create a product from a sku.\n",
    )
    .unwrap();
    fs::write(
        &lock,
        before.replace(
            &format!("vibe-version: {}", vibe::lock::lock_stamp()),
            "vibe-version: 99.0",
        ),
    )
    .unwrap();
    // Match the build record too: the stamp alone must force regeneration.
    let mut record: vibe::buildrecord::BuildRecord = serde_json::from_slice(&record).unwrap();
    record.locks.insert(
        "models/product/product.lock.md".into(),
        vibe::buildrecord::lock_hash(&fs::read_to_string(&lock).unwrap()),
    );
    fs::write(
        dir.join(vibe::buildrecord::PATH),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    fs::remove_dir_all(dir.join(".vibe/cache")).unwrap();
    build_project(&opts).await.unwrap_or_else(|r| panic!("{r}"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(before, fs::read_to_string(&lock).unwrap());
    // Unchanged prose is stale when its structured provider changes.
    let provider = dir.join("providers/main.md");
    fs::write(
        &provider,
        fs::read_to_string(&provider)
            .unwrap()
            .replace("pool: 5", "pool: 6"),
    )
    .unwrap();
    let result = cli();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("models/product/product.md"));
    fs::remove_dir_all(dir.join(".vibe/cache")).unwrap();
    let output = build_project(&opts).await.unwrap_or_else(|r| panic!("{r}"));
    assert_eq!(output.reused, 0);
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 3);
    let text = fs::read_to_string(&lock).unwrap();
    assert_ne!(text, before);
    assert!(build_project(&CompileOptions {
        root: &dir,
        llm: None,
        llm_config: None,
        check: false
    })
    .await
    .is_ok());
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn old_and_wrong_stamps_are_rejected_by_validation_and_runtime() {
    use sha2::{Digest, Sha256};
    let dir = std::env::temp_dir().join(format!("vibe-stamp-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    build_project(&CompileOptions {
        root: &dir,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap();
    let path = dir.join("providers/main.lock.md");
    let original = fs::read_to_string(&path).unwrap();
    let raw = r#"{"compiler.version":"0.1.0","prompt-version":"prompt-10"}"#;
    let hash = format!("sha256:{}", hex::encode(Sha256::digest(raw.as_bytes())));
    let old = original.replace(
        &format!("vibe-version: {}", vibe::lock::lock_stamp()),
        &format!("compiler: {{ name: md-logic, version: 0.1.0 }}\ncompiler-hash: {hash}"),
    );
    assert_eq!(
        vibe::lock::parse_lock(&old).unwrap().meta.vibe_version,
        "prompt-10 (old compiler-hash format)"
    );
    for (text, found) in [
        (old, "prompt-10"),
        (
            original.replace(
                &format!("vibe-version: {}", vibe::lock::lock_stamp()),
                "vibe-version: 99.0",
            ),
            "99.0",
        ),
    ] {
        fs::write(&path, text).unwrap();
        let error = vibe::validate_project(&vibe::ValidateOptions {
            root: &dir,
            deploy: false,
            dev: true,
        })
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(found) && error.contains("regenerate it with `vibe build`"),
            "{error}"
        );
        let mut manifest =
            vibe::manifest::build_manifest_with_mode(&std::collections::HashMap::new(), true)
                .unwrap();
        manifest
            .provenance
            .locks
            .push(vibe::manifest::LockProvenance {
                vibe_version: vibe::lock::parse_lock(&fs::read_to_string(&path).unwrap())
                    .unwrap()
                    .meta
                    .vibe_version,
                ..Default::default()
            });
        for dev in [false, true] {
            let error = vibe::manifest::check_deployable(&manifest, dev).unwrap_err();
            assert!(error.contains(found) && error.contains("regenerate it with `vibe build`"));
        }
        // A signed manifest still fails the actual serve startup path.
        let key = vibe::sign::load_or_create(&dir).unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        manifest.signature = vibe::sign::sign_bytes(&bytes, &key);
        let manifest_path = dir.join("bad.json");
        vibe::manifest::write_manifest(&manifest_path, &manifest).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
            .env_clear()
            .args(["serve", "--memory", "--project"])
            .arg(&dir)
            .arg("--manifest")
            .arg(&manifest_path)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(
            error.contains(found) && error.contains("regenerate it with `vibe build`"),
            "{error}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_manifest_hash_is_described_without_printing_hash() {
    let mut manifest =
        vibe::manifest::build_manifest_with_mode(&std::collections::HashMap::new(), true).unwrap();
    let lock: vibe::manifest::LockProvenance = serde_json::from_value(serde_json::json!({
        "id": "old.lock.md", "lock_sha256": "", "content_hash": "", "source_hash": "",
        "compiler_hash": "sha256:old-manifest-hash"
    }))
    .unwrap();
    manifest.provenance.locks.push(lock);
    for development in [true, false] {
        let error = vibe::manifest::check_deployable(&manifest, development).unwrap_err();
        assert!(error.contains("an old compiler-hash format"));
        assert!(error.contains("regenerate it with `vibe build`"));
        assert!(!error.contains("sha256:"));
    }
}

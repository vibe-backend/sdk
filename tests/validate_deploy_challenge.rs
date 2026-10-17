//! `vibe build` refuses locks with unverified challenges (#199).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vibe::compile::{regenerate_project, CompileOptions};

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn validate_cli(project: &Path, deploy: bool, envs: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.arg("build")
        .arg("--project")
        .arg(project)
        .arg("--output")
        .arg(project.join("manifest.json"));
    if !deploy {
        cmd.arg("--check");
    }
    for key in [
        "VIBE_ENV",
        "VIBE_TURNSTILE_SECRET",
        "VIBE_CHALLENGE_HOSTNAMES",
        "VIBE_CHALLENGE_PROVIDER",
        "VIBE_CHALLENGE_REPLAY_STORE",
    ] {
        cmd.env_remove(key);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let mut text = String::from_utf8_lossy(&out.stderr).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    (out.status.success(), text)
}

#[tokio::test]
async fn validate_deploy_refuses_after_failures_fixture_lock() {
    let root = std::env::temp_dir().join(format!("vibe-val-af-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop"),
        &root,
    );
    let product = root.join("api/catalog/v1/get-product/get-product.md");
    let src = fs::read_to_string(&product).unwrap();
    fs::write(
        &product,
        src.replace(
            "challenge: { type: none, when: always }",
            "challenge: { type: captcha, when: after-failures, after-failures: 3 }",
        ),
    )
    .unwrap();

    regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));
    regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: true,
    })
    .await
    .unwrap_or_else(|err| panic!("{err}"));

    let lock =
        fs::read_to_string(root.join("api/catalog/v1/get-product/get-product.lock.md")).unwrap();
    assert!(
        lock.contains("after-failures"),
        "fixture lock should declare after-failures"
    );

    let (ok, text) = validate_cli(&root, false, &[]);
    assert!(ok, "build --check should pass: {text}");

    let (ok, text) = validate_cli(&root, true, &[]);
    assert!(!ok, "build deploy validation should fail: {text}");
    assert!(
        text.contains("after-failures") || text.contains("not verified"),
        "{text}"
    );

    let _ = fs::remove_dir_all(&root);
}

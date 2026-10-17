use std::fs;
use std::path::{Path, PathBuf};
use vibe::lock::parse_lock;
use vibe::{
    approve_project, compile_project, manifest_from_project, read_manifest, write_manifest,
    ApproveOptions, CompileOptions,
};

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

#[tokio::test]
async fn project_manifest_contains_signed_audit() {
    // This integration test has its own process and is the only environment writer.
    let root = std::env::temp_dir().join(format!("vibe-manifest-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop"),
        &root,
    );
    compile_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await
    .unwrap();
    let mut hashes = Vec::new();
    for file in vibe::discover::discover(&root).unwrap() {
        let doc = parse_lock(&fs::read_to_string(root.join(file.lock_rel())).unwrap()).unwrap();
        hashes.push(doc.meta.compiler_hash);
    }
    hashes.sort();
    hashes.dedup();
    let key = vibe::sign::load_or_create(&root).unwrap();
    approve_project(&ApproveOptions {
        root: &root,
        reviewers: &["ada@example.com".into(), "grace@example.com".into()],
        reason: "manifest fixture",
        approval_ref: "local",
        commit: "local",
        now: "2026-10-17T00:00:00Z",
        signing_key: &key,
    })
    .unwrap();
    let sha = "0123456789abcdef0123456789abcdef01234567";
    for (name, value) in [
        ("GITHUB_REF_TYPE", "tag"),
        ("GITHUB_REF_NAME", "v1.8.0-rc.1"),
        ("GITHUB_SHA", sha),
        ("GITHUB_RUN_ID", "42"),
    ] {
        std::env::set_var(name, value);
    }
    let before = chrono::Utc::now();
    let manifest = manifest_from_project(&root, &key).unwrap();
    let after = chrono::Utc::now();
    let audit = manifest.audit.as_ref().unwrap();
    assert_eq!(audit.tag.as_deref(), Some("v1.8.0-rc.1"));
    assert_eq!(audit.commit.as_deref(), Some(sha));
    assert_eq!(audit.run_id.as_deref(), Some("42"));
    assert!(!hashes.is_empty());
    assert_eq!(audit.compiler_hashes, hashes);
    let built = chrono::DateTime::parse_from_rfc3339(&audit.built_at).unwrap();
    assert!(built.timestamp() >= before.timestamp() && built.timestamp() <= after.timestamp());
    assert!(audit.built_at.ends_with('Z'));
    let path = root.join("manifest.json");
    write_manifest(&path, &manifest).unwrap();
    assert_eq!(
        read_manifest(&path, &key.verifying_key()).unwrap(),
        manifest
    );
    for (name, value, field) in [
        ("GITHUB_REF_NAME", "invalid", "tag"),
        ("GITHUB_SHA", "abc", "commit"),
        ("GITHUB_RUN_ID", " 42 ", "run_id"),
    ] {
        let previous = std::env::var(name).unwrap();
        std::env::set_var(name, value);
        assert!(manifest_from_project(&root, &key)
            .unwrap_err()
            .to_string()
            .contains(field));
        std::env::set_var(name, previous);
    }
    fs::remove_dir_all(root).unwrap();
}

#![cfg(feature = "sdk")]

use std::{fs, path::Path, process::Command};
use vibe::discover::discover;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn flat_sources_attach_schemas_and_skip_locks() {
    let root = std::env::temp_dir().join(format!("vibe-flat-{}", uuid::Uuid::new_v4()));
    for (rel, text) in [
        ("api/shop/_definition.md", "definition"),
        ("api/shop/v1/_version.md", "version"),
        ("api/shop/v1/get-item.md", "endpoint\n"),
        ("api/shop/v1/get-item.schema.md", "schema"),
        ("api/shop/v1/get-item.lock.md", "ignored"),
    ] {
        write(&root, rel, text);
    }
    let files = discover(&root).unwrap();
    assert_eq!(files.len(), 3);
    let endpoint = files.iter().find(|f| f.id == "shop/v1/get-item").unwrap();
    assert_eq!(endpoint.lock_rel(), "api/shop/v1/get-item.lock.md");
    assert_eq!(
        endpoint.combined_text(),
        "endpoint\n\n<!-- schema.md -->\nschema"
    );
    write(&root, "api/shop/v1/orphan.schema.md", "schema");
    let report = discover(&root).unwrap_err().to_string();
    assert!(
        report.contains("schema has no matching endpoint file: api/shop/v1/orphan.md"),
        "{report}"
    );
    fs::remove_file(root.join("api/shop/v1/orphan.schema.md")).unwrap();
    write(&root, "api/shop/v1/schema.md", "endpoint named schema");
    assert!(discover(&root)
        .unwrap()
        .iter()
        .any(|f| f.id == "shop/v1/schema"));
    write(&root, "api/shop/v1/bad.name.md", "bad");
    assert!(discover(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn model_schema_is_rejected_as_a_source_path() {
    let root = std::env::temp_dir().join(format!("vibe-model-schema-{}", uuid::Uuid::new_v4()));
    write(&root, "models/item/item.md", "model");
    write(&root, "models/item/item.schema.md", "schema");

    let report = discover(&root).unwrap_err();
    assert_eq!(report.diags.len(), 1);
    assert_eq!(report.diags[0].file, "models/item/item.schema.md");
    assert_eq!(
        report.diags[0].message,
        "item.schema.md is not a source markdown file"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn old_files_are_all_reported_in_path_order_and_cli_modes_fail() {
    let root = std::env::temp_dir().join(format!("vibe-old-{}", uuid::Uuid::new_v4()));
    let cases = [
        ("get-item.lock.md", "delete it; `vibe build` writes api/shop/v1/get-item.lock.md"),
        ("get-item.md", "move it to api/shop/v1/get-item.md"),
        ("notes.txt", "endpoint folders are no longer supported; move endpoint sources to api/shop/v1/<name>.md"),
        ("schema.md", "move it to api/shop/v1/get-item.schema.md"),
    ];
    for (file, _) in cases {
        write(&root, &format!("api/shop/v1/get-item/{file}"), "old");
    }
    let report = discover(&root).unwrap_err();
    assert_eq!(report.diags.len(), cases.len());
    for (diag, (file, action)) in report.diags.iter().zip(cases) {
        assert_eq!(diag.file, format!("api/shop/v1/get-item/{file}"));
        assert_eq!(
            diag.message,
            format!("uses the old endpoint folder layout (decision 186): {action}")
        );
    }
    write(&root, ".gitignore", ".env\n");
    for mode in [None, Some("--check"), Some("--release")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vibe"));
        command.env_clear().arg("build").arg("--project").arg(&root);
        if let Some(mode) = mode {
            command.arg(mode);
        }
        let output = command.output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        for (_, action) in cases {
            assert!(error.contains(action), "{mode:?}: {error}");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

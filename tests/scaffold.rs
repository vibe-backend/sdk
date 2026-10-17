use std::{fs, process::Command};

#[test]
fn new_scaffolds_and_preserves_sources() {
    let root = std::env::temp_dir().join(format!("vibe-new-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let run = |path: &str| {
        Command::new(env!("CARGO_BIN_EXE_vibe"))
            .current_dir(&root)
            .args(["new", path])
            .output()
            .unwrap()
    };
    assert!(run("hello/v1/hello").status.success());
    for path in [
        "api/hello/_definition.md",
        "api/hello/v1/_version.md",
        "providers/main.md",
        "api/hello/v1/hello/hello.md",
    ] {
        assert!(root.join(path).is_file());
    }
    fs::write(root.join("providers/main.md"), "preserved").unwrap();
    let output = run("hello/v1/other");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("kept providers/main.md"));
    assert_eq!(
        fs::read_to_string(root.join("providers/main.md")).unwrap(),
        "preserved"
    );
    // Existing feature must be checked before creating even missing shared sources.
    fs::remove_file(root.join("api/hello/_definition.md")).unwrap();
    assert!(!run("hello/v1/hello").status.success());
    assert!(!root.join("api/hello/_definition.md").exists());
    for path in ["Hello/v1/x", "hello/1/x"] {
        assert!(!run(path).status.success());
    }
    fs::remove_dir_all(root).unwrap();
}

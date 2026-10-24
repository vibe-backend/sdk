#![cfg(feature = "sdk")]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

fn project() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-aux-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join(".vibe")).unwrap();
    fs::create_dir_all(root.join("api")).unwrap();
    fs::write(root.join(".gitignore"), ".env\n").unwrap();
    // Multi-tenant slug files and `{slug}` routes opt out of the built-in defaults.
    fs::write(
        root.join("api/_project.md"),
        "# project\n\n## Routes\n\n- prefix: (none)\n- default-slug: (none)\n",
    )
    .unwrap();
    root
}
fn build(root: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .current_dir(root)
        .arg("build")
        .args(args)
        .output()
        .unwrap()
}
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

macro_rules! removed {
    ($test:ident, $name:literal, $hint:literal) => {
        #[test]
        fn $test() {
            for args in [vec![$name], vec![$name, "add", "x", "--tenant", "y"]] {
                let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
                    .env_clear()
                    .args(args)
                    .output()
                    .unwrap();
                assert_eq!(out.status.code(), Some(2));
                assert_eq!(stderr(&out), concat!($hint, "\n"));
                assert!(out.stdout.is_empty());
            }
        }
    };
}
removed!(
    removed_migrate,
    "migrate",
    "`vibe migrate` was removed: migrations run automatically in `vibe run --db postgres`"
);
removed!(removed_keygen, "keygen", "`vibe keygen` was removed: `vibe build` creates the development signing key (.vibe/signing.key) automatically; release builds need no key (the release workflow signs with Sigstore keyless)");
removed!(removed_slug, "slug", "`vibe slug` was removed: add public slugs to .vibe/slugs.json (`vibe build` checks the file and lists the slugs)");
removed!(
    removed_secret_scan,
    "secret-scan",
    "`vibe secret-scan` was removed: `vibe build` runs the secret scan and fails on findings"
);
removed!(removed_endpoint, "endpoint", "`vibe endpoint` was removed: list disabled features in .vibe/disabled.txt (one `<feature-id><TAB><reason>` per line; `vibe build` checks it)");
removed!(
    removed_approve,
    "approve",
    "`vibe approve` was removed: use `vibe build`; review happens on the source pull request"
);

#[test]
fn help_has_only_build_run_and_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = stdout(&out);
    let commands = help
        .split("Commands:\n")
        .nth(1)
        .unwrap()
        .split("\nOptions:")
        .next()
        .unwrap();
    let names: Vec<_> = commands
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(names, ["build", "run", "help"]);
}

#[test]
fn build_lists_and_validates_slug_files() {
    let root = project();
    fs::write(
        root.join(".vibe/slugs.json"),
        r#"{"z":{"tenant_id":"tenant0002","public_write":true},"a":{"tenant_id":"tenant0001"}}"#,
    )
    .unwrap();
    let out = build(&root, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let output = stdout(&out);
    assert!(output.contains("slug a -> tenant tenant0001 (public write: no)\nslug z -> tenant tenant0002 (public write: yes)"));
    for text in [
        r#"{"Bad":{"tenant_id":"tenant0001"}}"#,
        r#"{"ok":{"tenant_id":"X"}}"#,
        "[]",
    ] {
        fs::write(root.join(".vibe/slugs.json"), text).unwrap();
        let out = build(&root, &[]);
        assert!(!out.status.success());
        assert!(stderr(&out).contains(".vibe/slugs.json"));
    }
    fs::write(root.join(".vibe/slugs.json"), "{}").unwrap();
    fs::write(root.join("slugs.example.json"), "broken").unwrap();
    let out = build(&root, &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("slugs.example.json"));
    fs::remove_file(root.join(".vibe/slugs.json")).unwrap();
    fs::write(
        root.join("slugs.example.json"),
        r#"{"example":{"tenant_id":"tenant0001"}}"#,
    )
    .unwrap();
    let out = build(&root, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("slugs from slugs.example.json (dev/memory only)"));
    assert!(stdout(&out).contains("slug example -> tenant tenant0001 (public write: no)"));
    fs::create_dir_all(root.join("api/demo/v1")).unwrap();
    fs::write(
        root.join("api/demo/v1/test.md"),
        "# test\nGET /demo/v1/{slug}/test\n",
    )
    .unwrap();
    let out = build(&root, &[]);
    assert!(stderr(&out).contains("warning: public routes use {slug} but .vibe/slugs.json has no slugs; add one, e.g. {\"<slug>\": {\"tenant_id\": \"<8-32 lowercase letters or digits>\", \"public_write\": false}}"));
    assert!(stderr(&out).contains("slugs.example.json is used only by `vibe run --db memory`"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn disabled_features_are_checked_and_listed() {
    let root = project();
    fs::write(root.join(".vibe/disabled.txt"), "missing\tmaintenance\n").unwrap();
    let out = build(&root, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("disabled feature missing: maintenance"));
    assert!(stderr(&out).contains("warning: .vibe/disabled.txt line 1: unknown feature missing"));
    for line in ["missing", "missing\t", "missing\t   "] {
        fs::write(root.join(".vibe/disabled.txt"), format!("\n{line}\n")).unwrap();
        let out = build(&root, &[]);
        assert!(!out.status.success());
        assert!(stderr(&out).contains(".vibe/disabled.txt line 2:"));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn secret_findings_fail_every_build_mode() {
    let root = project();
    fs::write(root.join("leak.txt"), format!("sk-{}", "x".repeat(30))).unwrap();
    for args in [&[][..], &["--check"][..], &["--release"][..]] {
        let out = build(&root, args);
        assert!(!out.status.success());
        assert!(
            stderr(&out).contains("leak.txt: possible secret material"),
            "{}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("secret scan found a problem"));
        assert!(!stderr(&out).contains(&"x".repeat(30)));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn artifact_missing_public_key_has_actionable_error_in_both_binaries() {
    let root = project();
    let out = build(&root, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    fs::remove_file(root.join(".vibe/signing.pub")).unwrap();
    let expected = format!("dev manifest needs the development public key {}; copy .vibe/signing.pub from the project that ran `vibe build` (next to the artifact, or pass --project <dir>)", root.join(".vibe/signing.pub").display());
    for bin in [
        env!("CARGO_BIN_EXE_vibe"),
        env!("CARGO_BIN_EXE_vibe-runtime"),
    ] {
        let mut cmd = Command::new(bin);
        cmd.env_clear().current_dir(&root);
        if bin == env!("CARGO_BIN_EXE_vibe") {
            cmd.arg("run");
        }
        let out = cmd
            .args(["--artifact", ".", "--db", "memory", "--project"])
            .arg(&root)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            stderr(&out).lines().any(|line| line == expected),
            "{}",
            stderr(&out)
        );
        assert!(out.stdout.is_empty());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn release_does_not_load_a_development_signing_key() {
    let root = project();
    for invalid_key in [false, true] {
        if invalid_key {
            fs::write(root.join(".vibe/signing.key"), "invalid").unwrap();
        }
        let out = build(
            &root,
            &[
                "--release",
                "--release-version",
                "1.8.0",
                "--commit",
                "0123456789abcdef0123456789abcdef01234567",
            ],
        );
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(!stderr(&out).contains("created development signing key"));
        assert!(!root.join(".vibe/signing.pub").exists());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn release_natural_language_sources_explain_missing_llm_credentials() {
    let root = project();
    fs::create_dir_all(root.join("models/product")).unwrap();
    fs::write(
        root.join("models/product/product.md"),
        "# product\nStaff can create products.\n",
    )
    .unwrap();
    let out = build(
        &root,
        &[
            "--release",
            "--release-version",
            "1.8.0",
            "--commit",
            "0123456789abcdef0123456789abcdef01234567",
        ],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("set LLM_PROVIDER, LLM_MODEL and LLM_API_KEY or LLM_API_KEY_FILE in the build environment"), "{}", stderr(&out));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_private_key_instruction_points_to_build() {
    let root = project();
    let error = vibe::sign::load_dev_key(&root).err().unwrap();
    assert_eq!(
        error,
        format!(
            "no development signing key at {}; run `vibe build` in the project to create one",
            root.join(".vibe/signing.key").display()
        )
    );
    fs::remove_dir_all(root).unwrap();
}

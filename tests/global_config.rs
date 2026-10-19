#![cfg(feature = "sdk")]
use std::{collections::HashMap, fs, path::Path, process::Command};
use vibe::global_config::{config_dir, load, write_initial};
fn root() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("vibe-global-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&p).unwrap();
    p
}
fn values() -> HashMap<String, String> {
    HashMap::from([
        ("LLM_PROVIDER".into(), "fake".into()),
        ("LLM_MODEL".into(), "global-model".into()),
    ])
}
#[test]
fn precedence_keys_sources_and_paths() {
    let dir = root().join("private");
    let (config, key) = write_initial(&dir, &values(), "sentinel-secret").unwrap();
    let mut env = HashMap::from([
        ("LLM_PROVIDER".into(), "env-provider".into()),
        ("LLM_MODEL".into(), "".into()),
    ]);
    let (merged, debug) = load(&env, "LLM_MODEL=project-model\n", &dir, false).unwrap();
    assert_eq!(merged["LLM_PROVIDER"], "env-provider");
    assert_eq!(merged["LLM_MODEL"], "project-model");
    assert!(merged["LLM_API_KEY"] == "sentinel-secret");
    assert!(debug.contains(&"LLM_PROVIDER: env".into()));
    assert!(debug.contains(&"LLM_MODEL: project".into()));
    assert!(debug.contains(&"LLM_API_KEY: global".into()));
    assert!(!debug.join("\n").contains("sentinel-secret"));
    fs::write(&config, "LLM_PROVIDER=fake\nLLM_API_KEY_FILE=llm-api-key\n").unwrap();
    assert!(load(&HashMap::new(), "", &dir, false).unwrap().0["LLM_API_KEY"] == "sentinel-secret");
    fs::remove_file(key).unwrap();
    env.insert("LLM_API_KEY".into(), "higher-secret".into());
    assert!(load(&env, "", &dir, false).is_ok());
    assert!(load(
        &HashMap::new(),
        "LLM_API_KEY_FILE=/not/read/here",
        &dir,
        false
    )
    .is_ok());
    assert!(write_initial(&dir, &values(), "other-secret").is_err());
    fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}
#[test]
fn validation_and_gates() {
    let dir = root();
    let config = dir.join("llm.env");
    write_initial(&dir, &values(), "sentinel-secret").unwrap();
    for text in [
        "LLM_API_KEY=sentinel-secret",
        "OPENAI_API_KEY=sentinel-secret",
        "UNKNOWN=sentinel-secret",
    ] {
        fs::write(&config, text).unwrap();
        let error = match load(&HashMap::new(), "", &dir, false) {
            Err(error) => error,
            Ok(_) => panic!("expected refusal"),
        };
        assert!(!error.contains("sentinel-secret"));
        assert!(error.contains(text.split('=').next().unwrap()));
        for (k, v) in [
            ("CI", "true"),
            ("CI", "1"),
            ("GITHUB_ACTIONS", "true"),
            ("VIBE_ENV", "production"),
            ("VIBE_ENV", "live"),
            ("VIBE_ENV", "ci"),
        ] {
            assert!(load(&HashMap::from([(k.into(), v.into())]), "", &dir, false).is_ok());
        }
        assert!(load(&HashMap::new(), "", &dir, true).is_ok());
    }
    fs::write(&config, "LLM_PROVIDER=fake\n").unwrap();
    for value in ["false", "0", ""] {
        let (merged, _) = load(
            &HashMap::from([("CI".into(), value.into())]),
            "",
            &dir,
            false,
        )
        .unwrap();
        assert_eq!(merged["LLM_PROVIDER"], "fake");
    }
    let (merged, _) = load(&HashMap::new(), "VIBE_ENV=production", &dir, false).unwrap();
    assert!(!merged.contains_key("LLM_PROVIDER"));
    let env = HashMap::from([
        ("XDG_CONFIG_HOME".into(), "relative".into()),
        ("HOME".into(), "/test-home".into()),
    ]);
    assert_eq!(
        config_dir(&env).unwrap(),
        Path::new("/test-home/.config/vibe")
    );
    fs::remove_dir_all(dir).unwrap();
}
#[cfg(unix)]
#[test]
fn private_modes_and_permission_errors() {
    use std::os::unix::fs::PermissionsExt;
    let root = root();
    let dir = root.join("private");
    let (config, key) = write_initial(&dir, &values(), "sentinel-secret").unwrap();
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for path in [&config, &key] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(!fs::read_to_string(&config)
        .unwrap()
        .contains("sentinel-secret"));
    assert!(fs::read_to_string(&key).unwrap() == "sentinel-secret");
    for path in [&config, &key] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = match load(&HashMap::new(), "", &dir, false) {
            Err(error) => error,
            Ok(_) => panic!("expected refusal"),
        };
        assert!(error.contains("chmod 600"));
        assert!(error.contains(&path.display().to_string()));
        assert!(!error.contains("sentinel-secret"));
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn dev_build_and_release_ignore_global() {
    let root = root();
    let project = root.join("project");
    fs::create_dir_all(project.join("api/hello/v1/hello")).unwrap();
    fs::write(
        project.join("api/hello/_definition.md"),
        vibe::scaffold::definition_content("hello"),
    )
    .unwrap();
    fs::write(
        project.join("api/hello/v1/_version.md"),
        vibe::scaffold::version_content("hello", "v1"),
    )
    .unwrap();
    fs::write(project.join("api/hello/v1/hello/hello.md"), "---\npublic: true\nwrites: false\nauth: none\n---\n# Hello\nGET /hello/v1/{slug}/hello returns constant JSON {\"message\":\"hello world\"} with status 200.\n").unwrap();
    fs::write(project.join(".gitignore"), ".env\n.vibe/\n").unwrap();
    let config = root.join("config");
    write_initial(&config.join("vibe"), &values(), "sentinel-secret").unwrap();
    let run = |release: bool, extra: Option<(&str, &str)>| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
        cmd.env_clear()
            .env("XDG_CONFIG_HOME", &config)
            .env("HOME", &root)
            .env("VIBE_DEBUG_LLM_CONFIG", "1")
            .current_dir(&project)
            .arg("build");
        if release {
            cmd.arg("--release");
        }
        if let Some((k, v)) = extra {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };
    let output = run(false, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LLM_PROVIDER: global"));
    assert!(!stderr.contains("sentinel-secret"));
    assert!(!stderr.contains("global-model"));
    let check = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .env("XDG_CONFIG_HOME", &config)
        .env("HOME", &root)
        .current_dir(&project)
        .args(["build", "--check"])
        .output()
        .unwrap();
    assert!(check.status.success(), "dev check failed");
    // Invalid global contents demonstrate these paths do not even parse it.
    fs::write(config.join("vibe/llm.env"), "UNKNOWN=sentinel-secret").unwrap();
    for (release, extra) in [
        (true, None),
        (false, Some(("CI", "true"))),
        (false, Some(("GITHUB_ACTIONS", "true"))),
        (false, Some(("VIBE_ENV", "production"))),
    ] {
        let output = run(release, extra);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("unknown global"));
        assert!(!stderr.contains("sentinel-secret"));
        assert!(!stderr.contains(": global"));
    }
    fs::remove_dir_all(root).unwrap();
}

#![cfg(feature = "sdk")]

use std::{fs, path::PathBuf, process::Command};

fn project() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-run-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("api/hello/v1/hello")).unwrap();
    fs::write(
        root.join("api/hello/v1/hello/hello.md"),
        "---\npublic: true\nauth: removed\n---\n# Hello\n\nA public GET endpoint returning {\"message\":\"hello world\"}.\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("providers")).unwrap();
    fs::write(
        root.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    fs::write(root.join("api/hello/_definition.md"), "# hello\n\n## Providers\n- allowlist: [main]\n- default: main\n## Pipelines\n(none)\n## Defaults\n- timeout: 10s\n- body: 1MB\n").unwrap();
    fs::write(
        root.join("api/hello/v1/_version.md"),
        include_str!("fixtures/shop/api/shop/v1/_version.md"),
    )
    .unwrap();
    root
}
fn cli(root: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vibe"));
    c.env_clear().current_dir(root);
    c
}
fn build(root: &std::path::Path) {
    let out = cli(root)
        .env("LLM_PROVIDER", "fake")
        .env("CI", "true")
        .arg("build")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn fake_build_and_run_guards() {
    let root = project();
    build(&root);
    let lock = fs::read_to_string(root.join("api/hello/v1/hello/hello.lock.md")).unwrap();
    assert!(!lock.contains("fake"));
    for file in ["manifest.json", ".vibe/build-record.json"] {
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(file)).unwrap()).unwrap();
        assert_eq!(
            if file == "manifest.json" {
                &json["provenance"]["test_build"]
            } else {
                &json["test_build"]
            },
            &true
        );
    }
    let out = cli(&root).arg("run").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--allow-test-build"));
    let out = cli(&root)
        .env("LLM_PROVIDER", "fake")
        .args(["build", "--release"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used for a release build"));
    let out = cli(&root).arg("build").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("hello.md"));
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn missing_stale_and_wrong_version_require_build() {
    for case in ["missing", "stale", "version"] {
        let root = project();
        if case != "missing" {
            build(&root);
        }
        if case == "stale" {
            fs::write(root.join("api/hello/v1/hello/hello.md"), "Changed source").unwrap();
        }
        if case == "version" {
            let path = root.join("api/hello/v1/hello/hello.lock.md");
            fs::write(
                &path,
                fs::read_to_string(&path).unwrap().replace(
                    &format!("vibe-version: {}", vibe::lock::lock_stamp()),
                    "vibe-version: 99.0",
                ),
            )
            .unwrap();
        }
        let out = cli(&root)
            .args(["run", "--allow-test-build"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("run `vibe build`"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn removed_serve_and_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .arg("serve")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("`vibe serve` was removed: use `vibe run`")
    );
    let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env_clear()
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("  build") && help.contains("  run"));
    assert!(!help.contains("  serve"));
}
#[test]
fn hello_serves_without_loading_llm_configuration() {
    use std::io::{Read, Write};
    for broken in [false, true] {
        let root = project();
        build(&root);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut cmd = cli(&root);
        cmd.args(["run", "--allow-test-build", "--addr", &addr.to_string()]);
        if broken {
            cmd.env("LLM_PROVIDER", "bogus")
                .env("LLM_BASE_URL", "http://10.255.255.1");
        }
        let mut child = cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let response = (|| {
            for _ in 0..100 {
                if let Ok(mut stream) = std::net::TcpStream::connect(addr) {
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                        .unwrap();
                    stream.write_all(b"GET /hello/v1/dev/hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
                    let mut body = String::new();
                    stream.read_to_string(&mut body).unwrap();
                    return body;
                }
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            String::new()
        })();
        let _ = child.kill();
        let out = child.wait_with_output().unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200") && response.contains("hello world"),
            "{response}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn api_key_file_is_never_printed() {
    let root = project();
    let key = "unique-private-ci-key-852902";
    fs::write(root.join("key"), format!("{key}\n")).unwrap();
    let out = cli(&root)
        .env("LLM_API_KEY_FILE", root.join("key"))
        .env("LLM_MODEL", "gpt-test-2026-01-01")
        .env("LLM_BASE_URL", "http://127.0.0.1:1")
        .env("LLM_ALLOW_PRIVATE_HOST", "true")
        .env("LLM_MAX_RETRIES", "0")
        .arg("build")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains(key));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(key));
    fs::remove_dir_all(root).unwrap();
}

// Run preparation in a child so stripping compile-time variables cannot race other tests.
#[test]
fn prepared_hello_router_without_socket() {
    if std::env::var_os("VIBE_PREPARE_CHILD").is_some() {
        use tower::ServiceExt;
        let root = project();
        build(&root);
        fs::remove_file(root.join("manifest.json")).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let ready = vibe::run::prepare(&vibe::run::RunOptions {
                project: root.clone(),
                source: vibe::run::RunSource::Project,
                addr: "127.0.0.1:8080".into(),
                db: vibe::run::DbChoice::Memory,
                allow_test_build: true,
            })
            .await
            .unwrap();
            let mut request = axum::http::Request::builder()
                .uri("/hello/v1/dev/hello")
                .body(axum::body::Body::empty())
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                "127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap(),
            ));
            let response = ready.app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), 200);
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                serde_json::json!({"message":"hello world"})
            );
        });
        assert!(!root.join("manifest.json").exists());
        fs::remove_dir_all(root).unwrap();
        return;
    }
    for broken in [false, true] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .env("VIBE_PREPARE_CHILD", "1")
            .args(["--exact", "prepared_hello_router_without_socket"]);
        if broken {
            child
                .env("LLM_PROVIDER", "bogus")
                .env("LLM_BASE_URL", "http://10.255.255.1");
        }
        assert!(child.status().unwrap().success());
    }
}

fn runtime_cli(root: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe-runtime"));
    cmd.env_clear().current_dir(root);
    cmd
}

#[test]
fn runtime_help_version_missing_artifact_and_wrong_stamp() {
    let root = project();
    for flag in ["--help", "--version"] {
        let out = runtime_cli(&root).arg(flag).output().unwrap();
        assert!(out.status.success());
        if flag == "--version" {
            assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
        }
    }
    for args in [vec![], vec!["run"]] {
        let out = runtime_cli(&root).args(args).output().unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("No such file"));
    }
    build(&root);
    let path = root.join("manifest.json");
    let mut manifest: vibe::manifest::Manifest =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest.provenance.locks[0].vibe_version = "99.0".into();
    manifest.signature.clear();
    let key = vibe::sign::load_or_create(&root).unwrap();
    manifest.signature = vibe::sign::sign_bytes(&serde_json::to_vec(&manifest).unwrap(), &key);
    vibe::manifest::write_manifest(&path, &manifest).unwrap();
    let out = runtime_cli(&root)
        .args(["run", "--artifact", ".", "--allow-test-build"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("regenerate it with `vibe build`"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_artifact_serves_hello() {
    use std::io::{Read, Write};
    let root = project();
    build(&root);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    // Run outside the artifact directory to exercise the project default.
    let mut child = Command::new(env!("CARGO_BIN_EXE_vibe-runtime"))
        .env_clear()
        .args(["run", "--artifact"])
        .arg(&root)
        .args(["--allow-test-build", "--addr", &addr.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let response = (|| {
        for _ in 0..100 {
            if let Ok(mut stream) = std::net::TcpStream::connect(addr) {
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                stream.write_all(b"GET /hello/v1/dev/hello HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
                let mut body = String::new();
                stream.read_to_string(&mut body).unwrap();
                return body;
            }
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        String::new()
    })();
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200") && response.contains("hello world"),
        "{response}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

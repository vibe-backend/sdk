#![cfg(feature = "sdk")]
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use tower::ServiceExt;
use vibe::{
    manifest::Manifest,
    runtime::{router, AppState, AuthConfig, MemoryExec},
};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            copy(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}
fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("vibe-project-routes-{}", uuid::Uuid::new_v4()));
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web-counter"),
        &root,
    );
    root
}
fn build(root: &Path) -> Result<Manifest, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .current_dir(root)
        .env_clear()
        .arg("build")
        .output()
        .unwrap();
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap())
}
fn state(root: &Path, manifest: Manifest) -> Arc<AppState> {
    Arc::new(AppState {
        manifest,
        exec: Arc::new(MemoryExec::new()),
        auth: AuthConfig::anonymous(),
        slugs: vibe::runtime::load_project_slugs(root, true).unwrap().slugs,
        dev_slugs: None,
        rates: Default::default(),
        quotas: Default::default(),
        disabled: Default::default(),
        min_override_ms: Some(0),
        honor_test_ip: true,
        trusted_proxies: vec![],
        dev_hints: true,
        challenge: None,
    })
}
async fn request(
    state: Arc<AppState>,
    method: &str,
    path: &str,
) -> (StatusCode, serde_json::Value) {
    let response = router(state)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn prefixed_default_slug_uses_tenant_permissions_and_quota() {
    let root = fixture();
    let manifest = build(&root).unwrap();
    assert_eq!(manifest.route_prefix, "/api");
    assert_eq!(manifest.default_slug.as_deref(), Some("acme"));
    assert!(manifest
        .routes
        .iter()
        .all(|r| r.path == "/api/web/v1/counter"));
    let state = state(&root, manifest);
    let (status, body) = request(state.clone(), "GET", "/api/web/v1/counter").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], 0);
    let (status, body) = request(state.clone(), "POST", "/api/web/v1/counter").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], 1);
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    // Seed only the tenant bucket, with its resolved tenant id, not slug acme.
    for _ in 1..1000 {
        assert!(state
            .quotas
            .lock()
            .unwrap()
            .check(&day, &[("tenant", "acme0001".into(), 1000)]));
    }
    assert_eq!(
        request(state.clone(), "POST", "/api/web/v1/counter")
            .await
            .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        request(state.clone(), "GET", "/api/web/v1/counter").await.1["value"],
        1
    );
    let mut state = Arc::try_unwrap(state).ok().unwrap();
    state.slugs.get_mut("acme").unwrap().public_write = false;
    assert_eq!(
        request(Arc::new(state), "POST", "/api/web/v1/counter")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn invalid_settings_and_tenant_conflicts_are_build_errors() {
    let root = fixture();
    for prefix in [
        "api",
        "/api/",
        "/a//b",
        "/{x}",
        "/A",
        "/a.b",
        "/_internal",
        "/health",
        &format!("/{}", "a".repeat(64)),
    ] {
        fs::write(
            root.join("api/_project.md"),
            format!("## Routes\n- prefix: {prefix}\n"),
        )
        .unwrap();
        assert!(build(&root).unwrap_err().contains("prefix"), "{prefix}");
    }
    fs::write(
        root.join("api/_project.md"),
        "## Routes\n- default-slug: BAD\n",
    )
    .unwrap();
    assert!(build(&root).unwrap_err().contains("default-slug"));
    fs::write(
        root.join("api/_project.md"),
        "## Routes\n- default-slug: acme\n",
    )
    .unwrap();
    fs::write(
        root.join("slugs.example.json"),
        r#"{"other":{"tenant_id":"other001"}}"#,
    )
    .unwrap();
    assert!(build(&root).unwrap_err().contains("extra slugs other"));
    fs::remove_file(root.join("slugs.example.json")).unwrap();
    let path = root.join("api/web/v1/counter.md");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        source.replace("/web/v1/counter", "/web/v1/{slug}/counter"),
    )
    .unwrap();
    assert!(build(&root).unwrap_err().contains("route uses {slug}"));
    fs::write(
        &path,
        source.replace("tenant: default-slug", "tenant: path.slug"),
    )
    .unwrap();
    assert!(build(&root).unwrap_err().contains("tenant: default-slug"));
    fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn prefix_only_and_default_only_preserve_route_modes() {
    for (settings, path, slug_in_path, omit_manifest_fields) in [
        (
            "- prefix: /api\n- default-slug: (none)",
            "/api/web/v1/acme/counter",
            true,
            false,
        ),
        (
            "- prefix: (none)\n- default-slug: acme",
            "/web/v1/counter",
            false,
            false,
        ),
        ("", "/api/web/v1/counter", false, false),
        (
            "- prefix: (none)\n- default-slug: (none)",
            "/web/v1/acme/counter",
            true,
            true,
        ),
    ] {
        let root = fixture();
        if settings.is_empty() {
            fs::remove_file(root.join("api/_project.md")).unwrap();
            // The built-in default slug is dev, so the example file cannot list acme.
            fs::write(
                root.join("slugs.example.json"),
                r#"{"dev":{"tenant_id":"devtenant","public_access":true,"public_write":true}}"#,
            )
            .unwrap();
        } else {
            fs::write(
                root.join("api/_project.md"),
                format!("## Routes\n{settings}\n"),
            )
            .unwrap();
        }
        if slug_in_path {
            for name in ["counter", "increment-counter"] {
                let file = root.join(format!("api/web/v1/{name}.md"));
                let text = fs::read_to_string(&file)
                    .unwrap()
                    .replace("/web/v1/counter", "/web/v1/{slug}/counter")
                    .replace("tenant: default-slug", "tenant: path.slug");
                fs::write(file, text).unwrap();
            }
        }
        let manifest = build(&root).unwrap();
        if settings.is_empty() {
            assert_eq!(manifest.route_prefix, "/api");
            assert_eq!(manifest.default_slug.as_deref(), Some("dev"));
        }
        if omit_manifest_fields {
            let json = serde_json::to_value(&manifest).unwrap();
            assert!(json.get("default_slug").is_none());
            assert!(json.get("route_prefix").is_none());
        }
        assert_eq!(
            request(state(&root, manifest), "GET", path).await.0,
            StatusCode::OK
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn runtime_startup_and_verify_reject_extra_slugs() {
    let root = fixture();
    build(&root).unwrap();
    fs::create_dir_all(root.join(".vibe")).unwrap();
    fs::write(
        root.join(".vibe/slugs.json"),
        r#"{"acme":{"tenant_id":"acme0001"},"other":{"tenant_id":"other001"}}"#,
    )
    .unwrap();
    for args in [
        vec!["verify", "--artifact", "manifest.json"],
        vec!["run", "--artifact", "manifest.json", "--db", "memory"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_vibe-runtime"))
            .current_dir(&root)
            .env_clear()
            .env("VIBE_ENV", "dev")
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("extra slugs other"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn credential_tenant_must_match_default_tenant() {
    let root = fixture();
    let mut manifest = build(&root).unwrap();
    let route = manifest
        .routes
        .iter_mut()
        .find(|r| r.method == "GET")
        .unwrap();
    route.public = false;
    route.auth_removed = false;
    route.auth_mode = "api-key".into();
    let mut state = Arc::try_unwrap(state(&root, manifest)).ok().unwrap();
    state.auth.pepper = b"test-pepper".to_vec();
    state.auth.env_name = "test".into();
    state.auth.api_keys.push(vibe::runtime::StoredKey {
        key_id: "webreadkeyxx".into(),
        secret_hash: vibe::runtime::hash_api_secret(b"test-pepper", "test-secret"),
        env: "test".into(),
        tenant_id: "other001".into(),
        scopes: vec!["web:read".into()],
        expires_unix: chrono::Utc::now().timestamp() + 3600,
        revoked: false,
    });
    for (tenant, expected) in [
        ("other001", StatusCode::UNAUTHORIZED),
        ("acme0001", StatusCode::OK),
    ] {
        state.auth.api_keys[0].tenant_id = tenant.into();
        let shared = Arc::new(state);
        let response = router(shared.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/web/v1/counter")
                    .header("X-API-Key", "mbk_test_webreadkeyxx_test-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        state = Arc::try_unwrap(shared).ok().unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn natural_locks_and_cache_track_mode_but_not_prefix() {
    let root = fixture();
    fs::remove_dir_all(root.join("models")).unwrap();
    fs::remove_file(root.join("api/web/v1/counter.md")).unwrap();
    fs::remove_file(root.join("api/web/v1/increment-counter.md")).unwrap();
    fs::write(root.join("api/web/v1/hello.md"), "---\npublic: true\nauth: removed\n---\n# Hello\nA public GET endpoint returning {\"message\":\"hello world\"}.\n").unwrap();
    let compile = |fake: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vibe"));
        command.current_dir(&root).env_clear().arg("build");
        if fake {
            command.env("LLM_PROVIDER", "fake").env("CI", "true");
        }
        command.output().unwrap()
    };
    assert!(compile(true).status.success());
    let lock_path = root.join("api/web/v1/hello.lock.md");
    let default_lock = fs::read_to_string(&lock_path).unwrap();
    assert!(default_lock.contains("tenant: default-slug"));
    assert!(!default_lock.contains("{slug}"));
    fs::write(
        root.join("api/_project.md"),
        "## Routes\n- prefix: /v2\n- default-slug: acme\n",
    )
    .unwrap();
    // Fake locks require fake configuration even for reuse; bytes must stay identical.
    assert!(compile(true).status.success());
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), default_lock);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.routes[0].path, "/v2/web/v1/hello");
    fs::write(
        root.join("api/_project.md"),
        "## Routes\n- prefix: /api\n- default-slug: (none)\n",
    )
    .unwrap();
    assert!(!compile(false).status.success());
    assert!(compile(true).status.success());
    let slug_lock = fs::read_to_string(&lock_path).unwrap();
    assert!(slug_lock.contains("/{slug}/hello"));
    assert!(slug_lock.contains("tenant: path.slug"));
    assert_ne!(slug_lock, default_lock);
    fs::write(
        root.join("api/_project.md"),
        "## Routes\n- default-slug: acme\n",
    )
    .unwrap();
    assert!(!compile(false).status.success());
    assert!(compile(true).status.success());
    assert_eq!(fs::read_to_string(lock_path).unwrap(), default_lock);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn dev_memory_maps_default_slug_and_missing_slug_is_otherwise_unknown() {
    let root = fixture();
    fs::remove_file(root.join("slugs.example.json")).unwrap();
    let manifest = build(&root).unwrap();
    assert_eq!(
        request(state(&root, manifest), "GET", "/api/web/v1/counter")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let prepared = vibe::run::prepare(&vibe::run::RunOptions {
        pins: Default::default(),
        project: root.clone(),
        source: vibe::run::RunSource::Project,
        addr: "127.0.0.1:0".into(),
        db: vibe::run::DbChoice::Memory,
        allow_test_build: false,
        allow_destructive_migrations: false,
    })
    .await
    .unwrap();
    for (method, expected) in [("GET", StatusCode::OK), ("POST", StatusCode::NOT_FOUND)] {
        let response = prepared
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/web/v1/counter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn signed_manifest_rejects_inconsistent_prefix_and_signature_covers_settings() {
    let root = fixture();
    let original = build(&root).unwrap();
    let key = vibe::sign::load_dev_key(&root).unwrap();
    for change in ["prefix", "default"] {
        let mut manifest = original.clone();
        if change == "prefix" {
            manifest.route_prefix = "/other".into();
        } else {
            manifest.default_slug = Some("other".into());
        }
        assert!(
            vibe::manifest::verify_manifest(manifest, &key.verifying_key())
                .unwrap_err()
                .to_string()
                .contains("signature")
        );
    }
    let mut manifest = original;
    manifest.routes[0].path = "/outside/web/v1/counter".into();
    manifest.signature.clear();
    manifest.signature = vibe::sign::sign_bytes(&serde_json::to_vec(&manifest).unwrap(), &key);
    assert!(
        vibe::manifest::verify_manifest(manifest, &key.verifying_key())
            .unwrap_err()
            .to_string()
            .contains("route_prefix")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn existing_lock_with_slug_conflicts_with_default_mode() {
    let root = fixture();
    build(&root).unwrap();
    let path = root.join("api/web/v1/counter.lock.md");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(
        path,
        text.replace("/web/v1/counter", "/web/v1/{slug}/counter"),
    )
    .unwrap();
    let errors = vibe::validate::validate_project(&vibe::validate::ValidateOptions {
        root: &root,
        deploy: false,
        dev: true,
    })
    .unwrap_err();
    assert!(errors.to_string().contains("route uses {slug}"));
    fs::remove_dir_all(root).unwrap();
}

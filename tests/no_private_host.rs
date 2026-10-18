#![cfg(feature = "sdk")]

use std::{fs, path::Path, process::Command};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BODY: &str = "## Table\n- provider: main\n- table: products\n## Fields\n| name | type | rules | system |\n| --- | --- | --- | --- |\n| sku | string | required, unique, max: 40 | no |\n## CRUD\n| method | options |\n| --- | --- |\n| create | - |\n| get | - |\n## Policies\n- rls: enable, force\n- policy: tenant_id = current_setting('app.tenant_id', false)\n";

fn scan(root: &Path, token: &str) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        // Decision 180's sole generated-file exception: local cache keys may
        // derive from the host. These files are never committed or packaged.
        if path.ends_with(".vibe/cache") {
            continue;
        }
        if path.is_dir() {
            scan(&path, token);
        } else {
            let bytes = fs::read(&path).unwrap();
            let count = bytes
                .windows(token.len())
                .filter(|w| *w == token.as_bytes())
                .count();
            assert_eq!(count, 0, "host occurs {count} times in {}", path.display());
        }
    }
}

fn assert_compilers(raw: &str, manifest: bool) {
    let json: serde_json::Value = serde_json::from_str(raw).unwrap();
    let compilers = if manifest {
        &json["provenance"]["compilers"]
    } else {
        &json["compilers"]
    };
    let compilers = compilers.as_object().unwrap();
    assert!(!compilers.is_empty());
    for stamp in compilers.values() {
        assert_eq!(stamp["endpoint-private"], true);
        assert!(stamp.get("endpoint-host").is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_cli_build_never_records_private_host() {
    // Wildcard .localhost is not resolved by this environment's DNS path.
    // endpoint_host is host-only (no port), so scan for that exact token.
    #[cfg(target_os = "linux")]
    let token = {
        let random = uuid::Uuid::new_v4();
        let bytes = random.as_bytes();
        format!(
            "127.{}.{}.{}",
            100 + bytes[0] % 151,
            1 + bytes[1] % 250,
            1 + bytes[2] % 250
        )
    };
    #[cfg(not(target_os = "linux"))]
    let token = "127.0.0.1".to_owned();
    let listener = tokio::net::TcpListener::bind((token.as_str(), 0))
        .await
        .unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 8192];
            loop {
                let n = socket.read(&mut chunk).await.unwrap();
                assert_ne!(n, 0);
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = serde_json::json!({"model":"gpt-test-2026-01-01","choices":[{"message":{"content":BODY}}]}).to_string();
            let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let tmp = std::env::temp_dir().join(format!("vibe-private-probe-{}", uuid::Uuid::new_v4()));
    let project = tmp.join("project");
    let out = tmp.join("out");
    fs::create_dir_all(project.join("models/product")).unwrap();
    fs::create_dir_all(project.join("providers")).unwrap();
    fs::create_dir_all(&out).unwrap();
    fs::write(project.join(".gitignore"), ".env\n.vibe/\n").unwrap();
    fs::write(
        project.join("providers/main.md"),
        include_str!("fixtures/shop/providers/main.md"),
    )
    .unwrap();
    fs::write(
        project.join("models/product/product.md"),
        "# product\nStaff can create a product from a sku.\n",
    )
    .unwrap();
    for output in [None, Some(out.join("manifest.json"))] {
        if output.is_some() {
            // Reuse a legacy record: both in-memory provenance construction and
            // the subsequent CLI rewrite must sanitize it without DNS/LLM work.
            let path = project.join(".vibe/build-record.json");
            let mut record: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            for stamp in record["compilers"].as_object_mut().unwrap().values_mut() {
                stamp["endpoint-host"] = token.clone().into();
            }
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            let key = vibe::sign::load_dev_key(&project).unwrap();
            let manifest =
                vibe::manifest::manifest_from_project_with_mode(&project, &key, true).unwrap();
            assert_compilers(&serde_json::to_string(&manifest).unwrap(), true);
            vibe::manifest::verify_manifest(manifest, &key.verifying_key()).unwrap();
        }

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
        cmd.env_clear()
            .current_dir(&project)
            .args(["build"])
            .env("GITHUB_SHA", "0000000000000000000000000000000000000000")
            .env("LLM_PROVIDER", "openai")
            .env("LLM_BASE_URL", &base)
            .env("LLM_API_KEY", "test-only")
            .env("LLM_MODEL", "gpt-test-2026-01-01")
            .env("LLM_ALLOW_PRIVATE_HOST", "1")
            .env("LLM_MAX_RETRIES", "0");
        if let Some(output) = output {
            cmd.arg("-o").arg(output);
        }
        let result = cmd.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        scan(&project, &token);
        scan(&out, &token);
        assert_compilers(
            &fs::read_to_string(project.join(".vibe/build-record.json")).unwrap(),
            false,
        );
    }
    for path in [project.join("manifest.json"), out.join("manifest.json")] {
        assert_compilers(&fs::read_to_string(path).unwrap(), true);
    }
    assert!(hits.load(std::sync::atomic::Ordering::SeqCst) > 0);
    server.abort();
    fs::remove_dir_all(tmp).unwrap();
}

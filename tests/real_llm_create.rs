#![cfg(feature = "sdk")]

//! Local, paid real-model smoke test. Set LLM_API_KEY, LLM_MODEL, LLM_BASE_URL
//! (and any optional LLM_* settings), then run:
//! `cargo test --test real_llm_create -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Project(PathBuf);
impl Drop for Project {
    fn drop(&mut self) {
        if std::env::var("VIBE_REAL_KEEP").as_deref() == Ok("1") {
            eprintln!("Keeping temp project: {}", self.0.display());
            return;
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Redact API key and base URL values: CLI diagnostics can mention them.
struct Redactor(Vec<String>);
impl Redactor {
    fn new() -> Self {
        let mut values: Vec<_> = std::env::vars()
            .filter(|(name, value)| {
                matches!(name.as_str(), "LLM_API_KEY" | "LLM_BASE_URL") && !value.is_empty()
            })
            .map(|(_, value)| value)
            .collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        Self(values)
    }

    fn clean(&self, text: &str) -> String {
        self.0.iter().fold(text.to_owned(), |text, value| {
            text.replace(value, "[redacted]")
        })
    }
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vibe"));
    command.current_dir(root);
    // LLM settings are inherited unchanged; never put credentials in arguments.
    command.env("VIBE_ENV", "development");
    command
}

fn run(root: &Path, args: &[&str], redact: &Redactor) {
    let output = command(root).args(args).output().expect("start vibe");
    assert!(
        output.status.success(),
        "vibe {} failed:\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        redact.clean(&String::from_utf8_lossy(&output.stdout)),
        redact.clean(&String::from_utf8_lossy(&output.stderr)),
    );
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn locks(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            locks(root, &path, out);
        } else if path.to_string_lossy().ends_with(".lock.md") {
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                fs::read(path).unwrap(),
            );
        }
    }
}

fn route(root: &Path, feature: &str) -> String {
    let text = fs::read_to_string(root.join(format!("api/catalog/v1/{feature}/{feature}.lock.md")))
        .unwrap();
    let doc = vibe::lock::parse_lock(&text).expect("parse feature lock");
    let section = vibe::lock::find_section(&doc.sections, "Route").expect("Route section");
    section
        .get("path")
        .expect("Route path")
        .trim()
        .trim_matches('`')
        .replace("{slug}", "dev")
}

fn server_logs(root: &Path, redact: &Redactor) -> String {
    ["server.stdout", "server.stderr"]
        .iter()
        .map(|path| redact.clean(&fs::read_to_string(root.join(path)).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn feature_locks(root: &Path, redact: &Redactor) -> String {
    ["create-product", "list-products"]
        .iter()
        .map(|feature| {
            let path = format!("api/catalog/v1/{feature}/{feature}.lock.md");
            let text = fs::read_to_string(root.join(&path)).unwrap_or_default();
            format!("{path}:\n{}", redact.clean(&text))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
#[ignore = "requires real LLM credentials and network; run locally only"]
async fn real_model_create_zh_and_en() {
    for name in ["LLM_API_KEY", "LLM_MODEL", "LLM_BASE_URL"] {
        assert!(
            std::env::var_os(name).is_some_and(|value| !value.is_empty()),
            "missing required variable {name}"
        );
    }
    let redact = Redactor::new();
    for zh in [true, false] {
        let project = Project(
            std::env::temp_dir().join(format!("vibe-real-create-{}", uuid::Uuid::new_v4())),
        );
        let root = &project.0;
        write(root, ".gitignore", ".env\n.vibe/cache/\n");
        write(
            root,
            "providers/main.md",
            include_str!("fixtures/shop/providers/main.md"),
        );
        write(root, "api/catalog/_definition.md", "# catalog\n\n## Providers\n\n- allowlist: [main]\n- default: main\n\n## Pipelines\n\n(none)\n\n## Defaults\n\n- timeout: 10s\n- body: 1MB\n");
        write(
            root,
            "api/catalog/v1/_version.md",
            include_str!("fixtures/shop/api/catalog/v1/_version.md"),
        );
        write(
            root,
            ".vibe/slugs.json",
            r#"{"dev":{"tenant_id":"dev00001","public_access":true,"public_write":true}}"#,
        );
        write(
            root,
            "models/product/product.md",
            if zh {
                "# product\n\n商品有 sku（必填，最長 40 字）同 name（必填，最長 120 字），可以新增同列出。\n"
            } else {
                "# product\n\nA product has sku (required, maximum 40 characters) and name (required, maximum 120 characters). Products can be created and listed.\n"
            },
        );
        for (feature, description) in [
            (
                "create-product",
                if zh {
                    "公開 POST 建立商品，唔使登入，body 有 sku 同 name，成功回 201 同新商品。路徑 /catalog/v1/{slug}/products。"
                } else {
                    "Public POST to create a product, without login. The body has sku and name. Return 201 and the new product. Path /catalog/v1/{slug}/products."
                },
            ),
            (
                "list-products",
                if zh {
                    "公開 GET 列出商品，唔使登入。路徑 /catalog/v1/{slug}/products。"
                } else {
                    "Public GET to list products, without login. Path /catalog/v1/{slug}/products."
                },
            ),
        ] {
            write(
                root,
                &format!("api/catalog/v1/{feature}/{feature}.md"),
                &format!("---\npublic: true\nauth: none\n---\n\n# {feature}\n\n{description}\n"),
            );
        }
        let mut runs = Vec::new();
        for _ in 0..3 {
            let cache = root.join(".vibe/cache");
            if cache.exists() {
                fs::remove_dir_all(cache).unwrap();
            }
            run(root, &["build"], &redact);
            let mut snapshot = BTreeMap::new();
            locks(root, root, &mut snapshot);
            assert!(!snapshot.is_empty(), "compile produced no locks");
            runs.push(snapshot);
        }
        for (path, first) in &runs[0] {
            let identical = runs
                .iter()
                .filter(|run| run.get(path) == Some(first))
                .count();
            eprintln!(
                "{}: {} identical {identical}/3",
                if zh { "zh" } else { "en" },
                path.display()
            );
            if let Some(other) = runs.iter().skip(1).find(|run| run.get(path) != Some(first)) {
                eprintln!("--- run 1\n+++ first differing run");
                let before = String::from_utf8_lossy(first);
                let after = other
                    .get(path)
                    .map(|bytes| String::from_utf8_lossy(bytes))
                    .unwrap_or_default();
                let before: Vec<_> = before.lines().collect();
                let after: Vec<_> = after.lines().collect();
                for index in 0..before.len().max(after.len()) {
                    if before.get(index) != after.get(index) {
                        eprintln!("@@ line {} @@", index + 1);
                        if let Some(line) = before.get(index) {
                            eprintln!("-{}", redact.clean(line));
                        }
                        if let Some(line) = after.get(index) {
                            eprintln!("+{}", redact.clean(line));
                        }
                    }
                }
            }
        }
        assert!(
            runs.iter().skip(1).all(|snapshot| snapshot == &runs[0]),
            "locks differ across the three builds ({})",
            if zh { "zh" } else { "en" }
        );
        let create = route(root, "create-product");
        let list = route(root, "list-products");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut server = Server(
            command(root)
                .args(["run", "--db", "memory", "--addr", &addr.to_string()])
                .stdout(Stdio::from(
                    fs::File::create(root.join("server.stdout")).unwrap(),
                ))
                .stderr(Stdio::from(
                    fs::File::create(root.join("server.stderr")).unwrap(),
                ))
                .spawn()
                .expect("start server"),
        );
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let base = format!("http://{addr}");
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if client.get(format!("{base}{list}")).send().await.is_ok() {
                break;
            }
            assert!(
                server.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "server not ready:\n{}",
                server_logs(root, &redact)
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let response = client
            .post(format!("{base}{create}"))
            .json(
                &serde_json::json!({"sku": "SKU-1", "name": if zh { "測試商品" } else { "Test" }}),
            )
            .send()
            .await
            .expect("POST create");
        let status = response.status();
        let body = response.text().await.expect("create body");
        eprintln!("POST {status}: {}", redact.clean(&body));
        assert!(
            status.as_u16() == 201,
            "create failed: {}\n{}\n{}",
            redact.clean(&body),
            server_logs(root, &redact),
            feature_locks(root, &redact)
        );
        let response = client
            .get(format!("{base}{list}"))
            .send()
            .await
            .expect("GET list");
        let status = response.status();
        let body = response.text().await.expect("list body");
        eprintln!("GET {status}: {}", redact.clean(&body));
        assert!(
            status.as_u16() == 200 && body.contains("SKU-1"),
            "list failed: {}\n{}",
            redact.clean(&body),
            feature_locks(root, &redact)
        );
    }
}

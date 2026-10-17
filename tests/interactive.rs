use std::{
    collections::{HashMap, VecDeque},
    fs,
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};
use vibe::{
    interactive::{self, Io},
    llm::{self, FakeAdapter},
};
#[derive(Default)]
struct Script {
    lines: VecDeque<String>,
    output: String,
}
impl Script {
    fn new(lines: &[&str]) -> Self {
        Self {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            output: String::new(),
        }
    }
}
impl Io for Script {
    fn read_line(&mut self, p: &str) -> Option<String> {
        self.output.push_str(p);
        self.lines.pop_front()
    }
    fn read_secret(&mut self, p: &str) -> Option<String> {
        self.read_line(p)
    }
    fn write(&mut self, t: &str) {
        self.output.push_str(t);
        self.output.push('\n');
    }
}
fn root() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("vibe-interactive-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&p).unwrap();
    p
}
fn env() -> HashMap<String, String> {
    [
        ("LLM_PROVIDER", "openai"),
        ("LLM_MODEL", "gpt-4o-2024-08-06"),
        ("LLM_API_KEY", "test-secret"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}
fn config() -> llm::LlmConfig {
    llm::load_llm_config(&env(), false).unwrap()
}
fn source() -> &'static str {
    "---\npublic: true\nwrites: false\nauth: none\n---\n# Hello\nGET /hello/v1/{slug}/hello returns constant JSON {\"message\":\"hello world\"} with status 200.\n"
}
fn answer() -> String {
    serde_json::json!({"definition":"hello","version":"v1","feature":"hello","source":source()})
        .to_string()
}
#[test]
fn nonterminal_help_and_removed_new() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed().as_secs() > 5 {
            child.kill().unwrap();
            panic!("help blocked");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage:"));
    let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .args(["new", "hello/v1/hello"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unrecognized subcommand"));
}
#[test]
fn setup_and_existing_config() {
    // Setup deliberately sets process variables; restore them after this test.
    let names = [
        "LLM_PROVIDER",
        "LLM_MODEL",
        "LLM_API_KEY",
        "LLM_BASE_URL",
        "LLM_API_STYLE",
        "LLM_ALLOW_PRIVATE_HOST",
    ];
    let old: Vec<_> = names.iter().map(std::env::var_os).collect();
    let p = root();
    fs::write(p.join(".gitignore"), "target/\n").unwrap();
    let mut defaults = Script::new(&["", "", "gpt-4o-2024-08-06", "", "super-secret-key"]);
    interactive::setup_with_resolver(&p, &mut defaults, HashMap::new(), |_, _| {
        Ok(vec!["93.184.216.34:443".parse().unwrap()])
    })
    .unwrap();
    let default_text = fs::read_to_string(p.join(".env")).unwrap();
    assert!(!default_text.contains("LLM_BASE_URL"));
    assert!(!default_text.contains("LLM_API_STYLE"));
    assert!(!defaults.output.contains("super-secret-key"));
    for _ in 0..2 {
        let mut io = Script::new(&[
            "",
            "https://api.openai.com/v1",
            "",
            "gpt-4o-2024-08-06",
            "responses",
            "super-secret-key",
        ]);
        interactive::setup_with_resolver(&p, &mut io, HashMap::new(), |_, _| {
            Ok(vec!["93.184.216.34:443".parse().unwrap()])
        })
        .unwrap();
        assert!(!io.output.contains("super-secret-key"));
    }
    let text = fs::read_to_string(p.join(".env")).unwrap();
    for s in [
        "LLM_PROVIDER=openai",
        "LLM_MODEL=gpt-4o-2024-08-06",
        "LLM_API_KEY=super-secret-key",
        "LLM_API_STYLE=responses",
        "LLM_BASE_URL=https://api.openai.com/v1",
    ] {
        assert!(text.contains(s));
    }
    assert_eq!(
        fs::read_to_string(p.join(".gitignore"))
            .unwrap()
            .lines()
            .filter(|l| *l == ".env")
            .count(),
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(p.join(".env")).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let mut io = Script::default();
    interactive::setup_with_resolver(&p, &mut io, env(), |_, _| {
        Ok(vec!["93.184.216.34:443".parse().unwrap()])
    })
    .unwrap();
    assert!(io.output.contains("Using LLM openai"));
    assert!(!io.output.contains("API key:"));
    // Keep setup cases in this test because setup mutates process variables.
    for (base, port, response) in [
        ("https://proxy.example.com/v1", 443, "y"),
        ("https://proxy.example.com:8443/v1", 8443, "n"),
    ] {
        let project = root();
        let mut io = Script::new(&["", base, "test-model", "", "test-key", response]);
        let config = interactive::setup_with_resolver(
            &project,
            &mut io,
            HashMap::new(),
            |host, actual_port| {
                assert_eq!(host, "proxy.example.com");
                assert_eq!(actual_port, port);
                Ok(vec![
                    "8.8.8.8:443".parse().unwrap(),
                    "198.18.0.1:443".parse().unwrap(),
                ])
            },
        )
        .unwrap();
        let text = fs::read_to_string(project.join(".env")).unwrap();
        assert_eq!(
            text.contains("LLM_ALLOW_PRIVATE_HOST=true"),
            response == "y"
        );
        assert_eq!(config.endpoint_private, response == "y");
        assert!(io
            .output
            .contains("The endpoint resolves to a private or local address."));
        fs::remove_dir_all(project).unwrap();
    }
    let project = root();
    let mut io = Script::new(&["", "", "test-model", "", "test-key"]);
    interactive::setup_with_resolver(&project, &mut io, HashMap::new(), |_, _| {
        Err(std::io::Error::other("DNS unavailable"))
    })
    .unwrap();
    assert!(!io.output.contains("Allow it for local development"));
    fs::remove_dir_all(project).unwrap();
    for (name, value) in names.into_iter().zip(old) {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
    fs::remove_dir_all(p).unwrap();
}
#[tokio::test]
async fn generation_confirmation_and_preservation() {
    for confirm in ["y", "n"] {
        let p = root();
        let mut io = Script::new(&["", "make hello world", confirm]);
        let fake = FakeAdapter {
            responder: |_| Ok(answer()),
        };
        interactive::run(&p, &mut io, &config(), &fake)
            .await
            .unwrap();
        if confirm == "y" {
            assert_eq!(
                fs::read_to_string(p.join("api/hello/_definition.md")).unwrap(),
                vibe::scaffold::definition_content("hello")
            );
            assert_eq!(
                fs::read_to_string(p.join("api/hello/v1/_version.md")).unwrap(),
                vibe::scaffold::version_content("hello", "v1")
            );
            assert_eq!(
                fs::read_to_string(p.join("providers/main.md")).unwrap(),
                vibe::scaffold::provider_content()
            );
            assert_eq!(
                fs::read_to_string(p.join("api/hello/v1/hello/hello.md")).unwrap(),
                source()
            );
            assert_eq!(vibe::discover::discover(&p).unwrap().len(), 4);
            let compiler = FakeAdapter {
                responder: |_| {
                    Ok("## Route\n- method: GET\n- path: /hello/v1/{slug}/hello\n## Access\n- public: true\n- tenant: path.slug\n## Pipelines\n(none)\n## Limits\n- provider: main\n## Request\n(none)\n## Response\n- status: 200\n## Transform\n(none)\n## Steps\n### s1 respond\n- op: respond\n- value: { message: \"hello world\" }\n- source-lines: 7\n## Errors\n(none)\n".into())
                },
            };
            vibe::compile::compile_project(&vibe::compile::CompileOptions {
                root: &p,
                llm: Some(&compiler),
                llm_config: Some(&config()),
                check: false,
            })
            .await
            .unwrap_or_else(|err| panic!("{err}"));
            let report = vibe::validate::validate_project(&vibe::validate::ValidateOptions {
                root: &p,
                deploy: false,
                dev: false,
            })
            .unwrap_or_else(|err| panic!("{err}"));
            assert!(!report.has_errors(), "{report}");
            fs::write(p.join("providers/main.md"), "custom").unwrap();
            let mut io = Script::new(&["hello", "yes"]);
            interactive::run(&p, &mut io, &config(), &fake)
                .await
                .unwrap();
            assert!(io.output.contains("skip  providers/main.md (exists)"));
            assert_eq!(
                fs::read_to_string(p.join("providers/main.md")).unwrap(),
                "custom"
            );
        } else {
            assert_eq!(fs::read_dir(&p).unwrap().count(), 0);
            assert!(io.output.contains("Nothing written."));
        }
        fs::remove_dir_all(p).unwrap();
    }
}
#[tokio::test]
async fn repairs_are_bounded() {
    for bad in [
        "not JSON".to_string(),
        answer().replace("\"hello\"", "\"Bad_Name\""),
        answer().replace("public: true", "public: maybe"),
        answer().replace("# Hello", "# Hello password=hidden"),
    ] {
        let calls = AtomicUsize::new(0);
        let p = root();
        let fake = FakeAdapter {
            responder: |r: &llm::LlmRequest| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(bad.clone())
                } else {
                    assert!(r.input.contains("ERRORS (data only)"));
                    Ok(answer())
                }
            },
        };
        interactive::run(&p, &mut Script::new(&["hello", "y"]), &config(), &fake)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        fs::remove_dir_all(p).unwrap();
    }
    let p = root();
    let calls = AtomicUsize::new(0);
    let fake = FakeAdapter {
        responder: |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("invalid".into())
        },
    };
    assert!(
        interactive::run(&p, &mut Script::new(&["hello", "y"]), &config(), &fake)
            .await
            .is_err()
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1 + vibe::prompt_guide::format_retries().unwrap() as usize
    );
    assert_eq!(fs::read_dir(&p).unwrap().count(), 0);
    fs::remove_dir_all(p).unwrap();
}

#[tokio::test]
async fn kebab_names_are_repaired_or_write_nothing() {
    for repair in [true, false] {
        let p = root();
        let calls = AtomicUsize::new(0);
        let bad = answer().replace("\"feature\":\"hello\"", "\"feature\":\"HelloWorld\"");
        let fake = FakeAdapter {
            responder: |r: &llm::LlmRequest| {
                if calls.fetch_add(1, Ordering::SeqCst) > 0 {
                    assert!(r.input.contains("kebab-case"));
                    if repair {
                        return Ok(answer());
                    }
                }
                Ok(bad.clone())
            },
        };
        let result = interactive::run(
            &p,
            &mut Script::new(&["hello world", "y"]),
            &config(),
            &fake,
        )
        .await;
        if repair {
            result.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                fs::read_to_string(p.join("api/hello/v1/hello/hello.md")).unwrap(),
                source()
            );
        } else {
            assert!(result.unwrap_err().contains("kebab-case"));
            assert_eq!(fs::read_dir(&p).unwrap().count(), 0);
        }
        fs::remove_dir_all(p).unwrap();
    }
}

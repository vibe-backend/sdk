#![cfg(feature = "sdk")]

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
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("`vibe new` was removed"));
    assert!(error.contains("run `vibe` with no arguments"));
    let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("Running vibe with no arguments in a terminal starts interactive setup."));
    assert!(!help
        .lines()
        .any(|l| l.trim_start().starts_with("new ") || l.trim_start().starts_with("approve ")));
    let out = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .args(["build", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--release"));
    assert!(!help.contains("--dev"));
}
#[test]
fn setup_and_existing_config() {
    let p = root();
    let mut settings = HashMap::from([(
        "XDG_CONFIG_HOME".into(),
        p.join("config").to_string_lossy().into_owned(),
    )]);
    let mut io = Script::new(&["", "", "gpt-4o-2024-08-06", "", "super-secret-key"]);
    let names = [
        "LLM_PROVIDER",
        "LLM_MODEL",
        "LLM_API_KEY",
        "LLM_BASE_URL",
        "LLM_API_STYLE",
        "LLM_API_KEY_FILE",
        "LLM_ALLOW_PRIVATE_HOST",
    ];
    let old: Vec<_> = names.iter().map(std::env::var_os).collect();
    interactive::setup_with_resolver(&p, &mut io, settings.clone(), |_, _| Ok(Vec::new())).unwrap();
    assert!(!io.output.contains("super-secret-key"));
    assert!(!p.join(".env").exists());
    let text = fs::read_to_string(p.join("config/vibe/llm.env")).unwrap();
    assert!(text.contains("LLM_API_KEY_FILE="));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for (path, mode) in [
            ("config/vibe", 0o700),
            ("config/vibe/llm.env", 0o600),
            ("config/vibe/llm-api-key", 0o600),
        ] {
            assert_eq!(
                fs::metadata(p.join(path)).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
    }
    assert!(!text.contains("super-secret-key"));
    let mut io = Script::default();
    settings.extend(env());
    interactive::setup_with_resolver(&p, &mut io, settings, |_, _| Ok(Vec::new())).unwrap();
    // Existing project settings are preserved, including the bytes surrounding them.
    let project = root();
    let original = "# local settings\nLLM_PROVIDER=openai\nLLM_MODEL=gpt-4o-2024-08-06\nLLM_API_KEY=project-secret\n";
    fs::write(project.join(".env"), original).unwrap();
    let settings = HashMap::from([(
        "XDG_CONFIG_HOME".into(),
        project.join("config").to_string_lossy().into_owned(),
    )]);
    let mut io = Script::default();
    interactive::setup_with_resolver(&project, &mut io, settings, |_, _| Ok(Vec::new())).unwrap();
    assert!(fs::read_to_string(project.join(".env")).unwrap() == original);
    assert!(!io.output.contains("project-secret"));
    assert!(!project.join("config").exists());
    fs::remove_dir_all(project).unwrap();
    // A private-host opt-in remains project scoped and uses the existing append logic.
    for base in ["http://127.0.0.1:11434/v1", "https://example.com/v1"] {
        let project = root();
        fs::write(project.join(".env"), "# keep this comment").unwrap();
        fs::write(project.join(".gitignore"), "target/\n").unwrap();
        let settings = HashMap::from([(
            "XDG_CONFIG_HOME".into(),
            project.join("config").to_string_lossy().into_owned(),
        )]);
        let mut io = Script::new(&["", base, "test-model", "responses", "private-secret", "yes"]);
        interactive::setup_with_resolver(&project, &mut io, settings.clone(), |_, _| {
            Ok(vec!["127.0.0.1:443".parse().unwrap()])
        })
        .unwrap();
        let text = fs::read_to_string(project.join(".env")).unwrap();
        assert!(text.starts_with("# keep this comment\n"));
        assert!(text.contains("LLM_ALLOW_PRIVATE_HOST=true"));
        assert!(!text.contains("private-secret"));
        assert!(!text.contains("LLM_MODEL"));
        assert!(!io.output.contains("private-secret"));
        let global = fs::read_to_string(project.join("config/vibe/llm.env")).unwrap();
        assert!(global.contains("LLM_API_STYLE=responses"));
        assert!(!global.contains("LLM_ALLOW_PRIVATE_HOST"));
        let mut io = Script::default();
        interactive::setup_with_resolver(&project, &mut io, settings, |_, _| Ok(Vec::new()))
            .unwrap();
        assert_eq!(
            fs::read_to_string(project.join(".gitignore"))
                .unwrap()
                .lines()
                .filter(|l| *l == ".env")
                .count(),
            1
        );
        fs::remove_dir_all(project).unwrap();
    }
    for blank in ["", "   "] {
        let project = root();
        let original = format!("LLM_PROVIDER=openai\nLLM_MODEL=m\nLLM_API_KEY={blank}\n");
        fs::write(project.join(".env"), &original).unwrap();
        let mut io = Script::new(&["replacement-secret"]);
        let config = interactive::setup_with_resolver(
            &project,
            &mut io,
            HashMap::from([(
                "XDG_CONFIG_HOME".into(),
                project.join("config").to_string_lossy().into_owned(),
            )]),
            |_, _| Ok(vec!["93.184.216.34:443".parse().unwrap()]),
        )
        .unwrap();
        assert_eq!(config.api_key, "replacement-secret");
        assert!(io.lines.is_empty());
        assert!(!project.join("config").exists());
        assert_eq!(
            fs::read_to_string(project.join(".gitignore"))
                .unwrap()
                .lines()
                .filter(|line| *line == ".env")
                .count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(project.join(".env"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(io.output.contains("API key:"));
        for prompt in [
            "Provider",
            "Model:",
            "Base URL",
            "API style",
            "replacement-secret",
        ] {
            assert!(!io.output.contains(prompt));
        }
        let bytes = fs::read(project.join(".env")).unwrap();
        assert!(bytes.starts_with(original.as_bytes()));
        assert!(String::from_utf8(bytes).unwrap()[original.len()..]
            .contains("LLM_API_KEY=replacement-secret\n"));
    }
    // Only the missing key is requested; existing model and original bytes win.
    for model in ["gpt-4o-2024-08-06", "existing-model"] {
        let project = root();
        let original = format!("# mine\nLLM_PROVIDER=openai\nLLM_MODEL={model}\n");
        fs::write(project.join(".env"), &original).unwrap();
        let mut io = Script::new(&["partial-secret"]);
        let config = interactive::setup_with_resolver(
            &project,
            &mut io,
            HashMap::from([(
                "XDG_CONFIG_HOME".into(),
                project.join("config").to_string_lossy().into_owned(),
            )]),
            |_, _| Ok(vec!["93.184.216.34:443".parse().unwrap()]),
        )
        .unwrap();
        assert_eq!(config.provider, "openai");
        assert_eq!(config.model, model);
        assert!(io.output.contains("API key:"));
        for prompt in [
            "Provider",
            "Base URL",
            "Model:",
            "API style",
            "partial-secret",
        ] {
            assert!(!io.output.contains(prompt));
        }
        assert!(io.lines.is_empty());
        assert!(!project.join("config").exists());
        assert_eq!(
            fs::read_to_string(project.join(".gitignore"))
                .unwrap()
                .lines()
                .filter(|line| *line == ".env")
                .count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(project.join(".env"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let text = fs::read_to_string(project.join(".env")).unwrap();
        assert!(text.starts_with(&original));
        for key in ["LLM_PROVIDER", "LLM_MODEL", "LLM_API_KEY"] {
            assert_eq!(
                text.lines()
                    .filter(|line| line.starts_with(&format!("{key}=")))
                    .count(),
                1
            );
        }
        fs::remove_dir_all(project).unwrap();
    }
    // Process-supplied values are never copied into .env; append preserves a
    // missing final newline and the first occurrence of an existing key.
    let project = root();
    let original = "# mine\nLLM_PROVIDER=openai\nLLM_MODEL=first-model\nLLM_MODEL=second-model";
    fs::write(project.join(".env"), original).unwrap();
    let supplied = HashMap::from([
        ("LLM_API_KEY".into(), "process-secret".into()),
        (
            "XDG_CONFIG_HOME".into(),
            project.join("config").to_string_lossy().into_owned(),
        ),
    ]);
    let mut io = Script::default();
    let config = interactive::setup_with_resolver(&project, &mut io, supplied, |_, _| {
        panic!("usable config must skip DNS lookup")
    })
    .unwrap();
    assert_eq!(config.model, "first-model");
    assert_eq!(fs::read_to_string(project.join(".env")).unwrap(), original);
    assert!(!io.output.contains("wrote .env"));
    assert!(!io.output.contains("process-secret"));
    fs::write(
        project.join(".env"),
        "# mine\nLLM_PROVIDER=openai\nLLM_MODEL=first-model",
    )
    .unwrap();
    let mut io = Script::new(&["new-secret"]);
    interactive::setup_with_resolver(
        &project,
        &mut io,
        HashMap::from([(
            "XDG_CONFIG_HOME".into(),
            project.join("config").to_string_lossy().into_owned(),
        )]),
        |_, _| Ok(vec!["93.184.216.34:443".parse().unwrap()]),
    )
    .unwrap();
    assert!(fs::read_to_string(project.join(".env"))
        .unwrap()
        .starts_with("# mine\nLLM_PROVIDER=openai\nLLM_MODEL=first-model\n# vibe LLM settings\n"));
    fs::remove_dir_all(project).unwrap();
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
            HashMap::from([(
                "XDG_CONFIG_HOME".into(),
                project.join("config").to_string_lossy().into_owned(),
            )]),
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
        let text = fs::read_to_string(project.join(".env")).unwrap_or_default();
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
    interactive::setup_with_resolver(
        &project,
        &mut io,
        HashMap::from([(
            "XDG_CONFIG_HOME".into(),
            project.join("config").to_string_lossy().into_owned(),
        )]),
        |_, _| Err(std::io::Error::other("DNS unavailable")),
    )
    .unwrap();
    assert!(!io.output.contains("Allow it for local development"));
    fs::remove_dir_all(project).unwrap();
    // Partial process settings also append only missing values to the project.
    for supplied_key in [false, true] {
        let project = root();
        let mut settings = HashMap::from([
            (
                "XDG_CONFIG_HOME".into(),
                project.join("config").to_string_lossy().into_owned(),
            ),
            ("LLM_PROVIDER".into(), "openai".into()),
        ]);
        let mut io = if supplied_key {
            settings.insert("LLM_API_KEY".into(), "process-secret".into());
            Script::new(&["missing-model"])
        } else {
            settings.insert("LLM_MODEL".into(), "existing-model".into());
            Script::new(&["missing-secret"])
        };
        interactive::setup_with_resolver(&project, &mut io, settings, |_, _| Ok(Vec::new()))
            .unwrap();
        assert!(io.lines.is_empty());
        assert!(!project.join("config").exists());
        assert!(!io.output.contains("process-secret"));
        assert!(!io.output.contains("missing-secret"));
        let text = fs::read_to_string(project.join(".env")).unwrap();
        assert!(!text.contains("LLM_PROVIDER="));
        assert!(!text.contains("process-secret"));
        assert_eq!(text.contains("LLM_MODEL="), supplied_key);
        assert_eq!(text.contains("LLM_API_KEY="), !supplied_key);
        fs::remove_dir_all(project).unwrap();
    }
    // An incomplete global file is an error, without prompting or overwriting it.
    let config_path = p.join("config/vibe/llm.env");
    fs::write(&config_path, "LLM_PROVIDER=openai\n").unwrap();
    let settings = HashMap::from([(
        "XDG_CONFIG_HOME".into(),
        p.join("config").to_string_lossy().into_owned(),
    )]);
    let mut io = Script::default();
    assert!(
        interactive::setup_with_resolver(&p, &mut io, settings, |_, _| Ok(Vec::new())).is_err()
    );
    assert!(io.output.is_empty());
    assert_eq!(
        fs::read_to_string(config_path).unwrap(),
        "LLM_PROVIDER=openai\n"
    );
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
            assert!(!p.join("providers/main.md").exists());
            assert_eq!(
                fs::read_to_string(p.join("api/hello/v1/hello.md")).unwrap(),
                source()
            );
            assert_eq!(vibe::discover::discover(&p).unwrap().len(), 3);
            let compiler = FakeAdapter {
                responder: |_| {
                    Ok("## Route\n- method: GET\n- path: /hello/v1/{slug}/hello\n## Access\n- public: true\n- tenant: path.slug\n## Pipelines\n(none)\n## Limits\n- provider: main\n## Request\n(none)\n## Response\n- status: 200\n## Transform\n(none)\n## Steps\n### s1 respond\n- op: respond\n- value: { message: \"hello world\" }\n- source-lines: 7\n## Errors\n(none)\n".into())
                },
            };
            vibe::compile::regenerate_project(&vibe::compile::CompileOptions {
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
            fs::create_dir_all(p.join("providers")).unwrap();
            fs::write(p.join("providers/main.md"), "custom").unwrap();
            let mut io = Script::new(&["hello", "yes"]);
            interactive::run(&p, &mut io, &config(), &fake)
                .await
                .unwrap();

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
async fn project_route_rule_is_sent_with_initial_and_repair_requests() {
    for default_slug in [None, Some("acme")] {
        let p = root();
        fs::create_dir_all(p.join("api")).unwrap();
        let settings = default_slug.map_or_else(
            || "## Routes\n- prefix: /api\n".to_string(),
            |slug| format!("## Routes\n- prefix: /api\n- default-slug: {slug}\n"),
        );
        fs::write(p.join("api/_project.md"), settings).unwrap();
        let rule = vibe::project::Routes::load(&p).unwrap().rule();
        let calls = AtomicUsize::new(0);
        let fake = FakeAdapter {
            responder: |r: &llm::LlmRequest| {
                assert!(r.system.contains("input's Project route rule"));
                assert!(r.input.starts_with(&format!("{rule}\nhello")));
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok("invalid JSON".into())
                } else {
                    assert!(r.input.contains("ERRORS (data only)"));
                    Ok(answer())
                }
            },
        };
        interactive::run(&p, &mut Script::new(&["hello", "n"]), &config(), &fake)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
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
                fs::read_to_string(p.join("api/hello/v1/hello.md")).unwrap(),
                source()
            );
        } else {
            assert!(result.unwrap_err().contains("kebab-case"));
            assert_eq!(fs::read_dir(&p).unwrap().count(), 0);
        }
        fs::remove_dir_all(p).unwrap();
    }
}

#[test]
fn dotenv_empty_llm_values_and_precedence() {
    let text = "LLM_API_KEY=\nLLM_API_KEY=   \nLLM_API_KEY=real\nOTHER=\nOTHER=second\nOPENAI_MODEL=  \nOPENAI_MODEL=m\n";
    let merged = llm::merge_dotenv(text, &HashMap::new());
    assert_eq!(merged["LLM_API_KEY"], "real");
    assert_eq!(merged["OPENAI_MODEL"], "m");
    assert_eq!(merged["OTHER"], "");
    for value in ["", "  ", "process"] {
        let current = HashMap::from([("LLM_API_KEY".into(), value.into())]);
        let merged = llm::merge_dotenv(text, &current);
        assert_eq!(
            merged["LLM_API_KEY"],
            if value.trim().is_empty() {
                "real"
            } else {
                "process"
            }
        );
    }
    let mut settings = env();
    for key in [
        "LLM_TIMEOUT",
        "LLM_MAX_RETRIES",
        "LLM_SEED",
        "LLM_BASE_URL",
        "LLM_API_STYLE",
        "LLM_ALLOW_PRIVATE_HOST",
        "LLM_PROVIDER",
        "OPENAI_API_KEY",
    ] {
        settings.insert(key.into(), "   ".into());
    }
    assert!(llm::load_llm_config(&settings, false).is_ok());
}

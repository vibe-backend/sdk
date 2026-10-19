#![cfg(feature = "sdk")]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let e = entry.unwrap();
        if e.path().is_dir() {
            copy(&e.path(), &to.join(e.file_name()));
        } else {
            fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}
fn temp() -> PathBuf {
    let p = std::env::temp_dir().join(format!("vibe-replay-{}", uuid::Uuid::new_v4()));
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nl-counter"),
        &p,
    );
    p
}
fn build(root: &Path, provider: &str, vars: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vibe"));
    c.current_dir(root)
        .env_clear()
        .env("LLM_PROVIDER", provider)
        .arg("build")
        .args(args);
    for (k, v) in vars {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn ok(o: Output) {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
fn locks(root: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, p: &Path, out: &mut Vec<(String, String)>) {
        for e in fs::read_dir(p).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else if p.to_string_lossy().ends_with(".lock.md") {
                out.push((
                    p.strip_prefix(root).unwrap().to_string_lossy().into(),
                    fs::read_to_string(p).unwrap(),
                ));
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort();
    out
}
#[test]
fn record_replay_identical_locks_and_test_build() {
    let a = temp();
    let b = temp();
    ok(build(
        &a,
        "fake",
        &[("VIBE_LLM_RECORD", "fixtures/llm")],
        &[],
    ));
    copy(&a.join("fixtures/llm"), &b.join("fixtures/llm"));
    ok(build(&b, "replay", &[], &[]));
    assert_eq!(locks(&a), locks(&b));
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(b.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["provenance"]["test_build"], true);
    let record = fs::read_to_string(b.join(".vibe/build-record.json")).unwrap();
    assert!(
        record.contains("\"test_build\": true") || record.contains("\"test-build\": true"),
        "{record}"
    );
    fs::remove_dir_all(a).unwrap();
    fs::remove_dir_all(b).unwrap();
}
#[test]
fn replay_misses_list_sources_and_record_command() {
    let p = temp();
    fs::remove_dir_all(p.join("fixtures")).ok();
    let o = build(&p, "replay", &[], &["--check"]);
    let s = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    for source in [
        "models/counter/counter.md",
        "models/counter/methods/read.md",
        "models/counter/methods/increment.md",
        "api/web/v1/counter.md",
        "api/web/v1/increment-counter.md",
    ] {
        assert!(s.contains(source), "missing {source}: {s}");
    }
    assert!(
        s.contains("compile")
            && s.contains("VIBE_LLM_RECORD=fixtures/llm")
            && s.contains("counter.md"),
        "{s}"
    );
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn unused_fixture_check_and_release_refusal() {
    let p = temp();
    ok(build(
        &p,
        "fake",
        &[("VIBE_LLM_RECORD", "fixtures/llm")],
        &[],
    ));
    let k = "a".repeat(64);
    fs::write(p.join(format!("fixtures/llm/{k}.md")), "unused").unwrap();
    let o = build(&p, "replay", &[], &["--check"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("unused LLM fixture {k}")));
    let o = build(&p, "replay", &[], &["--release"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("release build"));
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn recording_and_existing_fixture_scans_redact_secrets() {
    for (name, value, kind) in [
        ("LLM_API_KEY", "## Fields", "API key"),
        ("LLM_BASE_URL", "https://counter.invalid", "base URL"),
        ("LLM_BASE_URL", "https://COUNTER.invalid", "endpoint host"),
    ] {
        let p = temp();
        let dir = p.join("fixtures/llm");
        fs::create_dir_all(&dir).unwrap();
        let planted = if kind == "endpoint host" {
            "counter.invalid"
        } else {
            value
        };
        fs::write(dir.join("planted.md"), planted).unwrap();
        let o = build(&p, "replay", &[(name, value)], &["--check"]);
        let s = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success());
        assert!(s.contains(kind), "{s}");
        assert!(!s.contains(value), "{s}");
        fs::remove_dir_all(p).unwrap();
    }
    let p = temp();
    let o = build(
        &p,
        "fake",
        &[("VIBE_LLM_RECORD", "recorded"), ("LLM_API_KEY", "## ")],
        &[],
    );
    let s = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success());
    assert!(s.contains("API key") && !s.contains("## "), "{s}");
    assert!(!p.join("recorded").exists());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn committed_counter_fixtures_replay_without_credentials() {
    let p = temp();
    ok(build(&p, "replay", &[], &["--check"]));
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn recording_refused_in_ci_even_with_fresh_locks() {
    let p = temp();
    ok(build(&p, "fake", &[], &[]));
    for ci in ["true", "1"] {
        let output = build(
            &p,
            "fake",
            &[("VIBE_LLM_RECORD", "recorded"), ("CI", ci)],
            &[],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("refused in CI"));
    }
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn project_relative_override_and_dotenv_scan() {
    let p = temp();
    fs::rename(p.join("fixtures/llm"), p.join("reviewed")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vibe"))
        .current_dir(std::env::temp_dir())
        .env_clear()
        .env("LLM_PROVIDER", "replay")
        .env("VIBE_LLM_FIXTURES", "reviewed")
        .args(["build", "--check", "--project"])
        .arg(&p)
        .output()
        .unwrap();
    ok(output);
    fs::write(
        p.join(".gitignore"),
        ".env\n*.lock.md\n.vibe/\nmanifest.json\n",
    )
    .unwrap();
    fs::write(
        p.join(".env"),
        "LLM_API_KEY=planted-private-token\nVIBE_LLM_FIXTURES=reviewed\n",
    )
    .unwrap();
    fs::write(p.join("reviewed/planted.md"), "planted-private-token").unwrap();
    for args in [&[][..], &["--check"][..]] {
        let output = build(&p, "replay", &[], args);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(
            error.contains("planted.md") && error.contains("API key"),
            "{error}"
        );
        assert!(!error.contains("planted-private-token"));
    }
    fs::remove_dir_all(p).unwrap();
}

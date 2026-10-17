use std::fs;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand};
use std::io::IsTerminal;
use vibe::compile::{build_project, CompileOptions};
use vibe::llm::{self, LlmAdapter};
use vibe::manifest;
use vibe::run::{check_deployable, load_dotenv, load_manifest};
use vibe::runtime::PgExec;
use vibe::secret::secret_scan;
use vibe::sign;
use vibe::validate::{validate_project, ValidateOptions};

#[derive(Parser)]
#[command(
    name = "vibe",
    about = "Build a markdown-driven backend",
    after_help = "Running vibe with no arguments in a terminal starts interactive setup.\nvibe build generates locks, checks and packages.\nvibe run serves a project or artifact.\nvibe build --release builds a release package."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Generate locks, check and package the project.
    Build {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Build from source in memory and apply the deploy policy without writing locks.
        #[arg(long, conflicts_with = "release")]
        check: bool,
        /// Build an unsigned release package from source; never read .env or committed locks.
        #[arg(long)]
        release: bool,
        /// Output path, relative to the current working directory.
        #[arg(short, long, default_value = "manifest.json")]
        output: PathBuf,
        /// Release version without v (default: GITHUB_REF_NAME of a v* tag).
        #[arg(long)]
        release_version: Option<String>,
        /// Source commit (default: GITHUB_SHA or git rev-parse HEAD).
        #[arg(long)]
        commit: Option<String>,
    },
    /// Run a project or a signed artifact.
    Run {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        artifact: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
        #[arg(long, value_parser = ["memory", "postgres"])]
        db: Option<String>,
        #[arg(long)]
        allow_test_build: bool,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    if let Some(name) = std::env::args().skip(1).find(|arg| !arg.starts_with('-')) {
        if let Some(hint) = removed_command_hint(&name) {
            eprintln!("{hint}");
            std::process::exit(2);
        }
    }
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<(), String> {
    let Some(command) = cli.command else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() || ci_or_prod() {
            Cli::command().print_help().map_err(|e| e.to_string())?;
            return Ok(());
        }
        let project = Path::new(".");
        load_dotenv(project);
        let mut io = TerminalIo;
        let config = vibe::interactive::setup(project, &mut io, llm::env_map_from_process())?;
        let adapter = llm::adapter_for(&config)?;
        return vibe::interactive::run(project, &mut io, &config, adapter.as_ref()).await;
    };
    match command {
        Command::Build {
            project,
            check,
            release,
            output,
            release_version,
            commit,
        } => {
            check_build_inputs(&project)?;
            if release {
                cmd_manifest(&project, &output, true, release_version, commit).await
            } else {
                cmd_build(&project, check).await?;
                if check {
                    return Ok(());
                }
                cmd_validate(&project, true, !ci_or_prod())?;
                cmd_manifest(&project, &output, false, release_version, commit).await
            }
        }
        Command::Run {
            project,
            artifact,
            addr,
            db,
            allow_test_build,
        } => {
            let db = vibe::run::DbChoice::from_arg_or_env(db.as_deref())?;
            vibe::run::run(vibe::run::RunOptions {
                project,
                source: artifact
                    .map(vibe::run::RunSource::Artifact)
                    .unwrap_or(vibe::run::RunSource::Project),
                addr,
                db,
                allow_test_build,
            })
            .await
        }
    }
}

async fn cmd_build(project: &Path, check: bool) -> Result<(), String> {
    if !ci_or_prod() {
        load_dotenv(project);
    }
    if check {
        // Check and release always compile from source, ignoring disk locks.
        let config = llm::load_llm_config(&llm::env_map_from_process(), ci_or_prod()).ok();
        let adapter = config.as_ref().map(|c| llm::adapter_for(c)).transpose()?;
        let output = vibe::compile::regenerate_project(&CompileOptions {
            root: project,
            check: true,
            llm: adapter.as_ref().map(|a| a.as_ref() as &dyn LlmAdapter),
            llm_config: config.as_ref(),
        })
        .await
        .map_err(|e| e.to_string())?;
        print_report(&output.report);
        return Ok(());
    }
    let output = build_project(&CompileOptions {
        root: project,
        check,
        llm: None,
        llm_config: None,
    })
    .await
    .map_err(|e| e.to_string())?;
    print_report(&output.report);
    println!(
        "built {} locks ({} regenerated, {} up to date)",
        output.files.len(),
        output.files.len() - output.reused,
        output.reused
    );
    Ok(())
}

pub fn removed_command_hint(name: &str) -> Option<&'static str> {
    match name {
        "serve" => Some("`vibe serve` was removed: use `vibe run`"),
        "compile" => Some("`vibe compile` was removed: use `vibe build` (it validates automatically; add `--release` for release packaging)"),
        "validate" => Some("`vibe validate` was removed: use `vibe build` (it validates automatically; add `--release` for release packaging)"),
        "manifest" => Some("`vibe manifest` was removed: use `vibe build` (it validates automatically; add `--release` for release packaging)"),
        "new" => Some("`vibe new` was removed: run `vibe` with no arguments in a terminal"),
        "migrate" => Some("`vibe migrate` was removed: migrations run automatically in `vibe run --db postgres`"),
        "keygen" => Some("`vibe keygen` was removed: `vibe build` creates the development signing key (.vibe/signing.key) automatically; release builds need no key (the release workflow signs with Sigstore keyless)"),
        "slug" => Some("`vibe slug` was removed: add public slugs to .vibe/slugs.json (`vibe build` checks the file and lists the slugs)"),
        "secret-scan" => Some("`vibe secret-scan` was removed: `vibe build` runs the secret scan and fails on findings"),
        "endpoint" => Some("`vibe endpoint` was removed: list disabled features in .vibe/disabled.txt (one `<feature-id><TAB><reason>` per line; `vibe build` checks it)"),
        "approve" => Some("`vibe approve` was removed: use `vibe build`; review happens on the source pull request"),
        _ => None,
    }
}

fn cmd_validate(project: &Path, deploy: bool, dev: bool) -> Result<(), String> {
    let report = validate_project(&ValidateOptions {
        dev,
        root: project,
        deploy,
    })
    .map_err(|err| err.to_string())?;
    // #118: `vibe build` (and any validate with a production
    // VIBE_ENV) refuses `challenge_when: always` until a verifier is configured.
    let run_env =
        vibe::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), !deploy);
    let always = vibe::runtime::challenge::always_challenge_locks(project);
    let configured = vibe::runtime::challenge::configured_in_env()?;
    vibe::runtime::challenge::production_gate(run_env, configured, &always)?;
    // #199: challenges that are declared but never verified at runtime.
    let unverified = vibe::runtime::challenge::unverified_challenge_locks(project);
    vibe::runtime::challenge::unverified_gate(run_env, &unverified)?;
    // #157: the replay store choice (the table itself is checked by `vibe run`).
    let replay_kind =
        vibe::runtime::replay::replay_kind_from_vars(|name| std::env::var(name).ok())?;
    vibe::runtime::replay::resolve_replay_store(run_env, configured, replay_kind, true)?;
    print_report(&report);
    println!("validation ok");
    Ok(())
}

async fn cmd_manifest(
    project: &Path,
    output: &Path,
    release: bool,
    release_version: Option<String>,
    commit: Option<String>,
) -> Result<(), String> {
    if release && std::env::var("LLM_PROVIDER").as_deref() == Ok("fake") {
        return Err("the fake LLM provider (LLM_PROVIDER=fake) is for tests and cannot be used for a release build".into());
    }
    // Release manifests are signed by the release workflow with Sigstore
    // keyless (decisions 92, 119); no key is ever loaded for them.
    let key = if release {
        None
    } else {
        Some(sign::load_or_create(project)?)
    };
    let version = if release {
        release_version
            .or_else(|| {
                std::env::var("GITHUB_REF_NAME")
                    .ok()
                    .and_then(|name| name.strip_prefix('v').map(str::to_string))
            })
            .ok_or("release builds need --release-version or a `v*` tag in GITHUB_REF_NAME")?
    } else {
        String::new()
    };
    let commit = commit
        .or_else(|| std::env::var("GITHUB_SHA").ok())
        .or_else(|| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(project)
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        })
        .unwrap_or_default();
    // A release build compiles from source in memory and never reads lock
    // files from disk; a dev manifest uses the locks `vibe build` wrote
    // (checked against the build record).
    let loaded = if release {
        // A release build never reads the project .env: every input comes
        // from the release environment.
        let mut env = llm::env_map_from_process();
        if let Some(key) = env.get("VIBE_LLM_API_KEY").cloned() {
            env.entry("LLM_API_KEY".into()).or_insert(key);
        }
        llm::resolve_api_key_file(&mut env)?;
        // No LLM key (VIBE_LLM_API_KEY or LLM_API_KEY) is ever sent to a
        // host outside the release allowlist.
        if let Ok(config) = llm::load_llm_config(&env, true) {
            llm::check_release_endpoint(&config)?;
        }
        let config = llm::load_llm_config(&env, true).ok();
        let adapter = match &config {
            Some(config) => Some(llm::adapter_for(config).map_err(|err| err.to_string())?),
            None => None,
        };
        let (loaded, report) = vibe::compile::compile_in_memory(
            project,
            adapter.as_ref().map(|a| a.as_ref() as &dyn LlmAdapter),
            config.as_ref(),
        )
        .await
        .map_err(|err| err.to_string())?;
        print_report(&report);
        loaded
    } else {
        vibe::validate::load_project(project).map_err(|err| err.to_string())?
    };
    let info = manifest::BuildInfo {
        release,
        version,
        commit,
    };
    let manifest = manifest::build_signed(project, &loaded, key.as_ref(), &info)
        .map_err(|err| err.to_string())?;
    print_high_risk(&manifest);
    // Relative to the current directory, like `vibe run --artifact` (#31).
    let path = output.to_path_buf();
    manifest::write_manifest(&path, &manifest).map_err(|err| err.to_string())?;
    println!("wrote {}", path.display());
    Ok(())
}

fn print_high_risk(manifest: &manifest::Manifest) {
    let high = manifest.provenance.high_risk();
    if high.is_empty() {
        return;
    }
    eprintln!(
        "high-risk items ({}, listed for the deploy report):",
        high.len()
    );
    for item in high {
        eprintln!("  {} {}", item.lock, item.item);
    }
}

#[allow(dead_code)]
async fn cmd_migrate(manifest_path: &Path, project: &Path, tenant: &str) -> Result<(), String> {
    if !tenant
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        || !(8..=32).contains(&tenant.len())
    {
        return Err("tenant id must match ^[a-z0-9]{8,32}$".into());
    }
    let run_env = vibe::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), false);
    let manifest = load_manifest(manifest_path, project, run_env)?;
    check_deployable(&manifest, run_env)?;
    // Migrations need the table-owner role; serving must not use it.
    let url = std::env::var("VIBE_MIGRATE_DATABASE_URL").map_err(|_| {
        "VIBE_MIGRATE_DATABASE_URL is required (the owner role used for DDL; `vibe run` uses a separate non-owner VIBE_DATABASE_URL)".to_string()
    })?;
    let runtime_role = std::env::var("VIBE_RUNTIME_DB_ROLE").map_err(|_| {
        "VIBE_RUNTIME_DB_ROLE is required (the non-owner role `vibe run` connects as; it is granted SELECT, INSERT, UPDATE)".to_string()
    })?;
    let exec = PgExec::connect(&url).await?;
    for line in vibe::runtime::migrate_tenant(&exec, &manifest, tenant, &runtime_role).await? {
        println!("{line}");
    }
    // Last: the revoke is table-level (on `_platform_refresh_tokens` only).
    // `GRANT ... ON ALL TABLES IN SCHEMA` in a migration expands to per-table
    // grants when it runs, so revoking afterwards removes those as well.
    exec.check_role_separation_for(Some(&runtime_role)).await?;
    if manifest.routes.iter().any(|r| r.refresh.is_some()) {
        let schema = format!("t_{tenant}");
        exec.batch(&vibe::runtime::refresh::table_statements(&schema)?)
            .await?;
        exec.batch(&vibe::runtime::refresh::runtime_grant_statements(
            &schema,
            &runtime_role,
        )?)
        .await?;
        exec.batch(&vibe::runtime::refresh::cleanup_statements(&schema)?)
            .await?;
        println!("bootstrapped {schema}._platform_refresh_tokens (finished families cleaned up)");
    }
    Ok(())
}

fn ci_or_prod() -> bool {
    std::env::var("CI").ok().as_deref() == Some("true")
        || std::env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
        || !vibe::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), true)
            .is_dev()
}

fn print_report(report: &vibe::Report) {
    for diag in &report.diags {
        eprintln!("{diag}");
    }
}

struct TerminalIo;
impl vibe::interactive::Io for TerminalIo {
    fn read_line(&mut self, prompt: &str) -> Option<String> {
        use std::io::Write;
        print!("{prompt}");
        std::io::stdout().flush().ok()?;
        let mut line = String::new();
        (std::io::stdin().read_line(&mut line).ok()? > 0).then(|| line.trim_end().to_string())
    }
    fn read_secret(&mut self, prompt: &str) -> Option<String> {
        read_password(prompt)
    }
    fn write(&mut self, text: &str) {
        println!("{text}");
    }
}

fn read_password(prompt: &str) -> Option<String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().ok()?;
    let _guard = EchoGuard::disable()?;
    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line).ok()?;
    println!();
    (n > 0).then(|| line.trim_end_matches(['\r', '\n']).to_string())
}
#[cfg(unix)]
struct EchoGuard(libc::termios);
#[cfg(unix)]
impl EchoGuard {
    fn disable() -> Option<Self> {
        unsafe {
            let mut old = std::mem::zeroed();
            if libc::tcgetattr(0, &mut old) != 0 {
                return None;
            }
            let mut new = old;
            new.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(0, libc::TCSANOW, &new) != 0 {
                return None;
            }
            Some(Self(old))
        }
    }
}
#[cfg(unix)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
    fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
}
#[cfg(windows)]
struct EchoGuard(*mut std::ffi::c_void, u32);
#[cfg(windows)]
impl EchoGuard {
    fn disable() -> Option<Self> {
        unsafe {
            let handle = GetStdHandle(-10i32 as u32);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0 || SetConsoleMode(handle, mode & !4) == 0 {
                return None;
            }
            Some(Self(handle, mode))
        }
    }
}
#[cfg(windows)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        unsafe {
            SetConsoleMode(self.0, self.1);
        }
    }
}

fn check_build_inputs(project: &Path) -> Result<(), String> {
    let report = secret_scan(project);
    print_report(&report);
    if report.has_errors() {
        return Err("secret scan found a problem".into());
    }
    let runtime = vibe::runtime::load_project_slugs(project, false)?;
    let example_path = project.join("slugs.example.json");
    let example = if example_path.exists() {
        Some(vibe::runtime::load_slug_file(&example_path)?)
    } else {
        None
    };
    let selected = if project.join(".vibe/slugs.json").exists() {
        &runtime
    } else if let Some(example) = &example {
        println!("slugs from slugs.example.json (dev/memory only)");
        example
    } else {
        &runtime
    };
    let mut slugs: Vec<_> = selected.slugs.iter().collect();
    slugs.sort_by_key(|(slug, _)| *slug);
    for (slug, entry) in slugs {
        println!(
            "slug {slug} -> tenant {} (public write: {})",
            entry.tenant_id,
            if entry.public_write { "yes" } else { "no" }
        );
    }
    let sources = vibe::discover::discover(project).map_err(|e| e.to_string())?;
    if runtime.slugs.is_empty()
        && sources.iter().any(|source| {
            source.text.contains("{slug}")
                || fs::read_to_string(project.join(source.lock_rel()))
                    .is_ok_and(|text| text.contains("{slug}"))
        })
    {
        eprintln!("warning: public routes use {{slug}} but .vibe/slugs.json has no slugs; add one, e.g. {{\"<slug>\": {{\"tenant_id\": \"<8-32 lowercase letters or digits>\", \"public_write\": false}}}}; slugs.example.json is used only by `vibe run --db memory`");
    }
    let path = project.join(".vibe/disabled.txt");
    if path.exists() {
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let n = index + 1;
            let (id, reason) = line
                .split_once('\t')
                .filter(|(id, reason)| !id.trim().is_empty() && !reason.trim().is_empty())
                .ok_or_else(|| {
                    format!(
                        ".vibe/disabled.txt line {n}: expected <feature-id><TAB><non-empty reason>"
                    )
                })?;
            if !sources
                .iter()
                .any(|source| source.scope == Some(vibe::lock::Scope::Feature) && source.id == id)
            {
                eprintln!("warning: .vibe/disabled.txt line {n}: unknown feature {id}");
            }
            println!("disabled feature {id}: {reason}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn removed_commands_have_migration_hints() {
        for command in ["compile", "validate", "manifest"] {
            let hint = super::removed_command_hint(command).unwrap();
            assert!(hint.contains("vibe build") && hint.contains("--release"));
        }
        assert!(super::removed_command_hint("new")
            .unwrap()
            .contains("no arguments in a terminal"));
        for command in ["build", "run"] {
            assert!(super::removed_command_hint(command).is_none());
        }
    }
}

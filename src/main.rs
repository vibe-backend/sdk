use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use vibe::compile::{compile_project, CompileOptions};
use vibe::discover;
use vibe::llm::{self, HttpAdapter, LlmAdapter};
use vibe::lock::is_structured_source;
use vibe::manifest::{self, read_manifest};
use vibe::runtime::{router, AppState, AuthConfig, PgExec, QueryExec};
use vibe::secret::secret_scan;
use vibe::sign;
use vibe::validate::{validate_project, ValidateOptions};

#[derive(Parser)]
#[command(name = "vibe", about = "Compile and serve a markdown-driven backend")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile source markdown into lock files.
    Compile {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Build in memory and run the deploy policy. Ignores and does not write lock files.
        #[arg(long)]
        check: bool,
    },
    /// Run the deterministic validator.
    Validate {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Also apply deploy-only checks (e.g. no locks compiled against a private LLM endpoint).
        #[arg(long)]
        deploy: bool,
    },
    /// Build a signed manifest from validated locks.
    Manifest {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Output path, relative to the current directory (like `serve --manifest`).
        #[arg(long, short, default_value = "manifest.json")]
        output: PathBuf,
        /// Release build: signs with VIBE_RELEASE_SIGNING_KEY only and fails on
        /// high-risk items without --release-approval.
        #[arg(long)]
        release: bool,
        /// Reference to the approval of this release's high-risk items.
        #[arg(long)]
        release_approval: Option<String>,
        /// Source commit to record (default: GITHUB_SHA or `git rev-parse HEAD`).
        #[arg(long)]
        commit: Option<String>,
    },
    /// Build a manifest: compile in memory, validate, then sign from the
    /// validated content. Writes nothing unless every step succeeds.
    /// Default is a release build (release key only); `--dev` is a local
    /// development build.
    Build {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Development build: signs with the local dev key (.vibe/signing.key)
        /// and marks the manifest `build-mode: dev`. Production refuses it.
        #[arg(long)]
        dev: bool,
        /// Output path, relative to the current directory.
        #[arg(long, short, default_value = "manifest.json")]
        output: PathBuf,
        /// Reference to the approval of this release's high-risk items.
        #[arg(long)]
        release_approval: Option<String>,
        /// Source commit to record (default: GITHUB_SHA or `git rev-parse HEAD`).
        #[arg(long)]
        commit: Option<String>,
    },
    /// Create a development manifest signing key (.vibe/signing.key, mode 0600).
    Keygen {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Removed in v1.8.
    #[command(hide = true)]
    Approve {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    /// Serve HTTP from a signed manifest.
    Serve {
        #[arg(long, default_value = "manifest.json")]
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: String,
        /// Dev-only executor that does not use PostgreSQL.
        #[arg(long)]
        memory: bool,
    },
    /// Create tenant tables from model locks. Development bootstrap, not a production migration.
    Migrate {
        #[arg(long, default_value = "manifest.json")]
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        tenant: String,
    },
    /// Register a public slug for a tenant.
    Slug {
        #[command(subcommand)]
        action: SlugAction,
    },
    /// Scan the tree for credential material.
    SecretScan {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        fail_on_find: bool,
    },
    /// Disable an endpoint without redeploying.
    Endpoint {
        #[command(subcommand)]
        action: EndpointAction,
    },
}

#[derive(Subcommand)]
enum SlugAction {
    /// Write `.vibe/slugs.json`. High-risk public routes resolve `{slug}` from this file.
    Add {
        slug: String,
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        public_write: bool,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Subcommand)]
enum EndpointAction {
    Disable {
        feature: String,
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    Enable {
        feature: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
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
    match cli.command {
        Command::Compile { project, check } => cmd_compile(&project, check).await,
        Command::Validate { project, deploy } => cmd_validate(&project, deploy),
        Command::Manifest {
            project,
            output,
            release,
            release_approval,
            commit,
        } => cmd_manifest(&project, &output, release, release_approval, commit).await,
        Command::Build {
            project,
            dev,
            output,
            release_approval,
            commit,
        } => cmd_build(&project, dev, &output, release_approval, commit).await,
        Command::Keygen { project } => {
            sign::create_dev_key(&project)?;
            println!("wrote {}", project.join(".vibe/signing.key").display());
            Ok(())
        }
        Command::Approve { .. } => Err("`vibe approve` was removed in v1.8: locks carry no approval. Review the source in a pull request; release builds use `vibe build`, which requires --release-approval for high-risk items.".into()),
        Command::Serve {
            manifest,
            project,
            addr,
            memory,
        } => cmd_serve(&manifest, &project, &addr, memory).await,
        Command::Migrate {
            manifest,
            project,
            tenant,
        } => cmd_migrate(&manifest, &project, &tenant).await,
        Command::Slug { action } => match action {
            SlugAction::Add {
                slug,
                tenant,
                public_write,
                project,
            } => {
                vibe::runtime::add_slug(&project, &slug, &tenant, public_write)?;
                println!("added slug {slug} for tenant {tenant}");
                Ok(())
            }
        },
        Command::SecretScan {
            project,
            fail_on_find,
        } => {
            let report = secret_scan(&project);
            print_report(&report);
            if fail_on_find && report.has_errors() {
                Err("secret scan found a problem".into())
            } else {
                Ok(())
            }
        }
        Command::Endpoint { action } => match action {
            EndpointAction::Disable {
                feature,
                reason,
                project,
            } => {
                if reason.trim().is_empty() {
                    return Err("a reason is required".into());
                }
                let path = project.join(".vibe").join("disabled.txt");
                fs::create_dir_all(project.join(".vibe")).map_err(|err| err.to_string())?;
                let mut current = fs::read_to_string(&path).unwrap_or_default();
                if !current
                    .lines()
                    .any(|line| line.split('\t').next() == Some(feature.as_str()))
                {
                    current.push_str(&feature);
                    current.push('\t');
                    current.push_str(&reason.replace(['\n', '\t'], " "));
                    current.push('\n');
                    fs::write(&path, current).map_err(|err| err.to_string())?;
                }
                println!("disabled {feature}");
                Ok(())
            }
            EndpointAction::Enable { feature, project } => {
                let path = project.join(".vibe").join("disabled.txt");
                let current = fs::read_to_string(&path).unwrap_or_default();
                let kept: Vec<_> = current
                    .lines()
                    .filter(|line| line.split('\t').next() != Some(feature.as_str()))
                    .collect();
                fs::write(&path, kept.join("\n") + "\n").map_err(|err| err.to_string())?;
                println!("enabled {feature}");
                Ok(())
            }
        },
    }
}

async fn cmd_compile(project: &Path, check: bool) -> Result<(), String> {
    load_dotenv(project);
    let sources = discover::discover(project).map_err(|err| err.to_string())?;
    let natural = sources
        .iter()
        .any(|source| !is_structured_source(&source.combined_text()));
    let config = match llm::load_llm_config(&llm::env_map_from_process(), ci_or_prod()) {
        Ok(config) => Some(config),
        Err(report) if natural => return Err(report.to_string()),
        Err(_) => None,
    };
    let adapter;
    let adapter_ref: Option<&dyn LlmAdapter> = if natural {
        let config = config.as_ref().ok_or("LLM config is required")?;
        adapter = HttpAdapter::new(config.clone()).map_err(|err| err.to_string())?;
        Some(&adapter)
    } else {
        None
    };
    let output = compile_project(&CompileOptions {
        root: project,
        llm: adapter_ref,
        llm_config: config.as_ref(),
        check,
    })
    .await
    .map_err(|err| err.to_string())?;
    print_report(&output.report);
    println!("compiled {} locks", output.files.len());
    Ok(())
}

fn cmd_validate(project: &Path, deploy: bool) -> Result<(), String> {
    let report = validate_project(&ValidateOptions {
        root: project,
        deploy,
    })
    .map_err(|err| err.to_string())?;
    print_report(&report);
    println!("validation ok");
    Ok(())
}

async fn cmd_manifest(
    project: &Path,
    output: &Path,
    release: bool,
    release_approval: Option<String>,
    commit: Option<String>,
) -> Result<(), String> {
    let key = if release {
        sign::load_release_key()?
    } else {
        sign::load_dev_key(project)?
    };
    let commit = resolve_commit(project, commit);
    let loaded = if release {
        compile_release(project).await?
    } else {
        vibe::validate::load_project(project).map_err(|err| err.to_string())?
    };
    let info = manifest::BuildInfo {
        release,
        release_approval,
        commit,
    };
    let manifest =
        manifest::build_signed(project, &loaded, &key, &info).map_err(|err| err.to_string())?;
    print_high_risk(&manifest);
    // Relative to the current directory, like `vibe serve --manifest` (#31).
    let path = output.to_path_buf();
    manifest::write_manifest(&path, &manifest).map_err(|err| err.to_string())?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Reads the release inputs once (sources compiled in memory, never lock
/// files from disk).
async fn compile_release(project: &Path) -> Result<vibe::validate::LoadedProject, String> {
    compile_sources(project, true).await
}

async fn compile_sources(
    project: &Path,
    strict_llm: bool,
) -> Result<vibe::validate::LoadedProject, String> {
    load_dotenv(project);
    // The release environment provides the compiler key as VIBE_LLM_API_KEY.
    let mut env = llm::env_map_from_process();
    if let Some(key) = env.get("VIBE_LLM_API_KEY").cloned() {
        env.entry("LLM_API_KEY".into()).or_insert(key);
    }
    let config = llm::load_llm_config(&env, strict_llm).ok();
    let adapter = match &config {
        Some(config) => Some(HttpAdapter::new(config.clone()).map_err(|err| err.to_string())?),
        None => None,
    };
    let (loaded, report) = vibe::compile::compile_in_memory(
        project,
        adapter.as_ref().map(|a| a as &dyn LlmAdapter),
        config.as_ref(),
    )
    .await
    .map_err(|err| err.to_string())?;
    print_report(&report);
    Ok(loaded)
}

fn resolve_commit(project: &Path, commit: Option<String>) -> String {
    commit
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
        .unwrap_or_default()
}

/// `vibe build` (spec 2C.3, issue #34): the sources are read and compiled
/// once into memory; the validator runs on that content and the manifest is
/// built and signed from the same content. The output file is written only
/// after every step has passed, atomically, so a failure leaves no output.
async fn cmd_build(
    project: &Path,
    dev: bool,
    output: &Path,
    release_approval: Option<String>,
    commit: Option<String>,
) -> Result<(), String> {
    if dev && release_approval.is_some() {
        return Err("--release-approval applies only to release builds; drop --dev".into());
    }
    let commit = resolve_commit(project, commit);
    let loaded = compile_sources(project, !dev || ci_or_prod()).await?;
    // Validate first: errors stop the build before any key is used.
    let (result, _) = vibe::validate::validate_loaded(
        &ValidateOptions {
            root: project,
            deploy: true,
        },
        &loaded,
    );
    match result {
        Ok(report) => print_report(&report),
        Err(report) => {
            print_report(&report);
            return Err("validation failed; nothing was written".into());
        }
    }
    // A release build never falls back to the dev key (decision 92).
    let key = if dev {
        sign::load_dev_key(project)?
    } else {
        sign::load_release_key()?
    };
    let info = manifest::BuildInfo {
        release: !dev,
        release_approval,
        commit,
    };
    let manifest = manifest::build_signed(project, &loaded, &key, &info)
        .map_err(|err| format!("{err}\nnothing was written"))?;
    print_high_risk(&manifest);
    manifest::write_manifest(output, &manifest).map_err(|err| err.to_string())?;
    println!("wrote {}", output.display());
    Ok(())
}

fn print_high_risk(manifest: &manifest::Manifest) {
    let high = manifest.provenance.high_risk();
    if high.is_empty() {
        return;
    }
    eprintln!("high-risk items ({}):", high.len());
    for item in high {
        eprintln!("  {} {}", item.lock, item.item);
    }
    if let Some(reference) = &manifest.provenance.release_approval {
        eprintln!("release approval: {reference}");
    }
}

/// Manifest verifying key. Production trusts only the pinned release key
/// (VIBE_MANIFEST_PUBKEY or one compiled in); the project's
/// `.vibe/signing.pub` is accepted in development only.
fn manifest_key(
    project: &Path,
    run_env: vibe::runtime::RunEnv,
) -> Result<ed25519_dalek::VerifyingKey, String> {
    if let Some(key) = sign::pinned_public_key()? {
        return Ok(key);
    }
    if !run_env.is_dev() {
        return Err("no pinned manifest key: set VIBE_MANIFEST_PUBKEY to the release public key (the project's .vibe/signing.pub is not trusted outside development)".into());
    }
    eprintln!("warning: verifying the manifest with the development key in .vibe/signing.pub");
    sign::load_dev_public(project)
}

fn check_deployable(
    manifest: &manifest::Manifest,
    run_env: vibe::runtime::RunEnv,
) -> Result<(), String> {
    let provenance = &manifest.provenance;
    tracing::info!(
        build_mode = %provenance.build_mode,
        commit = %provenance.commit,
        locks = provenance.locks.len(),
        high_risk = provenance.high_risk().len(),
        "manifest provenance"
    );
    manifest::check_deployable(manifest, run_env.is_dev())
}

async fn cmd_serve(
    manifest_path: &Path,
    project: &Path,
    addr: &str,
    memory: bool,
) -> Result<(), String> {
    if let Some(name) = llm::runtime_llm_env_present() {
        return Err(format!("runtime environment must not contain {name}"));
    }
    let run_env =
        vibe::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), memory);
    if memory && !run_env.is_dev() {
        return Err(format!(
            "--memory is a development executor and is refused when VIBE_ENV is {:?}; use PostgreSQL",
            std::env::var("VIBE_ENV").unwrap_or_default()
        ));
    }
    let auth = auth_from_env(run_env)?;
    let key = manifest_key(project, run_env)?;
    let manifest = read_manifest(manifest_path, &key).map_err(|err| err.to_string())?;
    check_deployable(&manifest, run_env)?;
    // Decision 91: a manifest that is not `build-mode: dev` is production;
    // the in-memory executor is refused, with no override.
    if memory && manifest.provenance.build_mode != "dev" {
        return Err(format!(
            "--memory is a development executor and is refused for a `build-mode: {}` manifest; use PostgreSQL",
            manifest.provenance.build_mode
        ));
    }
    let exec: Arc<dyn QueryExec> = if memory {
        Arc::new(vibe::runtime::MemoryExec::new())
    } else {
        let url = std::env::var("VIBE_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .map_err(|_| {
                "set VIBE_DATABASE_URL or pass --memory for the dev executor".to_string()
            })?;
        let pg = PgExec::connect(&url).await?;
        pg.check_runtime_role().await?;
        Arc::new(pg)
    };
    let loaded = vibe::runtime::load_project_slugs(project, memory)?;
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    if loaded.slugs.is_empty()
        && manifest
            .routes
            .iter()
            .any(|route| route.public && route.path.contains("{slug}"))
    {
        eprintln!(
            "warning: public routes use {{slug}} but no tenants are configured; copy slugs.example.json to .vibe/slugs.json or run `vibe slug add <slug> --tenant <id>`"
        );
    }
    let addr: SocketAddr = addr.parse().map_err(|err| format!("bad address: {err}"))?;
    let state = Arc::new(AppState {
        manifest,
        exec,
        auth,
        slugs: loaded.slugs,
        rates: std::sync::Mutex::new(Default::default()),
        quotas: std::sync::Mutex::new(Default::default()),
        disabled: load_disabled(project),
        min_override_ms: None,
        honor_test_ip: false,
        trusted_proxies: trusted_proxies_from_env()?,
        dev_hints: memory,
    });
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| err.to_string())?;
    println!("listening on {addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|err| err.to_string())
}

async fn cmd_migrate(manifest_path: &Path, project: &Path, tenant: &str) -> Result<(), String> {
    if !tenant
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        || !(8..=32).contains(&tenant.len())
    {
        return Err("tenant id must match ^[a-z0-9]{8,32}$".into());
    }
    let run_env = vibe::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), false);
    let key = manifest_key(project, run_env)?;
    let manifest = read_manifest(manifest_path, &key).map_err(|err| err.to_string())?;
    check_deployable(&manifest, run_env)?;
    // Migrations need the table-owner role; serving must not use it.
    let url = std::env::var("VIBE_MIGRATE_DATABASE_URL").map_err(|_| {
        "VIBE_MIGRATE_DATABASE_URL is required (the owner role used for DDL; `vibe serve` uses a separate non-owner VIBE_DATABASE_URL)".to_string()
    })?;
    let runtime_role = std::env::var("VIBE_RUNTIME_DB_ROLE").map_err(|_| {
        "VIBE_RUNTIME_DB_ROLE is required (the non-owner role `vibe serve` connects as; it is granted SELECT, INSERT, UPDATE)".to_string()
    })?;
    let exec = PgExec::connect(&url).await?;
    for line in vibe::runtime::migrate_tenant(&exec, &manifest, tenant, &runtime_role).await? {
        println!("{line}");
    }
    Ok(())
}

fn auth_from_env(run_env: vibe::runtime::RunEnv) -> Result<AuthConfig, String> {
    let mut auth = AuthConfig::anonymous();
    if let Ok(pem) = std::env::var("VIBE_JWT_PUBLIC_KEY_PEM") {
        auth.public_pem = pem.replace("\\n", "\n");
        auth.alg = vibe::runtime::detect_jwt_alg(&auth.public_pem)?;
    }
    if let Ok(alg) = std::env::var("VIBE_JWT_ALG") {
        auth.alg = match alg.trim().to_ascii_uppercase().as_str() {
            "ES256" => jsonwebtoken::Algorithm::ES256,
            "RS256" => jsonwebtoken::Algorithm::RS256,
            other => {
                return Err(format!(
                    "VIBE_JWT_ALG `{other}`: only ES256 and RS256 are supported"
                ));
            }
        };
    }
    if let Ok(iss) = std::env::var("VIBE_JWT_ISS") {
        auth.iss = iss;
    }
    if let Ok(aud) = std::env::var("VIBE_JWT_AUD") {
        auth.aud = aud;
    }
    if let Ok(kid) = std::env::var("VIBE_JWT_KID") {
        auth.kid = kid;
    }
    let pepper = std::env::var("VIBE_API_KEY_PEPPER").ok();
    let (pepper, warning) = vibe::runtime::resolve_pepper(run_env, pepper.as_deref())?;
    if let Some(warning) = warning {
        eprintln!("warning: {warning}");
    }
    auth.pepper = pepper;
    // API keys are `mbk_live_…` in production and `mbk_test_…` elsewhere.
    auth.env_name = if run_env == vibe::runtime::RunEnv::Production {
        "live".into()
    } else {
        "test".into()
    };
    if let Ok(claim) = std::env::var("VIBE_JWT_ROLES_CLAIM") {
        if !claim.trim().is_empty() {
            if let Some(problem) = vibe::manifest::roles_claim_problem(claim.trim()) {
                return Err(format!("VIBE_JWT_ROLES_CLAIM: {problem}"));
            }
            auth.roles_claim = claim.trim().to_string();
        }
    }
    let jwks_uri = std::env::var("VIBE_JWT_JWKS_URI")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            // Spec §4.1: default `{iss}/.well-known/jwks.json` when no PEM is pinned.
            (auth.public_pem.is_empty() && std::env::var("VIBE_JWT_ISS").is_ok())
                .then(|| format!("{}/.well-known/jwks.json", auth.iss.trim_end_matches('/')))
        });
    if jwks_uri.is_some() && !auth.public_pem.is_empty() {
        return Err("set either VIBE_JWT_PUBLIC_KEY_PEM or VIBE_JWT_JWKS_URI, not both".into());
    }
    if let Some(uri) = jwks_uri {
        let source = vibe::runtime::jwks::HttpsJwks::new(&uri)?;
        auth.jwks = Some(std::sync::Arc::new(vibe::runtime::jwks::JwksCache::new(
            std::sync::Arc::new(source),
        )));
    }
    if let Ok(path) = std::env::var("VIBE_API_KEYS_FILE") {
        auth.api_keys_file = Some(std::sync::Arc::new(vibe::runtime::ApiKeyFile::open(
            Path::new(&path).to_path_buf(),
        )?));
    }
    Ok(auth)
}

/// `VIBE_TRUSTED_PROXIES`: comma-separated proxy IPs or CIDR blocks whose
/// `X-Forwarded-For` is honoured. Unset = trust no proxy (use the peer).
fn trusted_proxies_from_env() -> Result<Vec<vibe::runtime::ProxyNet>, String> {
    let Ok(raw) = std::env::var("VIBE_TRUSTED_PROXIES") else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            vibe::runtime::ProxyNet::parse(s).map_err(|err| format!("VIBE_TRUSTED_PROXIES: {err}"))
        })
        .collect()
}

fn load_disabled(project: &Path) -> std::collections::HashSet<String> {
    let path = project.join(".vibe").join("disabled.txt");
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split('\t').next())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn load_dotenv(project: &Path) {
    let path = project.join(".env");
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if std::env::var(key).is_err() {
            std::env::set_var(key, value);
        }
    }
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

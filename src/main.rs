use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use vibe::approve::{approve_project, ApproveOptions};
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
        /// Recompile and fail if the locks differ. Does not write.
        #[arg(long)]
        check: bool,
    },
    /// Run the deterministic validator.
    Validate {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Also require approval and a valid signature.
        #[arg(long)]
        deploy: bool,
    },
    /// Record local approval and sign locks.
    ///
    /// Production approval is a platform review. This command is the local stand-in.
    Approve {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Repeatable. High-risk locks (public endpoints, providers, and migrations) need two reviewers: `--reviewer a --reviewer b`.
        #[arg(long = "reviewer")]
        reviewers: Vec<String>,
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = "local")]
        approval_ref: String,
    },
    /// Build a signed manifest from approved locks.
    Manifest {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Output path, relative to the current directory (like `serve --manifest`).
        #[arg(long, short, default_value = "manifest.json")]
        output: PathBuf,
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
        Command::Approve {
            project,
            reviewers,
            reason,
            approval_ref,
        } => cmd_approve(&project, &reviewers, &reason, &approval_ref),
        Command::Manifest { project, output } => cmd_manifest(&project, &output),
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
    // A missing key is one more finding, not a reason to stop before
    // reporting everything else.
    let mut key_problem = None;
    let key = if deploy {
        match sign::load_public(project) {
            Ok(key) => Some(key),
            Err(err) => {
                key_problem = Some(vibe::Diag::error(
                    ".vibe/signing.pub",
                    format!("cannot load the signing key: {err}"),
                ));
                None
            }
        }
    } else {
        None
    };
    let result = validate_project(&ValidateOptions {
        root: project,
        deploy,
        verify_key: key.as_ref(),
    });
    let report = match (result, key_problem) {
        (Ok(report), None) => report,
        (Ok(mut report) | Err(mut report), Some(problem)) => {
            report.diags.insert(0, problem);
            return Err(report.to_string());
        }
        (Err(report), None) => return Err(report.to_string()),
    };
    // #118: `vibe validate --deploy` (and any validate with a production
    // VIBE_ENV) refuses `challenge_when: always` until a verifier is configured.
    let run_env =
        vibe::runtime::RunEnv::from_var(std::env::var("VIBE_ENV").ok().as_deref(), !deploy);
    let always = vibe::runtime::challenge::always_challenge_locks(project);
    let configured = vibe::runtime::challenge::configured_in_env()?;
    vibe::runtime::challenge::production_gate(run_env, configured, &always)?;
    // #199: challenges that are declared but never verified at runtime.
    let unverified = vibe::runtime::challenge::unverified_challenge_locks(project);
    vibe::runtime::challenge::unverified_gate(run_env, &unverified)?;
    // #157: the replay store choice (the table itself is checked by `vibe serve`).
    let replay_kind =
        vibe::runtime::replay::replay_kind_from_vars(|name| std::env::var(name).ok())?;
    vibe::runtime::replay::resolve_replay_store(run_env, configured, replay_kind, true)?;
    print_report(&report);
    if deploy {
        println!("validation ok");
    } else {
        println!("validation ok; approval and signatures are checked only with --deploy");
    }
    Ok(())
}

fn cmd_approve(
    project: &Path,
    reviewers: &[String],
    reason: &str,
    approval_ref: &str,
) -> Result<(), String> {
    let key = sign::load_or_create(project).map_err(|err| err.to_string())?;
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".into());
    let now = chrono::Utc::now().to_rfc3339();
    let report = approve_project(&ApproveOptions {
        root: project,
        reviewers,
        reason,
        approval_ref,
        commit: &commit,
        now: &now,
        signing_key: &key,
    })
    .map_err(|err| err.to_string())?;
    print_report(&report);
    println!(
        "approved locks with {}",
        hex::encode(key.verifying_key().to_bytes())
    );
    Ok(())
}

fn cmd_manifest(project: &Path, output: &Path) -> Result<(), String> {
    let key = sign::load_or_create(project).map_err(|err| err.to_string())?;
    let manifest = manifest::manifest_from_project(project, &key).map_err(|err| err.to_string())?;
    // Relative to the current directory, like `vibe serve --manifest` (#31).
    let path = output.to_path_buf();
    manifest::write_manifest(&path, &manifest).map_err(|err| err.to_string())?;
    println!("wrote {}", path.display());
    Ok(())
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
    // #199: refuse locks whose challenge is not verified at runtime, before
    // loading the database or the manifest (locks in the project tree).
    let unverified_locks = vibe::runtime::challenge::unverified_challenge_locks(project);
    vibe::runtime::challenge::unverified_gate(run_env, &unverified_locks)?;
    // #157: production always uses the PostgreSQL replay store and
    // refuses the memory switch; dev/CI must choose explicitly. Decided
    // before loading anything.
    let replay_choice = vibe::runtime::replay::resolve_replay_store(
        run_env,
        vibe::runtime::challenge::configured_in_env()?,
        vibe::runtime::replay::replay_kind_from_vars(|name| std::env::var(name).ok())?,
        !memory,
    )?;
    let mut auth = auth_from_env(run_env)?;
    let key = sign::load_public(project).map_err(|err| err.to_string())?;
    let manifest = read_manifest(manifest_path, &key).map_err(|err| err.to_string())?;
    // `sign-jwt` key-refs name environment variables holding private PEMs.
    auth.signing_keys =
        vibe::runtime::load_signing_keys(&manifest, &|name| std::env::var(name).ok(), &auth.kid)?;
    if memory {
        let with_query: Vec<&str> = manifest
            .methods
            .iter()
            .filter(|m| m.steps.iter().any(|s| s.op == "query"))
            .map(|m| m.target.as_str())
            .collect();
        if !with_query.is_empty() {
            eprintln!(
                "warning: --memory does not run `query` steps; these methods will fail until you use PostgreSQL: {}",
                with_query.join(", ")
            );
        }
    }
    // Per-route iss/jwks-uri (spec §4.1): one JWKS cache per URI, https only.
    auth.add_route_jwks(&manifest.routes)?;
    let mut pg_pool = None;
    let exec: Arc<dyn QueryExec> = if memory {
        Arc::new(vibe::runtime::MemoryExec::new())
    } else {
        let url = std::env::var("VIBE_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .map_err(|_| {
                "set VIBE_DATABASE_URL or pass --memory for the dev executor".to_string()
            })?;
        let pg = PgExec::connect_with(&url, vibe::runtime::PgOptions::from_env()?).await?;
        pg.check_runtime_role().await?;
        pg_pool = Some(pg.pool());
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
    // #118: refuse `challenge_when: always` in production until a verifier
    // is configured.
    // #157: build the per-tenant store chosen at startup; a missing or unusable
    // table refuses to start.
    let replay_store: Option<Arc<dyn vibe::runtime::replay::ReplayStore>> =
        match (replay_choice, pg_pool) {
            (Some(vibe::runtime::replay::StoreKind::Postgres), Some(pool)) => {
                // Per-tenant tables; the IP digest is keyed per tenant from
                // the API-key pepper.
                let store = vibe::runtime::replay::PgReplayStore::new(pool, &auth.pepper);
                let mut tenants: Vec<String> =
                    loaded.slugs.values().map(|s| s.tenant_id.clone()).collect();
                tenants.sort();
                tenants.dedup();
                store.check(&tenants).await?;
                Some(Arc::new(store))
            }
            (Some(vibe::runtime::replay::StoreKind::Postgres), None) => {
                return Err("the challenge replay store needs PostgreSQL".into())
            }
            _ => None,
        };
    let challenge = vibe::runtime::challenge::verifier_from_env_with_store(replay_store)?;
    let always: Vec<String> = manifest
        .routes
        .iter()
        .filter(|r| r.public && r.challenge_type != "none" && r.challenge_when == "always")
        .map(|r| r.feature_id.clone())
        .collect();
    vibe::runtime::challenge::production_gate(run_env, challenge.is_some(), &always)?;
    // #199: refuse challenges that are declared but never verified.
    let unverified: Vec<String> = manifest
        .routes
        .iter()
        .filter(|r| {
            vibe::runtime::challenge::challenge_unverified(
                r.public,
                &r.challenge_type,
                &r.challenge_when,
            )
        })
        .map(|r| r.feature_id.clone())
        .collect();
    vibe::runtime::challenge::unverified_gate(run_env, &unverified)?;
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
        challenge,
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
    let key = sign::load_public(project).map_err(|err| err.to_string())?;
    let manifest = read_manifest(manifest_path, &key).map_err(|err| err.to_string())?;
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
    let breached = vibe::runtime::breached::BreachedConfig::from_env(
        &|name| std::env::var(name).ok(),
        run_env == vibe::runtime::RunEnv::Production,
    )?;
    eprintln!("breached-password check: {}", breached.source.describe());
    auth.breached = std::sync::Arc::new(breached);
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
        let allow_private = std::env::var("VIBE_JWT_ALLOW_PRIVATE_HOST").as_deref() == Ok("true");
        if allow_private && !run_env.is_dev() {
            return Err("VIBE_JWT_ALLOW_PRIVATE_HOST is only allowed in development".into());
        }
        let source = vibe::runtime::jwks::HttpsJwks::with_policy(&uri, allow_private)?;
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

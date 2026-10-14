use std::collections::HashMap;
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
use vibe::runtime::{bootstrap_statements, router, AppState, AuthConfig, Col, PgExec, QueryExec};
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
    let key = if deploy {
        Some(sign::load_public(project).map_err(|err| err.to_string())?)
    } else {
        None
    };
    let report = validate_project(&ValidateOptions {
        root: project,
        deploy,
        verify_key: key.as_ref(),
    })
    .map_err(|err| err.to_string())?;
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
    let path = if output.is_absolute() {
        output.to_path_buf()
    } else {
        project.join(output)
    };
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
    let key = sign::load_public(project).map_err(|err| err.to_string())?;
    let manifest = read_manifest(manifest_path, &key).map_err(|err| err.to_string())?;
    let exec: Arc<dyn QueryExec> = if memory {
        Arc::new(vibe::runtime::MemoryExec::new())
    } else {
        let url = std::env::var("VIBE_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .map_err(|_| {
                "set VIBE_DATABASE_URL or pass --memory for the dev executor".to_string()
            })?;
        Arc::new(PgExec::connect(&url).await?)
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
        auth: auth_from_env()?,
        slugs: loaded.slugs,
        rates: std::sync::Mutex::new(HashMap::new()),
        quotas: std::sync::Mutex::new(HashMap::new()),
        disabled: load_disabled(project),
        min_override_ms: None,
        honor_test_ip: false,
        dev_hints: memory,
    });
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| err.to_string())?;
    println!("listening on {addr}");
    axum::serve(listener, app)
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
    let url = std::env::var("VIBE_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .map_err(|_| "VIBE_DATABASE_URL is required".to_string())?;
    let exec = PgExec::connect(&url).await?;
    let schema = format!("t_{tenant}");
    for model in &manifest.models {
        let fields: Vec<Col> = model
            .fields
            .iter()
            .map(|field| Col {
                name: field.name.clone(),
                ty: field.ty.clone(),
            })
            .collect();
        let uniques: Vec<String> = model
            .fields
            .iter()
            .filter(|field| field.unique)
            .map(|field| field.name.clone())
            .collect();
        let statements = bootstrap_statements(&schema, &model.table, &fields, &uniques)?;
        exec.batch(&statements).await?;
        println!("bootstrapped {}.{}", schema, model.table);
    }
    for migration in &manifest.migrations {
        exec.batch(&migration.statements).await?;
        println!("applied {}", migration.id);
    }
    Ok(())
}

fn auth_from_env() -> Result<AuthConfig, String> {
    let mut auth = AuthConfig::anonymous();
    if let Ok(pem) = std::env::var("VIBE_JWT_PUBLIC_KEY_PEM") {
        auth.public_pem = pem.replace("\\n", "\n");
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
    let vibe_env = std::env::var("VIBE_ENV").ok();
    let pepper = std::env::var("VIBE_API_KEY_PEPPER").ok();
    let (pepper, warning) = vibe::runtime::resolve_pepper(vibe_env.as_deref(), pepper.as_deref())?;
    if let Some(warning) = warning {
        eprintln!("warning: {warning}");
    }
    auth.pepper = pepper;
    if let Some(env_name) = vibe_env {
        auth.env_name = env_name;
    }
    Ok(auth)
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
        || matches!(
            std::env::var("VIBE_ENV").as_deref(),
            Ok("production") | Ok("ci")
        )
}

fn print_report(report: &vibe::Report) {
    for diag in &report.diags {
        eprintln!("{diag}");
    }
}

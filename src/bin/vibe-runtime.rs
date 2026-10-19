use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use vibe::run::{DbChoice, RunOptions, RunSource};

#[derive(Parser)]
#[command(
    version,
    about = "Run a signed vibe build artifact",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Command {
    Run(RunArgs),
    Verify(VerifyArgs),
}

#[derive(Args)]
struct RunArgs {
    #[arg(long, default_value = ".")]
    artifact: PathBuf,
    #[arg(long)]
    project: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    #[arg(long, value_parser = ["memory", "postgres"])]
    db: Option<String>,
    #[command(flatten)]
    pins: PinFlags,
    #[arg(long)]
    allow_test_build: bool,
    #[arg(long)]
    allow_destructive_migrations: bool,
}

#[derive(Args)]
struct PinFlags {
    /// Exact project tag workflow identity (VIBE_EXPECT_IDENTITY).
    #[arg(long)]
    expect_identity: Option<String>,
    /// GitHub Actions OIDC issuer (VIBE_EXPECT_ISSUER).
    #[arg(long)]
    expect_issuer: Option<String>,
    /// Numeric GitHub repository id (VIBE_EXPECT_REPOSITORY_ID).
    #[arg(long)]
    expect_repository_id: Option<String>,
}
impl From<PinFlags> for vibe::run::PinArgs {
    fn from(flags: PinFlags) -> Self {
        Self {
            identity: flags.expect_identity,
            issuer: flags.expect_issuer,
            repository_id: flags.expect_repository_id,
        }
    }
}
#[derive(Args)]
struct VerifyArgs {
    #[arg(long)]
    artifact: PathBuf,
    #[arg(long)]
    project: Option<PathBuf>,
    #[command(flatten)]
    pins: PinFlags,
    #[arg(long)]
    allow_test_build: bool,
}
fn verify(args: VerifyArgs) -> Result<(), String> {
    let result = vibe::run::verify_artifact(&vibe::run::VerifyOptions {
        project: args
            .project
            .unwrap_or_else(|| artifact_project(&args.artifact)),
        artifact: args.artifact,
        pins: args.pins.into(),
        allow_test_build: args.allow_test_build,
    })?;
    if !result.manifest.route_prefix.is_empty() || result.manifest.default_slug.is_some() {
        println!(
            "routes: prefix {}, default-slug {}",
            result.manifest.route_prefix,
            result.manifest.default_slug.as_deref().unwrap_or("(none)")
        );
    }
    if let Some(identity) = result.identity {
        println!(
            "verified: {} (release {}, commit {}, identity {})",
            result.path.display(),
            result.manifest.provenance.version,
            result.manifest.provenance.commit,
            identity
        );
    } else {
        println!("verified: {} (development manifest)", result.path.display());
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Verify(args)) => verify(args),
        Some(Command::Run(args)) => run(args).await,
        None => run(cli.run).await,
    };
    if let Err(message) = result {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn artifact_project(artifact: &std::path::Path) -> PathBuf {
    if artifact.is_dir() {
        artifact.into()
    } else {
        artifact
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."))
            .into()
    }
}

async fn run(args: RunArgs) -> Result<(), String> {
    let project = args
        .project
        .unwrap_or_else(|| artifact_project(&args.artifact));
    vibe::run::run(RunOptions {
        pins: args.pins.into(),
        project,
        source: RunSource::Artifact(args.artifact),
        addr: args.addr,
        db: DbChoice::from_arg_or_env(args.db.as_deref())?,
        allow_test_build: args.allow_test_build,
        allow_destructive_migrations: args.allow_destructive_migrations,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_manifest_uses_current_directory() {
        assert_eq!(
            artifact_project(std::path::Path::new("manifest.json")),
            PathBuf::from(".")
        );
        assert_eq!(
            artifact_project(std::path::Path::new("/app/manifest.json")),
            PathBuf::from("/app")
        );
    }

    #[test]
    fn bare_and_explicit_run_accept_the_same_flags() {
        for prefix in [vec!["vibe-runtime"], vec!["vibe-runtime", "run"]] {
            let cli = Cli::try_parse_from(prefix.into_iter().chain([
                "--artifact",
                "manifest.json",
                "--addr",
                "0.0.0.0:8080",
                "--db",
                "memory",
                "--allow-test-build",
                "--allow-destructive-migrations",
            ]))
            .unwrap();
            let args = match cli.command {
                Some(Command::Run(args)) => args,
                None => cli.run,
                Some(Command::Verify(_)) => panic!("expected run"),
            };
            assert_eq!(args.artifact, PathBuf::from("manifest.json"));
            assert_eq!(args.addr, "0.0.0.0:8080");
            assert!(args.allow_test_build);
            assert!(args.allow_destructive_migrations);
        }
        assert!(Cli::try_parse_from(["vibe-runtime", "--artifact", ".", "run"]).is_err());
    }
}

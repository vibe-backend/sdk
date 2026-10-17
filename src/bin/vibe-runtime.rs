use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use vibe::run::{DbChoice, RunOptions, RunSource};

#[derive(Parser)]
#[command(version, about = "Run a signed vibe build artifact")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Run(RunArgs),
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
    #[arg(long)]
    allow_test_build: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = match Cli::parse().command {
        Some(Command::Run(args)) => args,
        None => RunArgs {
            artifact: ".".into(),
            project: None,
            addr: "127.0.0.1:8080".into(),
            db: None,
            allow_test_build: false,
        },
    };
    if let Err(message) = run(args).await {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

async fn run(args: RunArgs) -> Result<(), String> {
    let project = args.project.unwrap_or_else(|| {
        if args.artifact.is_dir() {
            args.artifact.clone()
        } else {
            args.artifact
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .into()
        }
    });
    vibe::run::run(RunOptions {
        project,
        source: RunSource::Artifact(args.artifact),
        addr: args.addr,
        db: DbChoice::from_arg_or_env(args.db.as_deref())?,
        allow_test_build: args.allow_test_build,
    })
    .await
}

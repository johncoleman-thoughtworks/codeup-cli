//! Codeup CLI entry point.
//!
//! Subcommands (incremental):
//!   codeup scan [path]              — full or scoped scan
//!   codeup intent suggest [path]    — draft .codeup/intent.yaml
//!   codeup --version / --help
//!
//! Each subcommand will eventually delegate to codeup-core for analysis and
//! to reporter modules (sarif/markdown/json/text) for output. Today this is
//! a bones entry that compiles and parses --version / --help correctly.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "codeup",
    version,
    about = "Architectural anti-pattern scanner",
    long_about = "Codeup scans a codebase for architectural anti-patterns. The analyzer is shared with the VS Code extension; findings persist as YAML in .codeup/ so they travel with the repo and accumulate the team's decisions."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase log verbosity. Pass once for info, twice for debug, thrice for trace.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress all output except findings + errors.
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Scan a workspace for anti-patterns.
    Scan(ScanArgs),
    /// Draft a starter .codeup/intent.yaml from the workspace structure.
    Intent {
        #[command(subcommand)]
        action: IntentAction,
    },
}

#[derive(clap::Args, Debug)]
struct ScanArgs {
    /// Workspace root to scan. Defaults to the current directory.
    #[arg(default_value = ".")]
    path: std::path::PathBuf,

    /// Provider: anthropic | github-models. Defaults to auto-detect from
    /// available credentials.
    #[arg(long, env = "CODEUP_PROVIDER")]
    provider: Option<String>,

    /// API key for the active provider. For Anthropic, also picked up
    /// from ANTHROPIC_API_KEY env var. For GitHub Models, from GITHUB_TOKEN.
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    api_key: Option<String>,

    /// Output format: sarif | markdown | json | text.
    #[arg(long, default_value = "text")]
    out: String,

    /// Write the report to this file instead of stdout.
    #[arg(long)]
    output: Option<std::path::PathBuf>,

    /// Skip the LLM pass entirely. Only deterministic checks run.
    #[arg(long)]
    deterministic_only: bool,

    /// Maximum estimated USD cost before the scan aborts.
    #[arg(long, default_value_t = 5.0)]
    max_cost: f64,

    /// Exit code 1 if any finding has at least this severity. Use "none" to disable.
    #[arg(long, default_value = "high")]
    fail_on: String,
}

#[derive(Subcommand, Debug)]
enum IntentAction {
    /// Generate a starter intent.yaml from the workspace structure.
    Suggest {
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet);

    match cli.command {
        Command::Scan(args) => scan(args),
        Command::Intent { action } => match action {
            IntentAction::Suggest { path } => intent_suggest(&path),
        },
    }
}

fn scan(args: ScanArgs) -> anyhow::Result<()> {
    tracing::info!("codeup scan {:?} (provider {:?}, out {})", args.path, args.provider, args.out);
    anyhow::bail!("scan: not implemented yet — bones-only release")
}

fn intent_suggest(path: &std::path::Path) -> anyhow::Result<()> {
    tracing::info!("codeup intent suggest {:?}", path);
    anyhow::bail!("intent suggest: not implemented yet — bones-only release")
}

fn init_tracing(verbose: u8, quiet: bool) {
    use tracing_subscriber::{fmt, EnvFilter};
    if quiet {
        return;
    }
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_env("CODEUP_LOG").unwrap_or_else(|_| EnvFilter::new(level));
    fmt().with_env_filter(filter).with_target(false).init();
}

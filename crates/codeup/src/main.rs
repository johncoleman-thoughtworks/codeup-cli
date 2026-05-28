//! Codeup CLI entry point.

mod analyzer;
mod cache;
mod llm;
mod runner;
mod sarif;
mod store;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use llm::provider::{resolve, ProviderSetting};

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

    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

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
    #[arg(default_value = ".")]
    path: std::path::PathBuf,

    #[arg(long, env = "CODEUP_PROVIDER")]
    provider: Option<String>,

    /// Anthropic API key. Used only when the active provider is
    /// `anthropic` (or `auto` and a key is present). Never substituted
    /// into another provider's credential slot — see also
    /// `--github-token`.
    #[arg(long, env = "ANTHROPIC_API_KEY", hide_env_values = true)]
    anthropic_api_key: Option<String>,

    /// GitHub token for the GitHub Models endpoint. Used only when the
    /// active provider is `github-models` (or `auto` and no Anthropic
    /// key is present). Never substituted into another provider's
    /// credential slot — see also `--anthropic-api-key`.
    #[arg(long, env = "GITHUB_TOKEN", hide_env_values = true)]
    github_token: Option<String>,

    #[arg(long, env = "CODEUP_MODEL")]
    model: Option<String>,

    /// Report format: `text` (default, human summary), `sarif` (SARIF
    /// 2.1.0 JSON for GitHub Code Scanning), or `json` (raw findings).
    #[arg(long, default_value = "text")]
    out: String,

    #[arg(long)]
    output: Option<std::path::PathBuf>,

    #[arg(long)]
    deterministic_only: bool,

    #[arg(long, default_value_t = 5.0)]
    max_cost: f64,

    #[arg(long, default_value = "high")]
    fail_on: String,
}

#[derive(Subcommand, Debug)]
enum IntentAction {
    Suggest {
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    rt.block_on(async {
        match cli.command {
            Command::Scan(args) => scan(args).await,
            Command::Intent { action } => match action {
                IntentAction::Suggest { path: _ } => {
                    anyhow::bail!("intent suggest: not implemented yet — Phase 2.x")
                }
            },
        }
    })
}

async fn scan(args: ScanArgs) -> Result<()> {
    let now = chrono_now_iso();
    let setting = ProviderSetting::parse(args.provider.as_deref())?;

    let client = if args.deterministic_only {
        None
    } else {
        let resolved = match resolve(
            setting,
            args.anthropic_api_key.as_deref(),
            args.github_token.as_deref(),
            args.model.as_deref(),
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("LLM provider unavailable: {e}. Falling back to --deterministic-only.");
                None.map(|c: llm::provider::ResolvedProvider| c);
                anyhow::bail!("{e}\n\nHint: use --deterministic-only to skip the LLM pass and still get cycles + layer-violations + oversized-file findings.");
            }
        };
        tracing::info!(
            "provider: {} ({}) — model: {}",
            resolved.client.provider().as_str(),
            resolved.reason,
            resolved.client.model()
        );
        Some(resolved.client)
    };

    let summary = runner::run(runner::RunOptions {
        root: &args.path,
        now: &now,
        deterministic_only: args.deterministic_only,
        client: client.as_ref(),
        persist: true,
    })
    .await?;

    let report = match args.out.to_lowercase().as_str() {
        "text" => render_text(&summary),
        "sarif" => sarif::render(&summary.findings),
        "json" => serde_json::to_string_pretty(&summary.findings)
            .context("serializing findings to JSON")?,
        other => anyhow::bail!(
            "unknown --out format {other:?}: expected one of text | sarif | json"
        ),
    };

    if let Some(path) = &args.output {
        std::fs::write(path, &report).with_context(|| format!("writing {path:?}"))?;
    } else {
        println!("{report}");
    }

    // --fail-on threshold (default "high"): exit 1 if any open finding ≥ threshold
    let threshold = args.fail_on.to_lowercase();
    if threshold != "none" {
        let trip = summary.findings.iter().any(|f| {
            let sev = match f.severity {
                codeup_core::schema::Severity::Low => 1,
                codeup_core::schema::Severity::Medium => 2,
                codeup_core::schema::Severity::High => 3,
            };
            let bar = match threshold.as_str() {
                "low" => 1,
                "medium" => 2,
                _ => 3,
            };
            sev >= bar
                && !matches!(
                    f.status,
                    codeup_core::schema::Status::Dismissed
                        | codeup_core::schema::Status::Fixed
                )
        });
        if trip {
            std::process::exit(1);
        }
    }

    Ok(())
}

fn render_text(summary: &runner::RunSummary) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "# Codeup scan summary");
    let _ = writeln!(out);
    let _ = writeln!(out, "Root           : {}", summary.root.display());
    let _ = writeln!(out, "Files indexed  : {}", summary.index.files.len());
    let _ = writeln!(out, "Graph edges    : {}", summary.graph.edges.values().map(|s| s.len()).sum::<usize>());
    let _ = writeln!(out, "Cycles         : {}", summary.cycle_count);
    let _ = writeln!(out, "Layer violations: {}", summary.layer_violation_count);
    let _ = writeln!(out, "Oversized files: {}", summary.oversized_count);
    let _ = writeln!(out, "LLM scanned    : {}", summary.llm_files_scanned);
    let _ = writeln!(out, "LLM cached     : {}", summary.llm_files_cached);
    let _ = writeln!(out, "LLM skipped    : {}", summary.llm_files_skipped);
    let _ = writeln!(out, "Total findings : {}", summary.findings.len());
    let _ = writeln!(out);

    let mut by_sev: std::collections::BTreeMap<&str, Vec<&codeup_core::schema::Finding>> = std::collections::BTreeMap::new();
    for f in &summary.findings {
        if matches!(
            f.status,
            codeup_core::schema::Status::Dismissed | codeup_core::schema::Status::Fixed
        ) {
            continue;
        }
        let sev = match f.severity {
            codeup_core::schema::Severity::High => "high",
            codeup_core::schema::Severity::Medium => "medium",
            codeup_core::schema::Severity::Low => "low",
        };
        by_sev.entry(sev).or_default().push(f);
    }

    for sev in &["high", "medium", "low"] {
        let Some(items) = by_sev.get(*sev) else { continue };
        let _ = writeln!(out, "## {} ({})", sev, items.len());
        for f in items {
            let line = f.location.line.map(|n| format!(":{n}")).unwrap_or_default();
            let _ = writeln!(out, "  - {}  {}{}", f.category, f.location.file, line);
        }
        let _ = writeln!(out);
    }

    out
}

fn chrono_now_iso() -> String {
    // Plain ISO-8601-Z without pulling chrono. Millisecond precision is
    // required by the shared SCHEMA.md so the TS extension's js-yaml
    // round-trips the value cleanly (and ms is enough to keep timestamps
    // monotonic across a fast scan).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    unix_to_iso(secs, millis)
}

fn unix_to_iso(secs: i64, millis: u32) -> String {
    // Cheap ISO formatter. Good enough for timestamps in YAML. Emits
    // `YYYY-MM-DDTHH:MM:SS.mmmZ` per .codeup SCHEMA.md.
    let days = secs.div_euclid(86_400);
    let mut rem = secs.rem_euclid(86_400);
    let hour = rem / 3600;
    rem %= 3600;
    let minute = rem / 60;
    let second = rem % 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Civil-date algorithm by Howard Hinnant (public domain).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
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

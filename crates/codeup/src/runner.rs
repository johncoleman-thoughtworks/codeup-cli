//! Scan orchestrator. Mirrors TS `scan/runner.ts` but stripped of
//! VS Code progress UI — output channel is structured log lines.

use crate::analyzer::ToolUseReviewer;
use crate::cache::AnalysisCache;
use crate::llm::LLMClient;
use crate::review::review_workspace;
use crate::store::{load_intent, load_knowledge, FindingsStore};
use anyhow::Result;
use codeup_core::catalogue::load_catalogue;
use codeup_core::intent::{cycle_findings, layer_violations};
use codeup_core::quality::{oversized_files, SizeCheckOptions};
use codeup_core::scanner::graph::{build_graph, find_cycles, DependencyGraph};
use codeup_core::scanner::{scan_workspace, ProjectIndex};
use codeup_core::schema::Finding;
use std::path::{Path, PathBuf};

pub struct RunOptions<'a> {
    pub root: &'a Path,
    pub now: &'a str,
    pub deterministic_only: bool,
    pub client: Option<&'a LLMClient>,
    pub persist: bool,
}

pub struct RunSummary {
    pub root: PathBuf,
    pub index: ProjectIndex,
    pub graph: DependencyGraph,
    pub findings: Vec<Finding>,
    pub cycle_count: usize,
    pub oversized_count: usize,
    pub layer_violation_count: usize,
    pub llm_files_scanned: usize,
    pub llm_files_cached: usize,
    pub llm_files_skipped: usize,
}

pub async fn run(opts: RunOptions<'_>) -> Result<RunSummary> {
    let (knowledge, custom_patterns) = load_knowledge(opts.root)?;
    let catalogue = load_catalogue(&custom_patterns)?;

    tracing::info!("scanning workspace: {:?}", opts.root);
    let index = scan_workspace(opts.root, opts.now.to_string())?;
    tracing::info!("indexed {} files", index.files.len());
    let graph = build_graph(&index);

    let mut store = FindingsStore::load(opts.root)?;
    let mut all_new: Vec<Finding> = Vec::new();

    // Deterministic checks first — no API cost.
    let cycles = find_cycles(&graph);
    let cycle_count = cycles.len();
    for f in cycle_findings(&cycles, opts.now) {
        let stored = store.upsert_from_analysis(f)?;
        all_new.push(stored.clone());
    }

    let intent = load_intent(opts.root)?;
    let mut layer_violation_count = 0;
    if let Some(intent) = &intent {
        let edges: Vec<(&str, &str)> = graph
            .edges
            .iter()
            .flat_map(|(from, tos)| tos.iter().map(move |to| (from.as_str(), to.as_str())))
            .collect();
        let lvs = layer_violations(edges, intent, opts.now);
        layer_violation_count = lvs.len();
        for f in lvs {
            let stored = store.upsert_from_analysis(f)?;
            all_new.push(stored.clone());
        }
    }

    let oversized = oversized_files(&index, SizeCheckOptions::default(), opts.now);
    let oversized_count = oversized.len();
    for f in oversized {
        let stored = store.upsert_from_analysis(f)?;
        all_new.push(stored.clone());
    }

    let mut llm_files_scanned = 0;
    let mut llm_files_cached = 0;
    let mut llm_files_skipped = 0;

    // LLM pass — skipped when deterministic-only or no client. The
    // file-iteration / neighbour / persistence orchestration lives in
    // `review::review_workspace`; here we just supply the tool-use adapter.
    // The MCP server supplies a sampling adapter to the same orchestrator.
    if !opts.deterministic_only {
        if let Some(client) = opts.client {
            let cache = AnalysisCache::new(opts.root);
            tracing::info!(
                "LLM pass: provider={}, model={}",
                client.provider().as_str(),
                client.model()
            );
            let reviewer = ToolUseReviewer {
                catalogue: &catalogue,
                knowledge: &knowledge,
                custom_patterns: &custom_patterns,
                cache: &cache,
                client,
            };
            let llm = review_workspace(
                opts.root,
                None,
                &catalogue,
                &index,
                &graph,
                &knowledge,
                &reviewer,
                &mut store,
                opts.now,
            )
            .await?;
            llm_files_scanned = llm.files_scanned;
            llm_files_cached = llm.files_cached;
            llm_files_skipped = llm.files_skipped;
        } else {
            tracing::info!("no LLM client; deterministic-only run");
        }
    }

    if !opts.persist {
        // Findings are still in memory; the store wrote each one to disk as
        // it went. `--no-persist` would mean we should clean them up — for
        // v0.1 the default is to persist (matching the TS extension's
        // behaviour). A future --no-persist flag would skip the writes
        // upstream rather than delete here.
    }

    Ok(RunSummary {
        root: opts.root.to_path_buf(),
        index,
        graph,
        // Security: only emit findings the current run actually re-detected.
        // Persisted-only YAML files (potentially planted by a malicious PR)
        // are state, not authoritative output — they don't reach SARIF or
        // the --fail-on gate.
        findings: store.produced_by_this_run().cloned().collect(),
        cycle_count,
        oversized_count,
        layer_violation_count,
        llm_files_scanned,
        llm_files_cached,
        llm_files_skipped,
    })
}

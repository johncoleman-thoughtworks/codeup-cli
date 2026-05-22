//! Scan orchestrator. Mirrors TS `scan/runner.ts` but stripped of
//! VS Code progress UI — output channel is structured log lines.

use crate::analyzer::{analyze_file, AnalyzeContext, AnalyzeResult, NeighborFile, NeighborRelation, MAX_NEIGHBORS};
use crate::cache::AnalysisCache;
use crate::llm::LLMClient;
use crate::store::{load_intent, load_knowledge, FindingsStore};
use anyhow::Result;
use codeup_core::catalogue::{load_catalogue, patterns_for_language};
use codeup_core::intent::{cycle_findings, layer_violations};
use codeup_core::quality::{oversized_files, SizeCheckOptions};
use codeup_core::scanner::graph::{build_graph, find_cycles, neighbors_of, DependencyGraph};
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

    // LLM pass — skipped when deterministic-only or no client.
    if !opts.deterministic_only {
        if let Some(client) = opts.client {
            let cache = AnalysisCache::new(opts.root);
            let supported: Vec<&codeup_core::scanner::FileEntry> = index
                .files
                .iter()
                .filter(|f| !patterns_for_language(&catalogue, &f.language).is_empty())
                .collect();
            tracing::info!(
                "LLM pass: {} candidate files, provider={}, model={}",
                supported.len(),
                client.provider().as_str(),
                client.model()
            );

            for entry in &supported {
                let bytes = match std::fs::read(opts.root.join(&entry.path)) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("{}: read failed: {e}", entry.path);
                        continue;
                    }
                };
                let text = match std::str::from_utf8(&bytes) {
                    Ok(t) => t.to_string(),
                    Err(_) => {
                        llm_files_skipped += 1;
                        continue;
                    }
                };
                let neighbors = gather_neighbors(opts.root, entry, &graph, &index);
                let ctx = AnalyzeContext {
                    catalogue: &catalogue,
                    knowledge: &knowledge,
                    custom_patterns: &custom_patterns,
                    cache: &cache,
                    client,
                };
                match analyze_file(entry, &text, ctx, &neighbors, opts.now).await {
                    Ok(AnalyzeResult { findings, skipped, from_cache, .. }) => {
                        if let Some(reason) = skipped {
                            llm_files_skipped += 1;
                            tracing::debug!("{}: skipped ({})", entry.path, reason);
                            continue;
                        }
                        if from_cache {
                            llm_files_cached += 1;
                        } else {
                            llm_files_scanned += 1;
                        }
                        for f in findings {
                            let stored = store.upsert_from_analysis(f)?;
                            all_new.push(stored.clone());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("{}: analyze failed: {e}", entry.path);
                    }
                }
            }
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
        findings: store.all().cloned().collect(),
        cycle_count,
        oversized_count,
        layer_violation_count,
        llm_files_scanned,
        llm_files_cached,
        llm_files_skipped,
    })
}

fn gather_neighbors(
    root: &Path,
    entry: &codeup_core::scanner::FileEntry,
    graph: &DependencyGraph,
    index: &ProjectIndex,
) -> Vec<NeighborFile> {
    let (imports, imported_by) = neighbors_of(graph, &entry.path);
    let mut picks: Vec<(String, NeighborRelation)> = Vec::new();
    let ia: Vec<&str> = imports.into_iter().take(MAX_NEIGHBORS).collect();
    let ib: Vec<&str> = imported_by.into_iter().take(MAX_NEIGHBORS).collect();
    for i in 0..MAX_NEIGHBORS {
        if picks.len() >= MAX_NEIGHBORS { break; }
        if let Some(p) = ia.get(i) {
            picks.push((p.to_string(), NeighborRelation::Imports));
        }
        if picks.len() >= MAX_NEIGHBORS { break; }
        if let Some(p) = ib.get(i) {
            picks.push((p.to_string(), NeighborRelation::ImportedBy));
        }
    }
    // Same-package fallback (JVM/.NET case).
    if picks.len() < MAX_NEIGHBORS {
        let taken: std::collections::HashSet<&str> = picks.iter().map(|(p, _)| p.as_str()).chain(std::iter::once(entry.path.as_str())).collect();
        let dir = entry.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let mut siblings: Vec<&codeup_core::scanner::FileEntry> = index
            .files
            .iter()
            .filter(|f| !taken.contains(f.path.as_str()))
            .filter(|f| f.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("") == dir)
            .filter(|f| f.language == entry.language)
            .collect();
        siblings.sort_by(|a, b| a.path.cmp(&b.path));
        for sib in siblings {
            if picks.len() >= MAX_NEIGHBORS { break; }
            picks.push((sib.path.clone(), NeighborRelation::SamePackage));
        }
    }

    let by_path: std::collections::HashMap<&str, &codeup_core::scanner::FileEntry> =
        index.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut out = Vec::new();
    for (path, relation) in picks {
        let Some(e) = by_path.get(path.as_str()) else { continue };
        let bytes = match std::fs::read(root.join(&path)) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        out.push(NeighborFile { path, language: e.language.clone(), text, relation });
    }
    out
}

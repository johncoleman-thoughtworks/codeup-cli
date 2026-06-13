//! Shared review building blocks used by both the CLI analyzer and the
//! MCP server (`codeup mcp`).
//!
//! The CLI's LLM pass (`analyzer.rs`) gets structured `report_finding`
//! tool calls straight from the provider. MCP **sampling**, by contrast,
//! returns a free-text completion — the host runs the model and there is
//! no guaranteed tool-use channel (see PLAN-MCP.md, "Open questions").
//! So the sampling path asks the model for a fenced JSON array and parses
//! it here, then runs every element through the *same* catalogue
//! validation (`analyzer::validate_reported`) the tool-use path uses.
//! Single source of truth: the prompt builders, the validation, and the
//! `stable_id`/save contract all live in `analyzer`/`store` and are reused
//! verbatim, so MCP findings are byte-identical to CLI findings.

use crate::analyzer::{self, NeighborFile, NeighborRelation, MAX_FILE_CHARS, MAX_NEIGHBORS};
use crate::cache::ReportedFinding;
use crate::store::FindingsStore;
use anyhow::Result;
use codeup_core::catalogue::{patterns_for_language, Catalogue, CataloguePattern};
use codeup_core::knowledge::{relevant_for, KnowledgeSnapshot, RelevantKnowledge};
use codeup_core::scanner::graph::{neighbors_of, DependencyGraph};
use codeup_core::scanner::{FileEntry, ProjectIndex};
use codeup_core::schema::Finding;
use futures::future::BoxFuture;
use std::path::Path;

/// Output-token budget for a sampling review request. Matches the CLI
/// analyzer's `MAX_OUTPUT_TOKENS` so a file's findings aren't truncated
/// differently across the two paths.
pub const SAMPLING_MAX_TOKENS: u32 = 2048;

/// The instruction appended to the user prompt for the sampling path.
/// MCP sampling has no tool-use channel, so we ask for a fenced JSON
/// array explicitly. The shape mirrors the `report_finding` tool schema
/// field-for-field so `validate_reported` accepts it unchanged.
pub const SAMPLING_OUTPUT_INSTRUCTION: &str = "\n\n\
--- OUTPUT FORMAT ---\n\
Respond with ONLY a JSON array of findings — no prose, no markdown outside the array. \
Each element is an object with these fields:\n\
  - \"category\": string — a pattern id from the catalogue above (REQUIRED, must match exactly)\n\
  - \"severity\": \"low\" | \"medium\" | \"high\" (REQUIRED)\n\
  - \"line\": integer — 1-based starting line in the PRIMARY file (REQUIRED, >= 1)\n\
  - \"endLine\": integer — optional 1-based inclusive ending line\n\
  - \"explanation\": string — 2-5 sentences grounded in this code (REQUIRED)\n\
  - \"suggestedRemediation\": string — optional concrete next step\n\
  - \"confidence\": number 0..1 (REQUIRED; not a gate, always emit the finding)\n\
If there are no findings, respond with exactly []. \
Do not wrap the array in a code fence; emit the raw JSON array.";

/// Parse the findings JSON array out of a sampling completion and validate
/// each element against the catalogue allowlist. Tolerant of the common
/// ways a model wraps the array (a ```json fence, leading prose, trailing
/// commentary) — we locate the outermost balanced `[...]` and parse that.
/// Anything that doesn't validate (unknown category, bad severity, line 0,
/// empty explanation) is dropped, exactly as on the tool-use path.
pub fn parse_reported_findings(text: &str, patterns: &[&CataloguePattern]) -> Vec<ReportedFinding> {
    let Some(array) = extract_json_array(text) else {
        return Vec::new();
    };
    let value: serde_json::Value = match serde_json::from_str(&array) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| analyzer::validate_reported(item, patterns))
        .collect()
}

/// Locate the outermost balanced JSON array in `text`, ignoring brackets
/// that appear inside string literals. Returns the slice from the first
/// top-level `[` to its matching `]`. This survives a leading ```json
/// fence, surrounding prose, and brackets embedded in explanation strings.
fn extract_json_array(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| b == b'[')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

// ---- the review use-case (domain orchestration) -------------------------
//
// `review_workspace` is the single orchestrator for the LLM catalogue pass.
// It owns the *domain* concerns — which files are reviewable, neighbour
// context, persistence, telemetry — and delegates the one thing that
// differs between hosts (how a file's findings are produced) to a
// `FileReviewer` port. The CLI supplies a tool-use adapter; the MCP server
// supplies a host-sampling adapter. Neither orchestration loop is
// duplicated, and the use-case is unit-testable with a fake reviewer that
// never touches an LLM (see tests).

/// What the orchestrator hands a reviewer for one primary file. Everything
/// here is derived domain context; the reviewer only decides how to turn it
/// into findings.
pub struct FileReviewRequest<'a> {
    pub entry: &'a FileEntry,
    pub text: &'a str,
    pub neighbors: &'a [NeighborFile],
    pub patterns: &'a [&'a CataloguePattern],
    pub relevant: &'a RelevantKnowledge,
}

/// What a reviewer returns for one file.
pub struct FileReviewOutcome {
    pub findings: Vec<Finding>,
    /// Whether these findings came from a cache hit rather than a fresh
    /// model call — purely for the run summary.
    pub from_cache: bool,
}

/// The port. Implementations encapsulate the LLM mechanism (Anthropic /
/// GitHub Models tool-use, or host sampling) and any caching. A manual
/// `BoxFuture` keeps this dyn-compatible without an async-trait dependency,
/// matching the crate's enum-dispatch house style elsewhere.
///
/// `Sync` is required so a `&dyn FileReviewer` can be held across an await
/// inside the MCP server's per-request task (which tokio spawns on a
/// multi-thread runtime, demanding `Send`). Both adapters hold only `Sync`
/// data, so this costs nothing.
pub trait FileReviewer: Sync {
    fn review_file<'a>(
        &'a self,
        request: FileReviewRequest<'a>,
        now: &'a str,
    ) -> BoxFuture<'a, Result<FileReviewOutcome>>;
}

/// Telemetry from a workspace review pass.
#[derive(Debug, Default)]
pub struct WorkspaceReviewSummary {
    pub files_scanned: usize,
    pub files_cached: usize,
    pub files_skipped: usize,
    pub findings_persisted: usize,
    /// (file, message) for files whose review errored — surfaced by the
    /// MCP server, logged by the CLI.
    pub errors: Vec<(String, String)>,
}

/// Run the LLM catalogue pass over a workspace, persisting findings through
/// `store`. Deterministic findings are expected to have been produced
/// separately (they need no model); this is only the per-file judgement
/// pass. `scope`, when present, restricts the review to those exact
/// workspace-relative paths.
#[allow(clippy::too_many_arguments)]
pub async fn review_workspace(
    root: &Path,
    scope: Option<&[String]>,
    catalogue: &Catalogue,
    index: &ProjectIndex,
    graph: &DependencyGraph,
    knowledge: &KnowledgeSnapshot,
    reviewer: &dyn FileReviewer,
    store: &mut FindingsStore,
    now: &str,
) -> Result<WorkspaceReviewSummary> {
    let mut summary = WorkspaceReviewSummary::default();

    for entry in &index.files {
        if let Some(scope) = scope {
            if !scope.iter().any(|p| p == &entry.path) {
                continue;
            }
        }
        // Domain policy: only files with applicable catalogue patterns are
        // reviewable. Not "skipped" — simply out of scope for the catalogue.
        let patterns = patterns_for_language(catalogue, &entry.language);
        if patterns.is_empty() {
            continue;
        }
        let bytes = match std::fs::read(root.join(&entry.path)) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("{}: read failed: {e:#}", entry.path);
                continue;
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                summary.files_skipped += 1;
                continue;
            }
        };
        // Skip policy mirrors the analyzer's own bounds so CLI and MCP agree
        // on what's reviewable: binary content and oversized files are out.
        if text.contains('\0') || text.len() > MAX_FILE_CHARS {
            summary.files_skipped += 1;
            continue;
        }

        let neighbors = gather_neighbors(root, entry, graph, index);
        let relevant = relevant_for(&entry.path, knowledge);
        let request = FileReviewRequest {
            entry,
            text: &text,
            neighbors: &neighbors,
            patterns: &patterns,
            relevant: &relevant,
        };

        match reviewer.review_file(request, now).await {
            Ok(outcome) => {
                if outcome.from_cache {
                    summary.files_cached += 1;
                } else {
                    summary.files_scanned += 1;
                }
                for f in outcome.findings {
                    if store.upsert_from_analysis(f).is_ok() {
                        summary.findings_persisted += 1;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("{}: review failed: {e:#}", entry.path);
                summary.errors.push((entry.path.clone(), format!("{e:#}")));
            }
        }
    }

    Ok(summary)
}

/// Gather up to `MAX_NEIGHBORS` import/imported-by/same-package neighbours
/// of a primary file, reading their text for cross-file context. Shared by
/// both review paths so the prompt context is identical. (Moved here from
/// the CLI runner so the MCP path uses the same neighbour selection.)
pub fn gather_neighbors(
    root: &Path,
    entry: &FileEntry,
    graph: &DependencyGraph,
    index: &ProjectIndex,
) -> Vec<NeighborFile> {
    let (imports, imported_by) = neighbors_of(graph, &entry.path);
    let mut picks: Vec<(String, NeighborRelation)> = Vec::new();
    let ia: Vec<&str> = imports.into_iter().take(MAX_NEIGHBORS).collect();
    let ib: Vec<&str> = imported_by.into_iter().take(MAX_NEIGHBORS).collect();
    for i in 0..MAX_NEIGHBORS {
        if picks.len() >= MAX_NEIGHBORS {
            break;
        }
        if let Some(p) = ia.get(i) {
            picks.push((p.to_string(), NeighborRelation::Imports));
        }
        if picks.len() >= MAX_NEIGHBORS {
            break;
        }
        if let Some(p) = ib.get(i) {
            picks.push((p.to_string(), NeighborRelation::ImportedBy));
        }
    }
    // Same-package fallback (JVM/.NET case): siblings in the same directory
    // and language that the import resolver can't see.
    if picks.len() < MAX_NEIGHBORS {
        let taken: std::collections::HashSet<&str> = picks
            .iter()
            .map(|(p, _)| p.as_str())
            .chain(std::iter::once(entry.path.as_str()))
            .collect();
        let dir = entry.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let mut siblings: Vec<&FileEntry> = index
            .files
            .iter()
            .filter(|f| !taken.contains(f.path.as_str()))
            .filter(|f| f.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("") == dir)
            .filter(|f| f.language == entry.language)
            .collect();
        siblings.sort_by(|a, b| a.path.cmp(&b.path));
        for sib in siblings {
            if picks.len() >= MAX_NEIGHBORS {
                break;
            }
            picks.push((sib.path.clone(), NeighborRelation::SamePackage));
        }
    }

    let by_path: std::collections::HashMap<&str, &FileEntry> =
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

#[cfg(test)]
mod tests {
    use super::*;
    use codeup_core::catalogue::DefaultSeverity;

    fn pat(id: &str) -> CataloguePattern {
        CataloguePattern {
            id: id.into(),
            name: id.into(),
            languages: vec!["rust".into()],
            default_severity: DefaultSeverity::Medium,
            hint: "x".into(),
        }
    }

    #[test]
    fn parses_bare_array() {
        let pats = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = pats.iter().collect();
        let text = r#"[{"category":"god-class","severity":"high","line":12,"explanation":"too big","confidence":0.8}]"#;
        let out = parse_reported_findings(text, &refs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].category, "god-class");
        assert_eq!(out[0].line, 12);
    }

    #[test]
    fn parses_fenced_array_with_prose() {
        let pats = [pat("long-method")];
        let refs: Vec<&CataloguePattern> = pats.iter().collect();
        let text = "Here are the findings:\n```json\n[\n  {\"category\":\"long-method\",\"severity\":\"medium\",\"line\":3,\"explanation\":\"long\",\"confidence\":0.5}\n]\n```\nThat's all.";
        let out = parse_reported_findings(text, &refs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].category, "long-method");
    }

    #[test]
    fn empty_array_yields_nothing() {
        let pats = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = pats.iter().collect();
        assert!(parse_reported_findings("[]", &refs).is_empty());
        assert!(parse_reported_findings("No findings.", &refs).is_empty());
    }

    #[test]
    fn drops_unknown_category() {
        let pats = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = pats.iter().collect();
        let text = r#"[{"category":"made-up","severity":"high","line":1,"explanation":"x","confidence":1}]"#;
        assert!(parse_reported_findings(text, &refs).is_empty());
    }

    #[test]
    fn brackets_inside_strings_dont_break_extraction() {
        let pats = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = pats.iter().collect();
        let text = r#"[{"category":"god-class","severity":"low","line":1,"explanation":"uses arr[0] and map[key] heavily","confidence":0.4}]"#;
        let out = parse_reported_findings(text, &refs);
        assert_eq!(out.len(), 1);
        assert!(out[0].explanation.contains("arr[0]"));
    }

    // A reviewer that never touches an LLM — the whole point of the port.
    // It reports one fixed finding per file it's asked about.
    struct FakeReviewer;
    impl FileReviewer for FakeReviewer {
        fn review_file<'a>(
            &'a self,
            request: FileReviewRequest<'a>,
            now: &'a str,
        ) -> BoxFuture<'a, Result<FileReviewOutcome>> {
            Box::pin(async move {
                let r = ReportedFinding {
                    category: request.patterns[0].id.clone(),
                    severity: "high".into(),
                    line: 1,
                    end_line: None,
                    explanation: "fake".into(),
                    suggested_remediation: None,
                    confidence: 0.9,
                };
                let finding = analyzer::make_finding(
                    &request.entry.path,
                    Some(request.entry.content_hash.clone()),
                    r,
                    "fake-model",
                    now,
                );
                Ok(FileReviewOutcome { findings: vec![finding], from_cache: false })
            })
        }
    }

    #[test]
    fn review_workspace_orchestrates_without_an_llm() {
        use codeup_core::catalogue::load_catalogue;
        use codeup_core::scanner::graph::build_graph;
        use codeup_core::scanner::scan_workspace;

        // Minimal real workspace: one Rust file the catalogue applies to.
        let root = std::env::temp_dir().join(format!("codeup-review-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn f() -> i32 { 1 }\n").unwrap();

        let now = "2026-01-01T00:00:00.000Z";
        let catalogue = load_catalogue(&[]).unwrap();
        let index = scan_workspace(&root, now.to_string()).unwrap();
        let graph = build_graph(&index);
        let knowledge = KnowledgeSnapshot { dismissals: vec![], exemplars: vec![] };
        let mut store = FindingsStore::load(&root).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let summary = rt
            .block_on(review_workspace(
                &root,
                None,
                &catalogue,
                &index,
                &graph,
                &knowledge,
                &FakeReviewer,
                &mut store,
                now,
            ))
            .unwrap();

        assert!(summary.files_scanned >= 1, "should have reviewed the rs file");
        assert_eq!(summary.findings_persisted, summary.files_scanned);
        assert!(summary.errors.is_empty());
        // The finding actually landed on disk via the store.
        assert!(store.all().count() >= 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}

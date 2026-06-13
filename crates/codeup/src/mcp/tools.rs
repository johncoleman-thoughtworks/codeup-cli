//! Tool + prompt implementations for `codeup mcp`.
//!
//! Keyless tools (deterministic scan, catalogue, graph neighbours, list /
//! save findings) work in every host. `codeup_review` additionally uses
//! the host's model via sampling (see `sampling.rs`). Everything reuses
//! `codeup-core` and the CLI's analyzer/store, so findings are identical
//! to those the `codeup scan` command produces.

use super::{sampling, ServerCtx};
use crate::analyzer;
use crate::review;
use crate::runner::{self, RunOptions};
use crate::store::FindingsStore;
use anyhow::{anyhow, Context, Result};
use codeup_core::catalogue::{load_catalogue, patterns_for_language, Catalogue, CataloguePattern, DefaultSeverity};
use codeup_core::scanner::graph::{build_graph, neighbors_of};
use codeup_core::scanner::scan_workspace;
use codeup_core::schema::{Finding, FindingLocation, HistoryEvent, Priority, Severity, Status};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Tool definitions advertised in `tools/list`. The schemas mirror
/// PLAN-MCP.md so a host/agent sees exactly the documented surface.
pub fn list() -> Value {
    json!([
        {
            "name": "codeup_deterministic_scan",
            "description": "Run codeup's deterministic checks (import cycles via Tarjan SCC, layer violations against .codeup/intent.yaml, oversized files). No model, no key. Persists findings to .codeup/findings/ and returns a summary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "Workspace root. Default: server cwd." },
                    "persist": { "type": "boolean", "default": true, "description": "Write findings to .codeup/findings/." }
                }
            }
        },
        {
            "name": "codeup_catalogue",
            "description": "Return the codeup pattern catalogue (id, name, languages, defaultSeverity, hint). Optionally filtered to one language.",
            "inputSchema": {
                "type": "object",
                "properties": { "language": { "type": "string", "description": "e.g. java, typescript, python. Omit for all." } }
            }
        },
        {
            "name": "codeup_graph_neighbors",
            "description": "Top-N dependency-graph neighbours of a file (imports, importedBy, samePackage) for cross-file context.",
            "inputSchema": {
                "type": "object",
                "required": ["file"],
                "properties": {
                    "file": { "type": "string" },
                    "root": { "type": "string" },
                    "n": { "type": "integer", "default": 6, "minimum": 0, "maximum": 20 }
                }
            }
        },
        {
            "name": "codeup_list_findings",
            "description": "List findings currently persisted under .codeup/findings/ for the workspace.",
            "inputSchema": {
                "type": "object",
                "properties": { "root": { "type": "string" } }
            }
        },
        {
            "name": "codeup_save_findings",
            "description": "Validate findings against the catalogue allowlist and write/merge them into .codeup/findings/ per the shared schema (stable ids, quoted timestamps). Use after a host agent has reasoned over code (host-delegation mode).",
            "inputSchema": {
                "type": "object",
                "required": ["findings"],
                "properties": {
                    "root": { "type": "string" },
                    "detectedBy": { "type": "string", "description": "Attribution for these findings. Default: codeup-mcp:host-delegation." },
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["category", "file", "line", "explanation"],
                            "properties": {
                                "category": { "type": "string", "description": "MUST be a catalogue pattern id." },
                                "file": { "type": "string" },
                                "line": { "type": "integer" },
                                "endLine": { "type": "integer" },
                                "severity": { "type": "string", "enum": ["low", "medium", "high"] },
                                "explanation": { "type": "string" },
                                "suggestedRemediation": { "type": "string" },
                                "confidence": { "type": "number" }
                            }
                        }
                    }
                }
            }
        },
        {
            "name": "codeup_review",
            "description": "Review files against the codeup pattern catalogue using the HOST's model (via MCP sampling — no API key). Runs deterministic checks too, persists all findings to .codeup/. Falls back to deterministic-only with a note when the client lacks sampling.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Files to review (workspace-relative). Default: whole root." },
                    "root": { "type": "string" },
                    "deterministic_only": { "type": "boolean", "default": false }
                }
            }
        }
    ])
}

/// The host-delegation prompt advertised in `prompts/list`. Surfaces in
/// hosts that lack sampling (e.g. Claude Code) so the host's *own* agent
/// loop reasons and then calls `codeup_save_findings`.
pub fn prompt_list() -> Value {
    json!([
        {
            "name": "codeup",
            "description": "Review code against codeup's architectural anti-pattern catalogue, then persist findings via codeup_save_findings. Use when sampling is unavailable.",
            "arguments": [
                { "name": "language", "description": "Language to load the catalogue for (e.g. java). Optional.", "required": false }
            ]
        }
    ])
}

/// Build the `prompts/get` response for the `codeup` prompt.
pub fn prompt_get(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let language = args.get("language").and_then(|v| v.as_str());
    let (catalogue, _custom) = load_catalogue_for(&ctx.root)?;
    let patterns: Vec<&CataloguePattern> = match language {
        Some(lang) => patterns_for_language(&catalogue, lang),
        None => catalogue.patterns.iter().collect(),
    };
    let mut lines = vec![
        "You are an expert software architect reviewing code against the codeup catalogue of architectural anti-patterns.".to_string(),
        "Read the files the user wants reviewed (and their import neighbours, via codeup_graph_neighbors, for cross-file findings). For each distinct issue you are confident about, identify the single catalogue pattern id it maps to.".to_string(),
        "Then call the codeup_save_findings tool with the findings — each finding's `category` MUST be one of the catalogue ids below. Do not invent categories, and do not report stylistic nitpicks.".to_string(),
        String::new(),
        "Catalogue:".to_string(),
    ];
    for p in &patterns {
        lines.push(format!("- {} ({}): {}", p.id, severity_str(p.default_severity), collapse_ws(&p.hint)));
    }
    let text = lines.join("\n");
    Ok(json!({
        "description": "codeup architectural review (host-delegation)",
        "messages": [
            { "role": "user", "content": { "type": "text", "text": text } }
        ]
    }))
}

/// Dispatch a `tools/call`. Returns the full tool result object
/// (`{content, isError}`). Execution failures surface as `isError: true`
/// results rather than JSON-RPC errors.
pub async fn call(ctx: &ServerCtx, name: &str, args: Value) -> Result<Value> {
    match name {
        "codeup_deterministic_scan" => deterministic_scan(ctx, args).await,
        "codeup_catalogue" => catalogue_tool(ctx, args),
        "codeup_graph_neighbors" => graph_neighbors(ctx, args),
        "codeup_list_findings" => list_findings(ctx, args),
        "codeup_save_findings" => save_findings(ctx, args),
        "codeup_review" => review_tool(ctx, args).await,
        other => Ok(super::protocol::error_content(format!("unknown tool: {other}"))),
    }
}

// ---- keyless tools -------------------------------------------------------

async fn deterministic_scan(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let root = resolve_root(ctx, &args)?;
    let persist = args.get("persist").and_then(|v| v.as_bool()).unwrap_or(true);
    let now = crate::chrono_now_iso();
    let summary = runner::run(RunOptions {
        root: &root,
        now: &now,
        deterministic_only: true,
        client: None,
        persist,
    })
    .await
    .context("deterministic scan")?;

    let out = json!({
        "root": summary.root.display().to_string(),
        "filesIndexed": summary.index.files.len(),
        "cycles": summary.cycle_count,
        "layerViolations": summary.layer_violation_count,
        "oversizedFiles": summary.oversized_count,
        "findings": summary.findings,
    });
    Ok(super::protocol::text_content(pretty(&out)))
}

fn catalogue_tool(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let (catalogue, _custom) = load_catalogue_for(&ctx.root)?;
    let language = args.get("language").and_then(|v| v.as_str());
    let patterns: Vec<&CataloguePattern> = match language {
        Some(lang) => patterns_for_language(&catalogue, lang),
        None => catalogue.patterns.iter().collect(),
    };
    let items: Vec<Value> = patterns
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "languages": p.languages,
                "defaultSeverity": severity_str(p.default_severity),
                "hint": p.hint,
            })
        })
        .collect();
    let out = json!({ "count": items.len(), "patterns": items });
    Ok(super::protocol::text_content(pretty(&out)))
}

fn graph_neighbors(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let root = resolve_root(ctx, &args)?;
    let file = args
        .get("file")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`file` is required"))?
        .to_string();
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .unwrap_or(6)
        .min(20) as usize;

    let now = crate::chrono_now_iso();
    let index = scan_workspace(&root, now).context("scanning workspace")?;
    let graph = build_graph(&index);
    let (imports, imported_by) = neighbors_of(&graph, &file);

    // Same-package fallback (JVM/.NET, where siblings reference each other
    // without an import the resolver can see). Mirrors runner::gather_neighbors.
    let lang = index.files.iter().find(|f| f.path == file).map(|f| f.language.as_str());
    let dir = file.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let same_package: Vec<&str> = index
        .files
        .iter()
        .filter(|f| f.path != file)
        .filter(|f| Some(f.language.as_str()) == lang)
        .filter(|f| f.path.rsplit_once('/').map(|(d, _)| d).unwrap_or("") == dir)
        .map(|f| f.path.as_str())
        .take(n)
        .collect();

    let take_n = |v: Vec<&str>| -> Vec<String> { v.into_iter().take(n).map(str::to_string).collect() };
    let out = json!({
        "file": file,
        "imports": take_n(imports),
        "importedBy": take_n(imported_by),
        "samePackage": same_package.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    });
    Ok(super::protocol::text_content(pretty(&out)))
}

fn list_findings(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let root = resolve_root(ctx, &args)?;
    let store = FindingsStore::load(&root).context("loading .codeup/findings")?;
    let findings: Vec<&Finding> = store.all().collect();
    let out = json!({ "count": findings.len(), "findings": findings });
    Ok(super::protocol::text_content(pretty(&out)))
}

fn save_findings(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let root = resolve_root(ctx, &args)?;
    let detected_by = args
        .get("detectedBy")
        .and_then(|v| v.as_str())
        .unwrap_or("codeup-mcp:host-delegation")
        .to_string();
    let items = args
        .get("findings")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("`findings` must be an array"))?;

    let (catalogue, _custom) = load_catalogue_for(&root)?;
    let all_patterns: Vec<&CataloguePattern> = catalogue.patterns.iter().collect();
    let now = crate::chrono_now_iso();

    let mut store = FindingsStore::load(&root).context("loading .codeup/findings")?;
    let mut saved = Vec::new();
    let mut rejected = Vec::new();

    for item in items {
        let Some(file) = item.get("file").and_then(|v| v.as_str()) else {
            rejected.push(json!({ "reason": "missing file", "item": item }));
            continue;
        };
        // Reuse the exact catalogue/severity/line validation the CLI uses.
        let Some(reported) = analyzer::validate_reported(item, &all_patterns) else {
            rejected.push(json!({ "reason": "failed catalogue validation (unknown category, bad severity, line < 1, or empty explanation)", "file": file }));
            continue;
        };
        let content_hash = content_hash_of(&root, file);
        let finding = build_finding(file, reported, &detected_by, &now, content_hash);
        match store.upsert_from_analysis(finding) {
            Ok(f) => saved.push(f.id.clone()),
            Err(e) => rejected.push(json!({ "reason": format!("{e}"), "file": file })),
        }
    }

    let out = json!({ "saved": saved.len(), "savedIds": saved, "rejected": rejected });
    Ok(super::protocol::text_content(pretty(&out)))
}

// ---- host-tokens review (sampling) --------------------------------------

async fn review_tool(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let root = resolve_root(ctx, &args)?;
    let deterministic_only = args
        .get("deterministic_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let scoped: Option<Vec<String>> = args.get("paths").and_then(|v| v.as_array()).map(|a| {
        a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()
    });

    let now = crate::chrono_now_iso();

    // Deterministic pass always runs first (keyless, persists to .codeup/).
    let det = runner::run(RunOptions {
        root: &root,
        now: &now,
        deterministic_only: true,
        client: None,
        persist: true,
    })
    .await
    .context("deterministic pass")?;

    let mut summary = json!({
        "root": root.display().to_string(),
        "deterministic": {
            "cycles": det.cycle_count,
            "layerViolations": det.layer_violation_count,
            "oversizedFiles": det.oversized_count,
        },
        "llm": Value::Null,
    });

    if deterministic_only {
        summary["llm"] = json!({ "skipped": "deterministic_only requested" });
        return Ok(super::protocol::text_content(pretty(&summary)));
    }

    if !ctx.supports_sampling.load(std::sync::atomic::Ordering::SeqCst) {
        // No host-tokens path available here. The deterministic findings
        // are persisted; the catalogue review should be driven via the
        // `codeup` prompt (host-delegation) instead.
        summary["llm"] = json!({
            "skipped": "host does not support MCP sampling; deterministic findings persisted. Use the `codeup` prompt (host-delegation) for the catalogue review, then codeup_save_findings.",
        });
        return Ok(super::protocol::text_content(pretty(&summary)));
    }

    // Host-tokens LLM pass via sampling.
    let (catalogue, custom) = load_catalogue_for(&root)?;
    let index = scan_workspace(&root, now.clone()).context("scanning workspace")?;
    let graph = build_graph(&index);
    let knowledge_snapshot = load_knowledge_snapshot(&root)?;

    let mut store = FindingsStore::load(&root).context("loading .codeup/findings")?;
    let mut llm_findings = 0usize;
    let mut llm_files = 0usize;
    let mut errors: Vec<Value> = Vec::new();

    for entry in &index.files {
        if let Some(scope) = &scoped {
            if !scope.iter().any(|p| p == &entry.path) {
                continue;
            }
        }
        let patterns = patterns_for_language(&catalogue, &entry.language);
        if patterns.is_empty() {
            continue;
        }
        let bytes = match std::fs::read(root.join(&entry.path)) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if text.len() > 60_000 {
            continue;
        }

        let neighbors = runner::gather_neighbors_pub(&root, entry, &graph, &index);
        let relevant = codeup_core::knowledge::relevant_for(&entry.path, &knowledge_snapshot);
        let system_prompt = analyzer::build_system_prompt(&patterns, !neighbors.is_empty(), &relevant);
        let user_prompt = format!(
            "{}{}",
            analyzer::build_user_prompt(entry, &text, &neighbors),
            review::SAMPLING_OUTPUT_INSTRUCTION
        );

        match sampling::create_message(ctx, &system_prompt, &user_prompt, review::SAMPLING_MAX_TOKENS).await {
            Ok(outcome) => {
                llm_files += 1;
                let detected_by = outcome.model.unwrap_or_else(|| "codeup-mcp:host-sampling".to_string());
                let reported = review::parse_reported_findings(&outcome.text, &patterns);
                for r in reported {
                    let content_hash = Some(entry.content_hash.clone());
                    let finding = build_finding(&entry.path, r, &detected_by, &now, content_hash);
                    if store.upsert_from_analysis(finding).is_ok() {
                        llm_findings += 1;
                    }
                }
            }
            Err(e) => {
                errors.push(json!({ "file": entry.path, "error": format!("{e}") }));
            }
        }
    }

    summary["llm"] = json!({
        "filesReviewed": llm_files,
        "findings": llm_findings,
        "errors": errors,
    });
    let _ = custom; // custom patterns already merged into `catalogue`
    Ok(super::protocol::text_content(pretty(&summary)))
}

// ---- helpers -------------------------------------------------------------

fn resolve_root(ctx: &ServerCtx, args: &Value) -> Result<PathBuf> {
    match args.get("root").and_then(|v| v.as_str()) {
        Some(r) if !r.is_empty() => Ok(PathBuf::from(r)),
        _ => Ok(ctx.root.clone()),
    }
}

fn load_catalogue_for(root: &Path) -> Result<(Catalogue, Vec<CataloguePattern>)> {
    let (_snap, custom) = crate::store::load_knowledge(root).context("loading workspace knowledge")?;
    let catalogue = load_catalogue(&custom).context("loading catalogue")?;
    Ok((catalogue, custom))
}

fn load_knowledge_snapshot(root: &Path) -> Result<codeup_core::knowledge::KnowledgeSnapshot> {
    let (snap, _custom) = crate::store::load_knowledge(root).context("loading workspace knowledge")?;
    Ok(snap)
}

/// Build a `Finding` from a validated `ReportedFinding`, deriving the id
/// with the same `stable_id` the CLI uses so re-runs reuse the same file.
fn build_finding(
    file: &str,
    r: crate::cache::ReportedFinding,
    detected_by: &str,
    now: &str,
    content_hash: Option<String>,
) -> Finding {
    let id = analyzer::stable_id(file, &r.category, r.line);
    let severity = match r.severity.as_str() {
        "high" => Severity::High,
        "low" => Severity::Low,
        _ => Severity::Medium,
    };
    let priority = match severity {
        Severity::Low => Priority::Low,
        Severity::Medium => Priority::Medium,
        Severity::High => Priority::High,
    };
    Finding {
        schema_version: 1,
        id,
        category: r.category,
        severity,
        status: Status::Unconfirmed,
        priority,
        location: FindingLocation {
            file: file.to_string(),
            line: Some(r.line),
            end_line: r.end_line,
            ast_path: None,
            content_hash,
        },
        explanation: r.explanation,
        suggested_remediation: r.suggested_remediation,
        detected_at: now.to_string(),
        detected_by: detected_by.to_string(),
        confidence: Some(r.confidence),
        history: vec![HistoryEvent {
            timestamp: now.to_string(),
            event: "detected".into(),
            by: None,
            from: None,
            to: None,
            note: None,
        }],
    }
}

fn content_hash_of(root: &Path, file: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(root.join(file)).ok()?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Some(hex::encode(h.finalize()))
}

fn severity_str(s: DefaultSeverity) -> &'static str {
    match s {
        DefaultSeverity::Low => "low",
        DefaultSeverity::Medium => "medium",
        DefaultSeverity::High => "high",
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

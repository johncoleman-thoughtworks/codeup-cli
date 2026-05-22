//! Single-file analyzer — port of TS `analyzer/analyze.ts`.
//! Builds the prompt, calls the LLM, validates tool-use blocks against
//! the catalogue, caches the raw result, returns the Findings.

use crate::cache::{AnalysisCache, ReportedFinding};
use crate::llm::{LLMAnalyzeRequest, LLMClient, ToolDefinition};
use anyhow::{Context, Result};
use codeup_core::cache::analysis_cache_key;
use codeup_core::catalogue::{Catalogue, CataloguePattern, DefaultSeverity};
use codeup_core::knowledge::{format_for_prompt, relevant_for, KnowledgeSnapshot};
use codeup_core::scanner::FileEntry;
use codeup_core::schema::{Finding, FindingLocation, HistoryEvent, Priority, Severity, Status};
use sha2::{Digest, Sha256};

const MAX_OUTPUT_TOKENS: u32 = 2048;
const MAX_FILE_CHARS: usize = 60_000;
const MAX_NEIGHBOR_CHARS: usize = 8_000;
pub const MAX_NEIGHBORS: usize = 6;

#[derive(Debug, Clone)]
pub enum NeighborRelation {
    Imports,
    ImportedBy,
    SamePackage,
}

impl NeighborRelation {
    fn as_str(&self) -> &'static str {
        match self {
            NeighborRelation::Imports => "imports",
            NeighborRelation::ImportedBy => "importedBy",
            NeighborRelation::SamePackage => "samePackage",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NeighborFile {
    pub path: String,
    /// Carried for future per-language neighbor formatting; the current
    /// prompt builder reads it via `text`, but downstream consumers (MCP
    /// server, alt reporters) will want it.
    #[allow(dead_code)]
    pub language: String,
    pub text: String,
    pub relation: NeighborRelation,
}

pub struct AnalyzeContext<'a> {
    pub catalogue: &'a Catalogue,
    pub knowledge: &'a KnowledgeSnapshot,
    pub custom_patterns: &'a [CataloguePattern],
    pub cache: &'a AnalysisCache,
    pub client: &'a LLMClient,
}

#[derive(Debug, Clone)]
pub struct AnalyzeResult {
    /// File this result is about. Used by structured reporters; the text
    /// reporter reads findings by location instead.
    #[allow(dead_code)]
    pub file: String,
    pub findings: Vec<Finding>,
    pub from_cache: bool,
    pub skipped: Option<&'static str>,
}

pub async fn analyze_file(
    entry: &FileEntry,
    text: &str,
    ctx: AnalyzeContext<'_>,
    neighbors: &[NeighborFile],
    now: &str,
) -> Result<AnalyzeResult> {
    let patterns = codeup_core::catalogue::patterns_for_language(ctx.catalogue, &entry.language);
    if patterns.is_empty() {
        return Ok(AnalyzeResult {
            file: entry.path.clone(),
            findings: vec![],
            from_cache: false,
            skipped: Some("no-patterns"),
        });
    }
    if text.contains('\0') {
        return Ok(AnalyzeResult {
            file: entry.path.clone(),
            findings: vec![],
            from_cache: false,
            skipped: Some("binary"),
        });
    }
    if text.len() > MAX_FILE_CHARS {
        return Ok(AnalyzeResult {
            file: entry.path.clone(),
            findings: vec![],
            from_cache: false,
            skipped: Some("too-large"),
        });
    }

    let model = ctx.client.model();
    let neighbors_key = neighbors_cache_key(neighbors);
    let knowledge_hash = ctx.knowledge.hash(ctx.custom_patterns);
    let cache_key = analysis_cache_key(
        &entry.content_hash,
        &ctx.catalogue.hash,
        &model,
        &format!("{neighbors_key}:{knowledge_hash}"),
    );

    let relevant = relevant_for(&entry.path, ctx.knowledge);
    let reported = if let Some(hit) = ctx.cache.get(&cache_key) {
        hit.findings
    } else {
        let system_prompt = build_system_prompt(&patterns, !neighbors.is_empty(), &relevant);
        let user_prompt = build_user_prompt(entry, text, neighbors);
        let tool = report_finding_tool();
        let resp = ctx
            .client
            .analyze(LLMAnalyzeRequest {
                system_prompt: &system_prompt,
                user_prompt: &user_prompt,
                tool: &tool,
                max_output_tokens: MAX_OUTPUT_TOKENS,
            })
            .await
            .context("LLM analyze call")?;
        let reported: Vec<ReportedFinding> = resp
            .tool_calls
            .into_iter()
            .filter(|c| c.name == "report_finding")
            .filter_map(|c| validate_reported(&c.input, &patterns))
            .collect();
        ctx.cache.put(&cache_key, reported.clone(), now.to_string())?;
        reported
    };

    let findings: Vec<Finding> = reported
        .into_iter()
        .map(|r| make_finding(entry, r, &model, now))
        .collect();

    Ok(AnalyzeResult {
        file: entry.path.clone(),
        findings,
        from_cache: false, // we always re-serialize, but findings round-tripped through cache when present
        skipped: None,
    })
}

fn neighbors_cache_key(neighbors: &[NeighborFile]) -> String {
    if neighbors.is_empty() {
        return String::new();
    }
    let mut sorted: Vec<&NeighborFile> = neighbors.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut blob = String::new();
    for n in sorted {
        let mut h = Sha256::new();
        h.update(n.text.as_bytes());
        let hash = hex::encode(h.finalize());
        blob.push_str(&n.path);
        blob.push('@');
        blob.push_str(&hash[..16]);
        blob.push('|');
    }
    let mut h = Sha256::new();
    h.update(blob.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

fn validate_reported(input: &serde_json::Value, patterns: &[&CataloguePattern]) -> Option<ReportedFinding> {
    let obj = input.as_object()?;
    let category = obj.get("category")?.as_str()?.to_string();
    if !patterns.iter().any(|p| p.id == category) {
        return None;
    }
    let severity = obj.get("severity")?.as_str()?.to_string();
    if !matches!(severity.as_str(), "low" | "medium" | "high") {
        return None;
    }
    let line = obj.get("line")?.as_u64()? as u32;
    if line == 0 {
        return None;
    }
    let end_line = obj.get("endLine").and_then(|v| v.as_u64()).map(|n| n as u32);
    let explanation = obj.get("explanation")?.as_str()?.to_string();
    if explanation.is_empty() {
        return None;
    }
    let confidence = obj.get("confidence").and_then(|v| v.as_f64())? as f32;
    if !confidence.is_finite() {
        return None;
    }
    let suggested_remediation = obj
        .get("suggestedRemediation")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(ReportedFinding {
        category,
        severity,
        line,
        end_line,
        explanation,
        suggested_remediation,
        confidence,
    })
}

fn make_finding(entry: &FileEntry, r: ReportedFinding, model: &str, now: &str) -> Finding {
    let id = stable_id(&entry.path, &r.category, r.line);
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
            file: entry.path.clone(),
            line: Some(r.line),
            end_line: r.end_line,
            ast_path: None,
            content_hash: Some(entry.content_hash.clone()),
        },
        explanation: r.explanation,
        suggested_remediation: r.suggested_remediation,
        detected_at: now.to_string(),
        detected_by: model.to_string(),
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

fn stable_id(file: &str, category: &str, line: u32) -> String {
    let mut h = Sha256::new();
    h.update(format!("{file}:{category}:{line}").as_bytes());
    let hex = hex::encode(h.finalize());
    format!("{category}-{}", &hex[..12])
}

fn build_system_prompt(
    patterns: &[&CataloguePattern],
    with_neighbors: bool,
    knowledge: &codeup_core::knowledge::RelevantKnowledge,
) -> String {
    let mut lines: Vec<String> = vec![
        "You are an expert software architect performing a code review focused on architectural anti-patterns.".into(),
        if with_neighbors {
            "You will receive a primary source file under review, plus a few neighboring files (importers and imported modules) for context. Only emit findings about lines in the PRIMARY file — neighbor files are context only.".into()
        } else {
            "You will receive a single source file and a catalogue of patterns to look for.".into()
        },
        "Use the report_finding tool to report each distinct issue. Do not narrate; only emit tool calls.".into(),
        "Surface plausible candidates rather than only certainties. A noisy false positive is one click to dismiss; a missed real issue is invisible. Err toward reporting when the catalogue hint plausibly matches what you see.".into(),
        "Still: do not report stylistic nitpicks, formatting issues, or generic \"could be cleaner\" suggestions — each finding must map to a specific catalogue pattern id.".into(),
        "Match the line number to where the issue is most visible in the primary file. Use the confidence field honestly — 0.9 for textbook instances, 0.5 for plausible-but-arguable, 0.3 for \"worth a look.\" Always report; never use confidence as a gate.".into(),
    ];
    if with_neighbors {
        lines.push("Neighbor relations: `imports` = the primary file imports this one; `importedBy` = this file imports the primary; `samePackage` = lives in the same directory (e.g. same Java/Kotlin/C# package) — useful when languages reference siblings without explicit imports.".into());
        lines.push("Cross-file patterns worth looking for now that you have neighbor context: shotgun-surgery, type-leakage-across-boundaries, feature-envy, misplaced-responsibility, non-exclusive-subtypes (when the primary file extends/implements a parent and `samePackage` neighbors are other subtypes of the same parent — ask whether a real-world instance could reasonably be more than one of these subtypes at once).".into());
    }
    let fragment = format_for_prompt(knowledge);
    if !fragment.is_empty() {
        lines.push(fragment);
    }
    lines.push(String::new());
    lines.push("Catalogue:".into());
    for p in patterns {
        lines.push(format!("- id: {}", p.id));
        lines.push(format!("  name: {}", p.name));
        lines.push(format!("  defaultSeverity: {}", severity_str(p.default_severity)));
        lines.push(format!("  hint: {}", collapse_whitespace(&p.hint)));
    }
    lines.join("\n")
}

fn severity_str(s: DefaultSeverity) -> &'static str {
    match s {
        DefaultSeverity::Low => "low",
        DefaultSeverity::Medium => "medium",
        DefaultSeverity::High => "high",
    }
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_user_prompt(entry: &FileEntry, text: &str, neighbors: &[NeighborFile]) -> String {
    let mut lines: Vec<String> = vec![
        format!("PRIMARY FILE (analyze this one): {}", entry.path),
        format!("Language: {}", entry.language),
        String::new(),
        "```".into(),
        text.to_string(),
        "```".into(),
    ];
    if !neighbors.is_empty() {
        lines.push(String::new());
        lines.push("--- NEIGHBOR FILES (context only — do not emit findings about these) ---".into());
        for n in neighbors {
            let snippet = if n.text.len() > MAX_NEIGHBOR_CHARS {
                let mut s = n.text[..MAX_NEIGHBOR_CHARS].to_string();
                s.push_str("\n... (truncated)");
                s
            } else {
                n.text.clone()
            };
            lines.push(String::new());
            lines.push(format!("Neighbor ({}): {}", n.relation.as_str(), n.path));
            lines.push("```".into());
            lines.push(snippet);
            lines.push("```".into());
        }
    }
    lines.join("\n")
}

fn report_finding_tool() -> ToolDefinition {
    ToolDefinition {
        name: "report_finding".into(),
        description: "Report a single architectural anti-pattern finding in the file under review. Call once per distinct issue. Each finding must map to a specific catalogue pattern id.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "category": {"type": "string", "description": "Pattern id from the provided catalogue."},
                "severity": {"type": "string", "enum": ["low", "medium", "high"]},
                "line": {"type": "integer", "description": "1-based starting line."},
                "endLine": {"type": "integer", "description": "1-based ending line (inclusive)."},
                "explanation": {"type": "string", "description": "Why this is an instance of the pattern. 2-5 sentences."},
                "suggestedRemediation": {"type": "string"},
                "confidence": {"type": "number", "description": "0..1. NOT a gate; always emit the call."}
            },
            "required": ["category", "severity", "line", "explanation", "confidence"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(id: &str) -> CataloguePattern {
        CataloguePattern {
            id: id.into(),
            name: id.into(),
            languages: vec!["typescript".into()],
            default_severity: DefaultSeverity::Medium,
            hint: "x".into(),
        }
    }

    #[test]
    fn validate_reported_accepts_well_formed() {
        let patterns = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = patterns.iter().collect();
        let input = serde_json::json!({
            "category": "god-class",
            "severity": "high",
            "line": 7,
            "explanation": "lots",
            "confidence": 0.8
        });
        let r = validate_reported(&input, &refs).expect("valid");
        assert_eq!(r.category, "god-class");
        assert_eq!(r.line, 7);
    }

    #[test]
    fn validate_reported_rejects_unknown_category() {
        let patterns = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = patterns.iter().collect();
        let input = serde_json::json!({"category": "made-up", "severity": "high", "line": 1, "explanation": "x", "confidence": 1});
        assert!(validate_reported(&input, &refs).is_none());
    }

    #[test]
    fn validate_reported_rejects_bad_severity() {
        let patterns = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = patterns.iter().collect();
        let input = serde_json::json!({"category": "god-class", "severity": "extreme", "line": 1, "explanation": "x", "confidence": 1});
        assert!(validate_reported(&input, &refs).is_none());
    }

    #[test]
    fn validate_reported_rejects_line_zero() {
        let patterns = [pat("god-class")];
        let refs: Vec<&CataloguePattern> = patterns.iter().collect();
        let input = serde_json::json!({"category": "god-class", "severity": "high", "line": 0, "explanation": "x", "confidence": 1});
        assert!(validate_reported(&input, &refs).is_none());
    }

    #[test]
    fn stable_id_deterministic_and_category_prefixed() {
        let a = stable_id("src/foo.ts", "long-method", 42);
        let b = stable_id("src/foo.ts", "long-method", 42);
        assert_eq!(a, b);
        assert!(a.starts_with("long-method-"));
    }

    #[test]
    fn stable_id_differs_on_each_input() {
        let base = stable_id("src/foo.ts", "long-method", 42);
        assert_ne!(stable_id("src/bar.ts", "long-method", 42), base);
        assert_ne!(stable_id("src/foo.ts", "god-class", 42), base);
        assert_ne!(stable_id("src/foo.ts", "long-method", 43), base);
    }

    #[test]
    fn neighbors_cache_key_empty_when_no_neighbors() {
        assert_eq!(neighbors_cache_key(&[]), "");
    }

    #[test]
    fn neighbors_cache_key_order_independent() {
        let a = vec![
            NeighborFile { path: "a.ts".into(), language: "typescript".into(), text: "X".into(), relation: NeighborRelation::Imports },
            NeighborFile { path: "b.ts".into(), language: "typescript".into(), text: "Y".into(), relation: NeighborRelation::Imports },
        ];
        let b = vec![
            NeighborFile { path: "b.ts".into(), language: "typescript".into(), text: "Y".into(), relation: NeighborRelation::Imports },
            NeighborFile { path: "a.ts".into(), language: "typescript".into(), text: "X".into(), relation: NeighborRelation::Imports },
        ];
        assert_eq!(neighbors_cache_key(&a), neighbors_cache_key(&b));
    }
}

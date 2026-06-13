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

use crate::analyzer::{self};
use crate::cache::ReportedFinding;
use codeup_core::catalogue::CataloguePattern;

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
}

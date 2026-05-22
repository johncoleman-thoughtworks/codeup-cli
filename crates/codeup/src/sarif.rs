//! SARIF 2.1.0 reporter.
//!
//! GitHub Code Scanning ingests SARIF directly via `actions/upload-sarif`,
//! so this is the primary CI integration surface. We emit the minimum
//! valid shape: one `run`, one `tool.driver` (codeup), one `rule` per
//! distinct finding category that fired, one `result` per non-suppressed
//! finding. Dismissed / fixed findings are skipped — the local YAML is the
//! source of truth for those.
//!
//! Severity mapping: high → error, medium → warning, low → note.
//! `partialFingerprints.codeupId/v1` carries the stable finding id so
//! GitHub can dedupe across runs even if line numbers drift.
//!
//! Reference: <https://docs.oasis-open.org/sarif/sarif/v2.1.0/cos02/sarif-v2.1.0-cos02.html>

use codeup_core::schema::{Finding, Severity, Status};
use serde::Serialize;
use std::collections::BTreeMap;

const SCHEMA_URL: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/cos02/schemas/sarif-schema-2.1.0.json";
const SARIF_VERSION: &str = "2.1.0";
const TOOL_NAME: &str = "codeup";
const TOOL_INFO_URI: &str = "https://github.com/johncoleman-thoughtworks/codeup-cli";

#[derive(Serialize)]
struct SarifLog<'a> {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<Run<'a>>,
}

#[derive(Serialize)]
struct Run<'a> {
    tool: Tool<'a>,
    results: Vec<SarifResult<'a>>,
}

#[derive(Serialize)]
struct Tool<'a> {
    driver: Driver<'a>,
}

#[derive(Serialize)]
struct Driver<'a> {
    name: &'static str,
    version: &'static str,
    #[serde(rename = "informationUri")]
    information_uri: &'static str,
    rules: Vec<Rule<'a>>,
}

#[derive(Serialize)]
struct Rule<'a> {
    id: &'a str,
    name: &'a str,
    #[serde(rename = "shortDescription")]
    short_description: Text,
    #[serde(rename = "defaultConfiguration")]
    default_configuration: DefaultConfig,
}

#[derive(Serialize)]
struct DefaultConfig {
    level: &'static str,
}

#[derive(Serialize)]
struct SarifResult<'a> {
    #[serde(rename = "ruleId")]
    rule_id: &'a str,
    level: &'static str,
    message: Text,
    locations: Vec<Location<'a>>,
    #[serde(rename = "partialFingerprints")]
    partial_fingerprints: BTreeMap<&'static str, &'a str>,
}

#[derive(Serialize)]
struct Text {
    text: String,
}

#[derive(Serialize)]
struct Location<'a> {
    #[serde(rename = "physicalLocation")]
    physical_location: PhysicalLocation<'a>,
}

#[derive(Serialize)]
struct PhysicalLocation<'a> {
    #[serde(rename = "artifactLocation")]
    artifact_location: ArtifactLocation<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<Region>,
}

#[derive(Serialize)]
struct ArtifactLocation<'a> {
    uri: &'a str,
}

#[derive(Serialize)]
struct Region {
    #[serde(rename = "startLine")]
    start_line: u32,
    #[serde(rename = "endLine", skip_serializing_if = "Option::is_none")]
    end_line: Option<u32>,
}

fn severity_level(s: Severity) -> &'static str {
    match s {
        Severity::High => "error",
        Severity::Medium => "warning",
        Severity::Low => "note",
    }
}

fn is_suppressed(f: &Finding) -> bool {
    matches!(f.status, Status::Dismissed | Status::Fixed)
}

/// Render the scan's findings as SARIF 2.1.0 JSON (pretty-printed).
pub fn render(findings: &[Finding]) -> String {
    // Tool driver requires the rules array — collect a deterministic, deduped
    // list of categories that actually fired, picking the strongest severity
    // observed for each so the defaultConfiguration is meaningful.
    let mut rule_levels: BTreeMap<&str, Severity> = BTreeMap::new();
    for f in findings.iter().filter(|f| !is_suppressed(f)) {
        rule_levels
            .entry(f.category.as_str())
            .and_modify(|cur| {
                if severity_rank(f.severity) > severity_rank(*cur) {
                    *cur = f.severity;
                }
            })
            .or_insert(f.severity);
    }

    let rules: Vec<Rule> = rule_levels
        .iter()
        .map(|(cat, sev)| Rule {
            id: cat,
            name: cat,
            short_description: Text { text: humanize_category(cat) },
            default_configuration: DefaultConfig { level: severity_level(*sev) },
        })
        .collect();

    let results: Vec<SarifResult> = findings
        .iter()
        .filter(|f| !is_suppressed(f))
        .map(|f| {
            let mut fingerprints = BTreeMap::new();
            fingerprints.insert("codeupId/v1", f.id.as_str());
            SarifResult {
                rule_id: &f.category,
                level: severity_level(f.severity),
                message: Text { text: f.explanation.clone() },
                locations: vec![Location {
                    physical_location: PhysicalLocation {
                        artifact_location: ArtifactLocation { uri: &f.location.file },
                        region: f.location.line.map(|line| Region {
                            start_line: line,
                            end_line: f.location.end_line,
                        }),
                    },
                }],
                partial_fingerprints: fingerprints,
            }
        })
        .collect();

    let log = SarifLog {
        schema: SCHEMA_URL,
        version: SARIF_VERSION,
        runs: vec![Run {
            tool: Tool {
                driver: Driver {
                    name: TOOL_NAME,
                    version: env!("CARGO_PKG_VERSION"),
                    information_uri: TOOL_INFO_URI,
                    rules,
                },
            },
            results,
        }],
    };

    serde_json::to_string_pretty(&log).expect("SARIF serialization is infallible")
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Low => 1,
        Severity::Medium => 2,
        Severity::High => 3,
    }
}

/// Turn `anemic-domain-model` into `Anemic domain model` for rule
/// shortDescription. Good enough — the catalogue's full description
/// belongs in `fullDescription` later if we choose to pull it through.
fn humanize_category(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut chars = id.chars();
    if let Some(c) = chars.next() {
        out.extend(c.to_uppercase());
    }
    for c in chars {
        if c == '-' || c == '_' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeup_core::schema::{Finding, FindingLocation, Priority};
    use serde_json::Value;

    fn sample(id: &str, category: &str, sev: Severity, status: Status) -> Finding {
        Finding {
            schema_version: 1,
            id: id.into(),
            category: category.into(),
            severity: sev,
            status,
            priority: Priority::High,
            location: FindingLocation {
                file: "src/a.rs".into(),
                line: Some(10),
                end_line: Some(20),
                ast_path: None,
                content_hash: None,
            },
            explanation: format!("why {category} matters"),
            suggested_remediation: None,
            detected_at: "2026-05-22T07:55:01.123Z".into(),
            detected_by: "test".into(),
            confidence: None,
            history: vec![],
        }
    }

    #[test]
    fn renders_minimal_valid_sarif_shape() {
        let findings = vec![
            sample("a-1", "anemic-domain-model", Severity::High, Status::Unconfirmed),
            sample("b-1", "primitive-obsession", Severity::Medium, Status::Confirmed),
            sample("c-1", "dead-code", Severity::Low, Status::Dismissed), // suppressed
        ];
        let json = render(&findings);
        let v: Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["version"], "2.1.0");
        assert!(v["$schema"].as_str().unwrap().contains("sarif-schema-2.1.0"));
        let run = &v["runs"][0];
        assert_eq!(run["tool"]["driver"]["name"], "codeup");
        let rules = run["tool"]["driver"]["rules"].as_array().unwrap();
        // Dismissed category not present — only 2 rules
        assert_eq!(rules.len(), 2);
        let rule_ids: Vec<&str> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert!(rule_ids.contains(&"anemic-domain-model"));
        assert!(rule_ids.contains(&"primitive-obsession"));

        let results = run["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        let r0 = &results[0];
        assert_eq!(r0["level"], "error");
        assert_eq!(r0["locations"][0]["physicalLocation"]["region"]["startLine"], 10);
        assert_eq!(r0["partialFingerprints"]["codeupId/v1"], "a-1");
    }

    #[test]
    fn severity_levels_map_to_sarif() {
        assert_eq!(severity_level(Severity::High), "error");
        assert_eq!(severity_level(Severity::Medium), "warning");
        assert_eq!(severity_level(Severity::Low), "note");
    }

    #[test]
    fn rule_takes_strongest_severity_seen() {
        let findings = vec![
            sample("a-1", "primitive-obsession", Severity::Low, Status::Unconfirmed),
            sample("a-2", "primitive-obsession", Severity::High, Status::Unconfirmed),
        ];
        let json = render(&findings);
        let v: Value = serde_json::from_str(&json).unwrap();
        let rules = v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["defaultConfiguration"]["level"], "error");
    }

    #[test]
    fn humanize_category_inserts_spaces() {
        assert_eq!(humanize_category("anemic-domain-model"), "Anemic domain model");
        assert_eq!(humanize_category("dead-code"), "Dead code");
    }
}

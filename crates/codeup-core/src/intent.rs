//! Intent layers + deterministic checks — mirrors TS
//! `intent/{layers,check}.ts`.
//!
//! Layer rules are glob-matched paths; cycle detection consumes the
//! dependency graph (built later). All findings produced here are
//! `detectedBy = "codeup-deterministic"` with confidence 1.

use crate::schema::{Finding, FindingLocation, HistoryEvent, Priority, Severity, Status};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DETECTOR: &str = "codeup-deterministic";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerRule {
    pub layer: String,
    pub r#match: String,
    #[serde(rename = "cannotDependOn")]
    pub cannot_depend_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntentConfig {
    pub layers: Vec<LayerRule>,
}

/// Match a file against a layer rule's glob pattern. Trailing-slash
/// directory rules auto-extend to `pattern + "**"` so simple prefix
/// rules ("src/foo/") still work.
pub fn matches_rule(file: &str, pattern: &str) -> bool {
    let effective = normalize_pattern(pattern);
    match GlobBuilder::new(&effective).literal_separator(true).build() {
        Ok(g) => g.compile_matcher().is_match(file),
        Err(_) => false,
    }
}

fn normalize_pattern(pattern: &str) -> String {
    if pattern.ends_with('/') {
        format!("{pattern}**")
    } else {
        pattern.to_string()
    }
}

/// Identify the layer a file belongs to. Most-specific (longest pattern)
/// wins; ties broken by declaration order.
pub fn layer_for_file<'a>(file: &str, intent: &'a IntentConfig) -> Option<&'a str> {
    let mut best: Option<&LayerRule> = None;
    let mut best_len = 0usize;
    for rule in &intent.layers {
        if matches_rule(file, &rule.r#match) && rule.r#match.len() > best_len {
            best_len = rule.r#match.len();
            best = Some(rule);
        }
    }
    best.map(|r| r.layer.as_str())
}

#[derive(Debug, Clone)]
pub struct Cycle {
    pub files: Vec<String>,
}

/// Produce a Finding for each cycle. Format mirrors the TS implementation
/// so the YAML on disk is bit-for-bit equivalent.
pub fn cycle_findings(cycles: &[Cycle], now: &str) -> Vec<Finding> {
    cycles
        .iter()
        .map(|cycle| {
            let head = cycle.files.first().cloned().unwrap_or_default();
            let id = stable_id("cyclic-dependency", &cycle.files.join("|"));
            let is_self = cycle.files.len() == 1;
            let explanation = if is_self {
                format!(
                    "{head} imports from itself (transitive self-loop in the module graph)."
                )
            } else {
                let chain = {
                    let mut v = cycle.files.clone();
                    if let Some(first) = cycle.files.first() {
                        v.push(first.clone());
                    }
                    v.join(" → ")
                };
                format!(
                    "Cyclic import chain across {} files:\n\n{}\n\nCycles make these files impossible to reason about or test in isolation; usually signals a missing abstraction that wants to live in a separate module.",
                    cycle.files.len(),
                    chain
                )
            };
            base_finding(
                &id,
                "cyclic-dependency",
                Severity::High,
                &head,
                &explanation,
                "Extract the shared concept into a third module that both can depend on, or invert the dependency direction so the lower-level module no longer reaches into the higher-level one.",
                now,
            )
        })
        .collect()
}

/// Produce a Finding for each forbidden import edge. Takes a borrowed
/// edge set: (from_file, to_file) tuples — the caller passes the graph.
pub fn layer_violations<'a, I>(edges: I, intent: &IntentConfig, now: &str) -> Vec<Finding>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut out = Vec::new();
    for (from, to) in edges {
        let Some(from_layer) = layer_for_file(from, intent) else { continue };
        let Some(rule) = intent.layers.iter().find(|l| l.layer == from_layer) else { continue };
        if rule.cannot_depend_on.is_empty() {
            continue;
        }
        let Some(to_layer) = layer_for_file(to, intent) else { continue };
        if !rule.cannot_depend_on.iter().any(|s| s == to_layer) {
            continue;
        }
        let id = stable_id("layer-violation", &format!("{from}->{to}"));
        let explanation = format!(
            "Layer \"{from_layer}\" ({from}) imports from layer \"{to_layer}\" ({to}). Configured intent in .codeup/intent.yaml prohibits this direction."
        );
        let remediation = format!(
            "Move the shared abstraction down into a layer that \"{from_layer}\" is allowed to depend on, or invert the call via an interface defined in \"{from_layer}\" and implemented in \"{to_layer}\"."
        );
        out.push(base_finding(
            &id,
            "layer-violation",
            Severity::High,
            from,
            &explanation,
            &remediation,
            now,
        ));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn base_finding(
    id: &str,
    category: &str,
    severity: Severity,
    file: &str,
    explanation: &str,
    remediation: &str,
    now: &str,
) -> Finding {
    Finding {
        schema_version: 1,
        id: id.to_string(),
        category: category.to_string(),
        severity,
        status: Status::Unconfirmed,
        priority: match severity {
            Severity::Low => Priority::Low,
            Severity::Medium => Priority::Medium,
            Severity::High => Priority::High,
        },
        location: FindingLocation {
            file: file.to_string(),
            line: None,
            end_line: None,
            ast_path: None,
            content_hash: None,
        },
        explanation: explanation.to_string(),
        suggested_remediation: Some(remediation.to_string()),
        detected_at: now.to_string(),
        detected_by: DETECTOR.to_string(),
        confidence: Some(1.0),
        history: vec![HistoryEvent {
            timestamp: now.to_string(),
            event: "detected".to_string(),
            by: None,
            from: None,
            to: None,
            note: None,
        }],
    }
}

fn stable_id(category: &str, key: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{category}:{key}").as_bytes());
    // TS uses SHA-1 + 12 hex chars. SHA-256 + 12 is functionally equivalent
    // for our id stability purposes (collision-resistant prefix).
    let hex = hex::encode(h.finalize());
    format!("{category}-{}", &hex[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> IntentConfig {
        IntentConfig {
            layers: vec![
                LayerRule {
                    layer: "domain".into(),
                    r#match: "src/main/java/com/x/domain/".into(),
                    cannot_depend_on: vec!["infrastructure".into(), "web".into()],
                },
                LayerRule {
                    layer: "application".into(),
                    r#match: "src/main/java/com/x/application/".into(),
                    cannot_depend_on: vec!["web".into()],
                },
                LayerRule {
                    layer: "web".into(),
                    r#match: "src/main/java/com/x/web/".into(),
                    cannot_depend_on: vec![],
                },
                LayerRule {
                    layer: "infrastructure".into(),
                    r#match: "src/main/java/com/x/infrastructure/".into(),
                    cannot_depend_on: vec![],
                },
                LayerRule {
                    layer: "everything".into(),
                    r#match: "src/main/java/com/x/".into(),
                    cannot_depend_on: vec![],
                },
            ],
        }
    }

    #[test]
    fn layer_for_file_longest_prefix_wins() {
        let c = cfg();
        assert_eq!(layer_for_file("src/main/java/com/x/domain/Order.java", &c), Some("domain"));
        assert_eq!(layer_for_file("src/main/java/com/x/web/Controller.java", &c), Some("web"));
        assert_eq!(layer_for_file("src/main/java/com/x/other/Thing.java", &c), Some("everything"));
    }

    #[test]
    fn layer_for_file_unknown_returns_none() {
        assert!(layer_for_file("src/test/X.java", &cfg()).is_none());
    }

    #[test]
    fn monorepo_glob_matches_across_packages() {
        let c = IntentConfig {
            layers: vec![
                LayerRule {
                    layer: "domain".into(),
                    r#match: "packages/*/src/**/domain/**".into(),
                    cannot_depend_on: vec!["infrastructure".into(), "web".into()],
                },
                LayerRule {
                    layer: "web".into(),
                    r#match: "packages/*/src/**/web/**".into(),
                    cannot_depend_on: vec![],
                },
            ],
        };
        assert_eq!(
            layer_for_file("packages/api/src/main/java/domain/Order.java", &c),
            Some("domain")
        );
        assert_eq!(
            layer_for_file("packages/worker/src/main/kotlin/web/Handler.kt", &c),
            Some("web")
        );
        assert!(layer_for_file("packages/api/README.md", &c).is_none());
    }

    #[test]
    fn trailing_slash_prefix_backcompat() {
        let c = IntentConfig {
            layers: vec![LayerRule {
                layer: "domain".into(),
                r#match: "src/domain/".into(),
                cannot_depend_on: vec![],
            }],
        };
        assert_eq!(layer_for_file("src/domain/Order.java", &c), Some("domain"));
        assert_eq!(
            layer_for_file("src/domain/nested/Deep.java", &c),
            Some("domain")
        );
        assert_eq!(layer_for_file("src/web/Other.java", &c), None);
    }

    #[test]
    fn cycle_findings_one_per_cycle_all_high() {
        let cycles = vec![
            Cycle { files: vec!["src/a.ts".into(), "src/b.ts".into()] },
            Cycle { files: vec!["src/x.ts".into(), "src/y.ts".into(), "src/z.ts".into()] },
        ];
        let findings = cycle_findings(&cycles, "2026-01-01T00:00:00Z");
        assert_eq!(findings.len(), 2);
        for f in &findings {
            assert_eq!(f.category, "cyclic-dependency");
            assert!(matches!(f.severity, Severity::High));
            assert!(matches!(f.status, Status::Unconfirmed));
            assert_eq!(f.detected_by, "codeup-deterministic");
        }
    }

    #[test]
    fn cycle_findings_have_stable_ids() {
        let cycles = vec![Cycle { files: vec!["src/a.ts".into(), "src/b.ts".into()] }];
        let a = cycle_findings(&cycles, "t").remove(0).id;
        let b = cycle_findings(&cycles, "t").remove(0).id;
        assert_eq!(a, b);
    }

    #[test]
    fn layer_violation_flags_forbidden_edge() {
        let intent = IntentConfig {
            layers: vec![
                LayerRule {
                    layer: "domain".into(),
                    r#match: "src/domain/".into(),
                    cannot_depend_on: vec!["infrastructure".into()],
                },
                LayerRule {
                    layer: "infrastructure".into(),
                    r#match: "src/infrastructure/".into(),
                    cannot_depend_on: vec![],
                },
            ],
        };
        let edges = vec![("src/domain/Order.java", "src/infrastructure/Db.java")];
        let findings = layer_violations(edges, &intent, "t");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, "layer-violation");
        assert_eq!(findings[0].location.file, "src/domain/Order.java");
    }

    #[test]
    fn layer_violation_quiet_when_allowed() {
        let intent = IntentConfig {
            layers: vec![
                LayerRule {
                    layer: "domain".into(),
                    r#match: "src/domain/".into(),
                    cannot_depend_on: vec!["infrastructure".into()],
                },
                LayerRule {
                    layer: "infrastructure".into(),
                    r#match: "src/infrastructure/".into(),
                    cannot_depend_on: vec![],
                },
            ],
        };
        let edges = vec![("src/infrastructure/Db.java", "src/domain/Order.java")];
        assert!(layer_violations(edges, &intent, "t").is_empty());
    }
}

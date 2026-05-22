//! Knowledge schema + retrieval — mirrors TS `knowledge/{schema,retrieve}.ts`.
//!
//! Dismissals and exemplars accumulated by the team are read in here and
//! formatted into a "Project conventions" prompt fragment. Pure: no I/O,
//! no fs. Callers (CLI / future MCP server) handle YAML load.

use crate::catalogue::CataloguePattern;
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const MAX_DISMISSALS: usize = 3;
pub const MAX_EXEMPLARS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissalEntry {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub id: String,
    pub category: String,
    #[serde(rename = "filePathPattern")]
    pub file_path_pattern: String,
    pub rationale: String,
    #[serde(rename = "dismissedAt")]
    pub dismissed_at: String,
    #[serde(rename = "dismissedBy")]
    pub dismissed_by: String,
    #[serde(rename = "originalFindingId")]
    pub original_finding_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExemplarEntry {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub id: String,
    pub category: String,
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub excerpt: String,
    #[serde(rename = "confirmedAt")]
    pub confirmed_at: String,
    #[serde(rename = "confirmedBy")]
    pub confirmed_by: String,
    #[serde(rename = "originalFindingId")]
    pub original_finding_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissalsFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<DismissalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExemplarsFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<ExemplarEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPatternsFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default)]
    pub patterns: Vec<CataloguePattern>,
}

#[derive(Debug, Clone, Default)]
pub struct KnowledgeSnapshot {
    pub dismissals: Vec<DismissalEntry>,
    pub exemplars: Vec<ExemplarEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct RelevantKnowledge {
    pub dismissals: Vec<DismissalEntry>,
    pub exemplars: Vec<ExemplarEntry>,
}

impl KnowledgeSnapshot {
    /// Compute a hash of the knowledge state — used as part of the
    /// analyzer's cache key. Matches the JS implementation (top-level
    /// SHA-256 over the structural fields, sliced to 16 hex chars).
    pub fn hash(&self, custom_patterns: &[CataloguePattern]) -> String {
        #[derive(Serialize)]
        struct D<'a> { c: &'a str, p: &'a str, r: &'a str }
        #[derive(Serialize)]
        struct E<'a> { c: &'a str, f: &'a str, x: &'a str }

        let d: Vec<_> = self
            .dismissals
            .iter()
            .map(|e| D { c: &e.category, p: &e.file_path_pattern, r: &e.rationale })
            .collect();
        let e: Vec<_> = self
            .exemplars
            .iter()
            .map(|e| E { c: &e.category, f: &e.file_path, x: &e.excerpt })
            .collect();
        let p: Vec<String> = custom_patterns
            .iter()
            .map(|p| format!("{}:{}", p.id, p.hint))
            .collect();

        let blob = serde_json::json!({ "d": d, "e": e, "p": p }).to_string();
        let mut h = Sha256::new();
        h.update(blob.as_bytes());
        hex::encode(h.finalize())[..16].to_string()
    }
}

/// Find dismissal and exemplar entries relevant to analyzing `file_path`.
pub fn relevant_for(file_path: &str, snapshot: &KnowledgeSnapshot) -> RelevantKnowledge {
    let mut dismissals: Vec<DismissalEntry> = snapshot
        .dismissals
        .iter()
        .filter(|d| matches_glob(file_path, &d.file_path_pattern))
        .cloned()
        .collect();
    dismissals = dedupe_by_category(dismissals, MAX_DISMISSALS, |d| d.category.clone());

    let file_dir = parent_dir(file_path);
    let mut exemplars: Vec<(i32, ExemplarEntry)> = snapshot
        .exemplars
        .iter()
        .map(|e| (directory_proximity(&file_dir, &parent_dir(&e.file_path)), e.clone()))
        .collect();
    exemplars.sort_by(|a, b| b.0.cmp(&a.0));
    let exemplars: Vec<ExemplarEntry> = exemplars.into_iter().map(|(_, e)| e).collect();
    let exemplars = dedupe_by_category(exemplars, MAX_EXEMPLARS, |e| e.category.clone());

    RelevantKnowledge { dismissals, exemplars }
}

/// Format relevant knowledge as a system-prompt fragment. Returns an empty
/// string when nothing is relevant so the prompt stays tight.
pub fn format_for_prompt(k: &RelevantKnowledge) -> String {
    if k.dismissals.is_empty() && k.exemplars.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        String::new(),
        "Project conventions (from this team's prior dismissals and confirmations):".into(),
    ];
    if !k.dismissals.is_empty() {
        lines.push(String::new());
        lines.push("Patterns previously dismissed as not-applicable in this project:".into());
        for d in &k.dismissals {
            let rat = collapse_whitespace(&d.rationale);
            lines.push(format!(
                "- {} (files matching `{}`): {}",
                d.category, d.file_path_pattern, rat
            ));
        }
        lines.push("Take these dismissals seriously — if the case in front of you matches the dismissed pattern's situation, do not report it. If your case is meaningfully different, report it but acknowledge the prior dismissal in your explanation.".into());
    }
    if !k.exemplars.is_empty() {
        lines.push(String::new());
        lines.push("Patterns confirmed as real instances in this project (use as positive examples):".into());
        for e in &k.exemplars {
            let ex: String = collapse_whitespace(&e.excerpt).chars().take(300).collect();
            lines.push(format!("- {} confirmed in {}: {}", e.category, e.file_path, ex));
        }
    }
    lines.join("\n")
}

pub fn matches_glob(file_path: &str, pattern: &str) -> bool {
    if pattern == file_path {
        return true;
    }
    // literal_separator(true) matches minimatch's default: a single `*`
    // does NOT cross `/` boundaries; only `**` does.
    match GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(g) => g.compile_matcher().is_match(file_path),
        Err(_) => false,
    }
}

fn parent_dir(file_path: &str) -> String {
    Path::new(file_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn directory_proximity(a: &str, b: &str) -> i32 {
    if a == b {
        return 100;
    }
    let a_segs: Vec<&str> = a.split('/').collect();
    let b_segs: Vec<&str> = b.split('/').collect();
    let mut shared = 0i32;
    for i in 0..a_segs.len().min(b_segs.len()) {
        if a_segs[i] == b_segs[i] {
            shared += 1;
        } else {
            break;
        }
    }
    shared * 10
}

fn dedupe_by_category<T, F>(items: Vec<T>, cap: usize, key: F) -> Vec<T>
where
    F: Fn(&T) -> String,
{
    let mut out = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for item in items {
        let k = key(&item);
        let n = seen.entry(k).or_insert(0);
        if *n >= cap {
            continue;
        }
        *n += 1;
        out.push(item);
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dismissal(category: &str, pat: &str, rationale: &str) -> DismissalEntry {
        DismissalEntry {
            schema_version: 1,
            id: format!("d-{category}-{pat}"),
            category: category.into(),
            file_path_pattern: pat.into(),
            rationale: rationale.into(),
            dismissed_at: "2026-01-01T00:00:00Z".into(),
            dismissed_by: "tester".into(),
            original_finding_id: "f-1".into(),
        }
    }

    fn exemplar(category: &str, file: &str, excerpt: &str) -> ExemplarEntry {
        ExemplarEntry {
            schema_version: 1,
            id: format!("e-{category}-{file}"),
            category: category.into(),
            file_path: file.into(),
            excerpt: excerpt.into(),
            confirmed_at: "2026-01-01T00:00:00Z".into(),
            confirmed_by: "tester".into(),
            original_finding_id: "f-1".into(),
        }
    }

    #[test]
    fn glob_exact_path() {
        assert!(matches_glob("src/foo.ts", "src/foo.ts"));
    }

    #[test]
    fn glob_double_star() {
        assert!(matches_glob("src/test/x/y/z.test.ts", "src/test/**"));
        assert!(matches_glob("src/test/x.ts", "src/test/**"));
        assert!(!matches_glob("src/main/x.ts", "src/test/**"));
    }

    #[test]
    fn glob_single_star_does_not_cross_dirs() {
        assert!(matches_glob("src/foo.ts", "src/*.ts"));
        assert!(!matches_glob("src/a/foo.ts", "src/*.ts"));
    }

    #[test]
    fn dismissals_filtered_by_glob() {
        let snap = KnowledgeSnapshot {
            dismissals: vec![
                dismissal("long-method", "src/test/**", "tests are allowed long methods"),
                dismissal("long-method", "src/main/**", "irrelevant"),
            ],
            exemplars: vec![],
        };
        let r = relevant_for("src/test/foo.test.ts", &snap);
        assert_eq!(r.dismissals.len(), 1);
        assert_eq!(r.dismissals[0].rationale, "tests are allowed long methods");
    }

    #[test]
    fn exemplars_ranked_by_proximity() {
        let snap = KnowledgeSnapshot {
            dismissals: vec![],
            exemplars: vec![
                exemplar("anemic-domain-model", "src/unrelated/X.java", "far"),
                exemplar("anemic-domain-model", "src/domain/order/OrderItem.java", "same dir"),
                exemplar("anemic-domain-model", "src/domain/customer/Customer.java", "sibling"),
            ],
        };
        let r = relevant_for("src/domain/order/Order.java", &snap);
        assert_eq!(r.exemplars[0].excerpt, "same dir");
    }

    #[test]
    fn caps_per_category() {
        let mut dismissals = Vec::new();
        for i in 0..10 {
            dismissals.push(dismissal("long-method", "**", &format!("r{i}")));
        }
        let snap = KnowledgeSnapshot { dismissals, exemplars: vec![] };
        let r = relevant_for("src/x.ts", &snap);
        assert_eq!(r.dismissals.len(), MAX_DISMISSALS);
    }

    #[test]
    fn format_for_prompt_empty() {
        let out = format_for_prompt(&RelevantKnowledge::default());
        assert!(out.is_empty());
    }

    #[test]
    fn format_for_prompt_populated() {
        let k = RelevantKnowledge {
            dismissals: vec![dismissal("long-method", "src/test/**", "tests can be long")],
            exemplars: vec![exemplar("anemic-domain-model", "src/domain/X.java", "classic case")],
        };
        let out = format_for_prompt(&k);
        assert!(out.contains("Patterns previously dismissed"));
        assert!(out.contains("tests can be long"));
        assert!(out.contains("Patterns confirmed as real instances"));
        assert!(out.contains("classic case"));
    }
}

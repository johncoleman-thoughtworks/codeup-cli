//! Deterministic quality checks. Currently: oversized-file.
//! Mirrors TS `quality/sizeCheck.ts`.

use crate::scanner::walk::ProjectIndex;
use crate::schema::{Finding, FindingLocation, HistoryEvent, Priority, Severity, Status};
use sha2::{Digest, Sha256};

const DETECTOR: &str = "codeup-deterministic";

#[derive(Debug, Clone, Copy)]
pub struct SizeCheckOptions {
    pub warn_bytes: u64,
    pub critical_bytes: u64,
}

impl Default for SizeCheckOptions {
    fn default() -> Self {
        Self {
            warn_bytes: 30_000,
            critical_bytes: 60_000,
        }
    }
}

/// Languages that map to "actual source code" — the only files for
/// which oversized-file is meaningful signal. Data formats (YAML / JSON /
/// TOML), docs (markdown), and plain text all routinely exceed the warn
/// threshold for legitimate reasons (catalogues, schemas, fixtures) and
/// flagging them just adds noise to the report.
fn is_source_language(lang: &str) -> bool {
    !matches!(
        lang,
        "yaml" | "json" | "toml" | "markdown" | "plaintext" | "html" | "css" | "scss" | "sql"
    )
}

pub fn oversized_files(index: &ProjectIndex, options: SizeCheckOptions, now: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for file in &index.files {
        if file.size < options.warn_bytes {
            continue;
        }
        if !is_source_language(&file.language) {
            continue;
        }
        let is_critical = file.size >= options.critical_bytes;
        let severity = if is_critical { Severity::High } else { Severity::Medium };
        let id = stable_id("oversized-file", &file.path);
        let size = file.size;
        let warn = options.warn_bytes;
        let critical = options.critical_bytes;
        let explanation = if is_critical {
            format!(
                "This file is {size} bytes — beyond Codeup's {critical}-byte analysis cap. The deep LLM scan was skipped for this file; only deterministic checks ran. The size itself is the finding: at this scale, navigation, code review, merge-conflict surface area, and Codeup's own reasoning quality all suffer."
            )
        } else {
            format!(
                "This file is {size} bytes — past the {warn}-byte warning threshold. Navigation, review, and merge-conflict surface area all grow with file size. Consider splitting along natural concern lines before the file grows further."
            )
        };
        let remediation = "Split along concern boundaries — distinct classes / responsibilities / aggregates that have grown into one file usually want their own. If this file is generated code or large test fixtures, add it to .gitignore or the scanner exclude list so Codeup stops analyzing it. If the size is deliberate and acceptable, dismiss with a rationale so the knowledge base remembers.";
        out.push(Finding {
            schema_version: 1,
            id,
            category: "oversized-file".into(),
            severity,
            status: Status::Unconfirmed,
            priority: match severity {
                Severity::Low => Priority::Low,
                Severity::Medium => Priority::Medium,
                Severity::High => Priority::High,
            },
            location: FindingLocation {
                file: file.path.clone(),
                line: Some(1),
                end_line: None,
                ast_path: None,
                content_hash: Some(file.content_hash.clone()),
            },
            explanation,
            suggested_remediation: Some(remediation.into()),
            detected_at: now.to_string(),
            detected_by: DETECTOR.into(),
            confidence: Some(1.0),
            history: vec![HistoryEvent {
                timestamp: now.to_string(),
                event: "detected".into(),
                by: None,
                from: None,
                to: None,
                note: None,
            }],
        });
    }
    out
}

fn stable_id(category: &str, key: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{category}:{key}").as_bytes());
    let hex = hex::encode(h.finalize());
    format!("{category}-{}", &hex[..12])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::walk::FileEntry;

    fn entry(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.into(),
            language: "typescript".into(),
            size,
            content_hash: format!("h_{path}"),
            mtime: 0,
            raw_imports: vec![],
        }
    }

    fn index(files: Vec<FileEntry>) -> ProjectIndex {
        ProjectIndex {
            schema_version: 1,
            generated_at: String::new(),
            root_name: "r".into(),
            files,
        }
    }

    #[test]
    fn no_findings_below_warn() {
        let idx = index(vec![entry("small.ts", 1000), entry("medium.ts", 29_999)]);
        assert!(oversized_files(&idx, SizeCheckOptions::default(), "t").is_empty());
    }

    #[test]
    fn medium_severity_between_warn_and_critical() {
        let idx = index(vec![entry("big.ts", 45_000)]);
        let findings = oversized_files(&idx, SizeCheckOptions::default(), "t");
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Medium));
        assert_eq!(findings[0].category, "oversized-file");
        assert!(findings[0].explanation.contains("45000"));
    }

    #[test]
    fn high_severity_at_critical() {
        let idx = index(vec![entry("huge.ts", 80_000)]);
        let findings = oversized_files(&idx, SizeCheckOptions::default(), "t");
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::High));
        assert!(findings[0].explanation.contains("beyond Codeup's"));
    }

    #[test]
    fn boundary_thresholds() {
        let idx = index(vec![entry("at-warn.ts", 30_000), entry("at-critical.ts", 60_000)]);
        let findings = oversized_files(&idx, SizeCheckOptions::default(), "t");
        assert_eq!(findings.len(), 2);
        let warn_sev = findings.iter().find(|f| f.location.file == "at-warn.ts").unwrap();
        let crit_sev = findings.iter().find(|f| f.location.file == "at-critical.ts").unwrap();
        assert!(matches!(warn_sev.severity, Severity::Medium));
        assert!(matches!(crit_sev.severity, Severity::High));
    }

    #[test]
    fn stable_ids_across_runs() {
        let idx = index(vec![entry("foo.ts", 50_000)]);
        let a = oversized_files(&idx, SizeCheckOptions::default(), "t1").remove(0).id;
        let b = oversized_files(&idx, SizeCheckOptions::default(), "t2").remove(0).id;
        assert_eq!(a, b);
    }

    #[test]
    fn custom_thresholds_respected() {
        let idx = index(vec![entry("foo.ts", 5_000)]);
        let findings = oversized_files(
            &idx,
            SizeCheckOptions { warn_bytes: 1_000, critical_bytes: 10_000 },
            "t",
        );
        assert_eq!(findings.len(), 1);
        assert!(matches!(findings[0].severity, Severity::Medium));
    }
}

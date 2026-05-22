//! Disk I/O for `.codeup/`. Findings YAML (one file per finding,
//! upsert-preserves-status), knowledge YAML (dismissals + exemplars +
//! custom catalogue patterns), intent YAML. Migration runner is applied
//! on every load.

use anyhow::{anyhow, Context, Result};
use codeup_core::catalogue::CataloguePattern;
use codeup_core::intent::IntentConfig;
use codeup_core::knowledge::{
    CustomPatternsFile, DismissalsFile, ExemplarsFile,
    KnowledgeSnapshot,
};
use codeup_core::migrations::{
    run_migrations, CUSTOM_PATTERNS_CURRENT_VERSION, CUSTOM_PATTERNS_MIGRATIONS,
    DISMISSAL_CURRENT_VERSION, DISMISSAL_MIGRATIONS, EXEMPLAR_CURRENT_VERSION,
    EXEMPLAR_MIGRATIONS, FINDING_CURRENT_VERSION, FINDING_MIGRATIONS,
};
use codeup_core::schema::Finding;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FINDINGS_REL: &str = ".codeup/findings";
pub const KNOWLEDGE_REL: &str = ".codeup/knowledge";
pub const INTENT_REL: &str = ".codeup/intent.yaml";

pub struct FindingsStore {
    root: PathBuf,
    by_id: BTreeMap<String, Finding>,
}

impl FindingsStore {
    pub fn load(root: &Path) -> Result<Self> {
        let dir = root.join(FINDINGS_REL);
        let mut by_id = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self { root: root.to_path_buf(), by_id });
        }
        for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {dir:?}"))? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".yaml") && !name.ends_with(".yml") {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let raw: serde_yaml::Value = match serde_yaml::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("findings: {name}: {e}");
                    continue;
                }
            };
            let mig = match run_migrations(raw, name, FINDING_CURRENT_VERSION, FINDING_MIGRATIONS) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("findings: {name}: {e}");
                    continue;
                }
            };
            let finding: Finding = match serde_yaml::from_value(mig.value) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("findings: {name}: {e}");
                    continue;
                }
            };
            // Belt-and-braces: never surface findings pointing at .codeup/
            if finding.location.file == ".codeup" || finding.location.file.starts_with(".codeup/") {
                continue;
            }
            by_id.insert(finding.id.clone(), finding);
        }
        Ok(Self { root: root.to_path_buf(), by_id })
    }

    pub fn all(&self) -> impl Iterator<Item = &Finding> {
        self.by_id.values()
    }

    /// Lookup by id. Used by the future intent suggester and MCP server
    /// surfaces; the scan command doesn't need it directly.
    #[allow(dead_code)]
    pub fn get(&self, id: &str) -> Option<&Finding> {
        self.by_id.get(id)
    }

    /// Upsert: if a finding with the same id exists, preserve its
    /// status / priority / history but refresh the analytical fields.
    /// Otherwise insert as `unconfirmed` with a `detected` history event.
    pub fn upsert_from_analysis(&mut self, new: Finding) -> Result<&Finding> {
        let merged = match self.by_id.get(&new.id) {
            Some(existing) => Finding {
                schema_version: existing.schema_version,
                id: existing.id.clone(),
                category: new.category,
                severity: new.severity,
                status: existing.status,
                priority: existing.priority,
                location: new.location,
                explanation: new.explanation,
                suggested_remediation: new.suggested_remediation,
                detected_at: existing.detected_at.clone(),
                detected_by: new.detected_by,
                confidence: new.confidence,
                history: existing.history.clone(),
            },
            None => Finding {
                history: vec![codeup_core::schema::HistoryEvent {
                    timestamp: new.detected_at.clone(),
                    event: "detected".into(),
                    by: None,
                    from: None,
                    to: None,
                    note: None,
                }],
                ..new
            },
        };
        self.save(&merged)?;
        let id = merged.id.clone();
        self.by_id.insert(id.clone(), merged);
        Ok(self.by_id.get(&id).unwrap())
    }

    fn save(&self, finding: &Finding) -> Result<()> {
        let dir = self.root.join(FINDINGS_REL);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.yaml", finding.id));
        let body = serde_yaml::to_string(finding)?;
        std::fs::write(&path, body)?;
        Ok(())
    }
}

pub fn load_knowledge(root: &Path) -> Result<(KnowledgeSnapshot, Vec<CataloguePattern>)> {
    let dir = root.join(KNOWLEDGE_REL);
    let dismissals = read_yaml_with_migration::<DismissalsFile>(
        &dir.join("dismissals.yaml"),
        DISMISSAL_CURRENT_VERSION,
        DISMISSAL_MIGRATIONS,
    )?
    .map(|f| f.entries)
    .unwrap_or_default();
    let exemplars = read_yaml_with_migration::<ExemplarsFile>(
        &dir.join("exemplars.yaml"),
        EXEMPLAR_CURRENT_VERSION,
        EXEMPLAR_MIGRATIONS,
    )?
    .map(|f| f.entries)
    .unwrap_or_default();
    let custom_patterns = read_yaml_with_migration::<CustomPatternsFile>(
        &dir.join("patterns.yaml"),
        CUSTOM_PATTERNS_CURRENT_VERSION,
        CUSTOM_PATTERNS_MIGRATIONS,
    )?
    .map(|f| f.patterns)
    .unwrap_or_default();
    Ok((
        KnowledgeSnapshot { dismissals, exemplars },
        custom_patterns,
    ))
}

pub fn load_intent(root: &Path) -> Result<Option<IntentConfig>> {
    let path = root.join(INTENT_REL);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {path:?}"))?;
    let intent: IntentConfig = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("parsing {path:?}"))?;
    Ok(Some(intent))
}

fn read_yaml_with_migration<T: serde::de::DeserializeOwned>(
    path: &Path,
    current_version: u32,
    migrations: &[codeup_core::migrations::Migration],
) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {path:?}"))?;
    let raw: serde_yaml::Value = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("parsing YAML at {path:?}"))?;
    let display = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let mig = run_migrations(raw, display, current_version, migrations)
        .map_err(|e| anyhow!("{e}"))?;
    let value: T = serde_yaml::from_value(mig.value)
        .with_context(|| format!("decoding {path:?} into typed value"))?;
    Ok(Some(value))
}

// Re-export DismissalEntry / ExemplarEntry so callers don't need to dig
// into codeup_core; keeps the CLI's import surface flat.

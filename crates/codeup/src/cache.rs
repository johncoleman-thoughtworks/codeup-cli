//! Per-entry analysis cache at `.codeup/cache/entries/<hash>.json`.
//! Lazy-loaded: get() reads from disk on miss, no global load on startup.

use crate::store::{safe_create_dir_all, safe_write_yaml};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const VALID_SEVERITIES: &[&str] = &["high", "medium", "low"];

const ENTRIES_REL: &str = ".codeup/cache/entries";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: String,
    #[serde(rename = "analyzedAt")]
    pub analyzed_at: String,
    pub findings: Vec<ReportedFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportedFinding {
    pub category: String,
    pub severity: String,
    pub line: u32,
    #[serde(rename = "endLine", skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    pub explanation: String,
    #[serde(rename = "suggestedRemediation", skip_serializing_if = "Option::is_none")]
    pub suggested_remediation: Option<String>,
    pub confidence: f32,
}

fn validate_cache_entry(entry: &CacheEntry, path: &std::path::Path) -> Result<()> {
    for f in &entry.findings {
        if !VALID_SEVERITIES.contains(&f.severity.as_str()) {
            anyhow::bail!("cache entry {path:?}: invalid severity {:?}", f.severity);
        }
        if !(0.0..=1.0).contains(&f.confidence) {
            anyhow::bail!("cache entry {path:?}: confidence {} out of [0,1]", f.confidence);
        }
        if f.line == 0 {
            anyhow::bail!("cache entry {path:?}: line must be > 0");
        }
        if f.category.is_empty()
            || !f.category.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        {
            anyhow::bail!("cache entry {path:?}: invalid category {:?}", f.category);
        }
    }
    Ok(())
}

pub struct AnalysisCache {
    root: PathBuf,
}

impl AnalysisCache {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }

    pub fn get(&self, key: &str) -> Result<Option<CacheEntry>> {
        let path = self.entry_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading cache entry {path:?}"))?;
        let entry: CacheEntry = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing cache entry {path:?}"))?;
        validate_cache_entry(&entry, &path)?;
        Ok(Some(entry))
    }

    pub fn put(&self, key: &str, findings: Vec<ReportedFinding>, now: String) -> Result<()> {
        let entry = CacheEntry { key: key.to_string(), analyzed_at: now, findings };
        let dir = self.root.join(ENTRIES_REL);
        safe_create_dir_all(&self.root, &dir)?;
        // Drop a self-ignoring .gitignore once, using the same symlink-safe
        // write path so a planted symlink at .codeup/cache/.gitignore can't
        // redirect the write.
        let gi_dir = self.root.join(".codeup/cache");
        if !gi_dir.join(".gitignore").exists() {
            if safe_create_dir_all(&self.root, &gi_dir).is_ok() {
                let _ = safe_write_yaml(
                    &self.root,
                    &gi_dir,
                    ".gitignore",
                    "# Codeup-generated state. Safe to delete; will be regenerated on next scan.\n*\n!.gitignore\n",
                );
            }
        }
        let path = self.entry_path(key);
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("invalid entry path {path:?}"))?
            .to_string();
        let body = serde_json::to_string_pretty(&entry)?;
        safe_write_yaml(&self.root, &dir, &filename, &body)?;
        Ok(())
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        let mut h = Sha256::new();
        h.update(key.as_bytes());
        let hex = hex::encode(h.finalize());
        self.root.join(ENTRIES_REL).join(format!("{}.json", &hex[..32]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_finding() -> ReportedFinding {
        ReportedFinding {
            category: "long-method".into(),
            severity: "low".into(),
            line: 1,
            end_line: None,
            explanation: "too long".into(),
            suggested_remediation: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn get_returns_ok_none_on_true_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AnalysisCache::new(dir.path());
        assert!(matches!(cache.get("no-such-key"), Ok(None)));
    }

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AnalysisCache::new(dir.path());
        cache.put("k1", vec![make_finding()], "2026-01-01T00:00:00Z".into()).unwrap();
        let hit = cache.get("k1").unwrap().expect("should be a cache hit");
        assert_eq!(hit.key, "k1");
        assert_eq!(hit.findings.len(), 1);
        assert_eq!(hit.findings[0].category, "long-method");
    }

    #[test]
    fn get_returns_err_on_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AnalysisCache::new(dir.path());
        // Write a valid entry first so the path exists, then corrupt it.
        cache.put("k2", vec![], "2026-01-01T00:00:00Z".into()).unwrap();
        let path = cache.entry_path("k2");
        std::fs::write(&path, b"not json at all {{{{").unwrap();
        assert!(cache.get("k2").is_err(), "corrupt JSON must return Err, not None");
    }
}

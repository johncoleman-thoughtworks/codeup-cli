//! Per-entry analysis cache at `.codeup/cache/entries/<hash>.json`.
//! Lazy-loaded: get() reads from disk on miss, no global load on startup.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

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

pub struct AnalysisCache {
    root: PathBuf,
}

impl AnalysisCache {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }

    pub fn get(&self, key: &str) -> Option<CacheEntry> {
        let path = self.entry_path(key);
        if !path.exists() {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn put(&self, key: &str, findings: Vec<ReportedFinding>, now: String) -> Result<()> {
        let entry = CacheEntry { key: key.to_string(), analyzed_at: now, findings };
        let dir = self.root.join(ENTRIES_REL);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir:?}"))?;
        // Drop a self-ignoring .gitignore once.
        let gi = self.root.join(".codeup/cache/.gitignore");
        if !gi.exists() {
            let _ = std::fs::create_dir_all(gi.parent().unwrap());
            let _ = std::fs::write(&gi, "# Codeup-generated state. Safe to delete; will be regenerated on next scan.\n*\n!.gitignore\n");
        }
        let path = self.entry_path(key);
        let body = serde_json::to_vec_pretty(&entry)?;
        std::fs::write(&path, body)?;
        Ok(())
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        let mut h = Sha256::new();
        h.update(key.as_bytes());
        let hex = hex::encode(h.finalize());
        self.root.join(ENTRIES_REL).join(format!("{}.json", &hex[..32]))
    }
}

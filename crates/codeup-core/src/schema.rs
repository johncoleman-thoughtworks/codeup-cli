//! Wire-compatible types matching the TypeScript extension's YAML schema.
//! The same `.codeup/findings/*.yaml` produced by the CLI must round-trip
//! through the extension and vice versa.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Unconfirmed,
    Confirmed,
    Dismissed,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Ignore,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingLocation {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(rename = "endLine", skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(rename = "astPath", skip_serializing_if = "Option::is_none")]
    pub ast_path: Option<String>,
    #[serde(rename = "contentHash", skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub timestamp: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub id: String,
    pub category: String,
    pub severity: Severity,
    pub status: Status,
    pub priority: Priority,
    pub location: FindingLocation,
    pub explanation: String,
    #[serde(rename = "suggestedRemediation", skip_serializing_if = "Option::is_none")]
    pub suggested_remediation: Option<String>,
    #[serde(rename = "detectedAt")]
    pub detected_at: String,
    #[serde(rename = "detectedBy")]
    pub detected_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub history: Vec<HistoryEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_yaml_round_trip() {
        let f = Finding {
            schema_version: 1,
            id: "sample-1".into(),
            category: "anemic-domain-model".into(),
            severity: Severity::High,
            status: Status::Unconfirmed,
            priority: Priority::High,
            location: FindingLocation {
                file: "src/foo.ts".into(),
                line: Some(12),
                end_line: None,
                ast_path: None,
                content_hash: None,
            },
            explanation: "x".into(),
            suggested_remediation: None,
            detected_at: "2026-01-01T00:00:00Z".into(),
            detected_by: "human".into(),
            confidence: None,
            history: vec![],
        };
        let yaml = serde_yaml::to_string(&f).unwrap();
        let back: Finding = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.id, "sample-1");
        assert_eq!(back.severity, Severity::High);
    }
}

//! Catalogue loader — mirrors TS `catalogue/loader.ts`.
//!
//! The default catalogue is fetched from codeup-vscx via
//! `scripts/sync-catalogue.sh` and embedded at compile time. Workspace
//! overrides (from `.codeup/knowledge/patterns.yaml`) merge on top by id.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_YAML: &str = include_str!("../resources/default.yaml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CataloguePattern {
    pub id: String,
    pub name: String,
    pub languages: Vec<String>,
    #[serde(rename = "defaultSeverity")]
    pub default_severity: DefaultSeverity,
    pub hint: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogueFile {
    #[serde(default)]
    #[allow(dead_code)]
    schema_version: u32,
    patterns: Vec<CataloguePattern>,
}

#[derive(Debug, Clone)]
pub struct Catalogue {
    pub patterns: Vec<CataloguePattern>,
    pub hash: String,
}

/// Load the default catalogue + merge any workspace overrides on top.
///
/// `overrides` typically comes from `.codeup/knowledge/patterns.yaml` —
/// pass an empty slice if no overrides exist. Patterns with matching `id`
/// are replaced; new ids are appended.
pub fn load_catalogue(overrides: &[CataloguePattern]) -> Result<Catalogue, serde_yaml::Error> {
    let parsed: CatalogueFile = serde_yaml::from_str(DEFAULT_YAML)?;
    let merged = merge_patterns(&parsed.patterns, overrides);
    let override_blob = if overrides.is_empty() {
        String::new()
    } else {
        // Stable serialization of the override-relevant fields so the hash
        // is deterministic across runs that supply the same overrides.
        let normalised: Vec<_> = overrides
            .iter()
            .map(|p| {
                (
                    p.id.as_str(),
                    p.hint.as_str(),
                    p.default_severity,
                    p.languages.as_slice(),
                )
            })
            .collect();
        serde_json::to_string(&normalised).unwrap_or_default()
    };
    let mut hasher = Sha256::new();
    hasher.update(DEFAULT_YAML.as_bytes());
    hasher.update(b"|");
    hasher.update(override_blob.as_bytes());
    let hash = hex::encode(hasher.finalize())[..16].to_string();
    Ok(Catalogue { patterns: merged, hash })
}

pub fn merge_patterns(
    base: &[CataloguePattern],
    overrides: &[CataloguePattern],
) -> Vec<CataloguePattern> {
    if overrides.is_empty() {
        return base.to_vec();
    }
    let mut by_id: std::collections::HashMap<String, CataloguePattern> =
        base.iter().map(|p| (p.id.clone(), p.clone())).collect();
    let mut order: Vec<String> = base.iter().map(|p| p.id.clone()).collect();
    for o in overrides {
        if !by_id.contains_key(&o.id) {
            order.push(o.id.clone());
        }
        by_id.insert(o.id.clone(), o.clone());
    }
    order.into_iter().filter_map(|id| by_id.remove(&id)).collect()
}

pub fn patterns_for_language<'a>(
    catalogue: &'a Catalogue,
    language: &str,
) -> Vec<&'a CataloguePattern> {
    catalogue
        .patterns
        .iter()
        .filter(|p| p.languages.iter().any(|l| l == language))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_catalogue_loads_and_has_many_patterns() {
        let cat = load_catalogue(&[]).expect("default catalogue parses");
        assert!(
            cat.patterns.len() >= 90,
            "expected the embedded catalogue to ship 90+ patterns, got {}",
            cat.patterns.len()
        );
    }

    #[test]
    fn default_catalogue_hash_is_stable() {
        let a = load_catalogue(&[]).unwrap();
        let b = load_catalogue(&[]).unwrap();
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.hash.len(), 16);
    }

    #[test]
    fn override_changes_hash() {
        let a = load_catalogue(&[]).unwrap();
        let b = load_catalogue(&[CataloguePattern {
            id: "long-method".into(),
            name: "Long Method".into(),
            languages: vec!["typescript".into()],
            default_severity: DefaultSeverity::High,
            hint: "team override".into(),
        }])
        .unwrap();
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn patterns_for_language_filters_correctly() {
        let cat = load_catalogue(&[]).unwrap();
        let java = patterns_for_language(&cat, "java");
        let go = patterns_for_language(&cat, "go");
        assert!(!java.is_empty());
        assert!(!go.is_empty());
        // Sanity: every returned pattern's language list contains the requested language
        for p in &java {
            assert!(p.languages.iter().any(|l| l == "java"));
        }
    }

    #[test]
    fn merge_overrides_replace_by_id() {
        let base = vec![CataloguePattern {
            id: "a".into(),
            name: "A".into(),
            languages: vec!["typescript".into()],
            default_severity: DefaultSeverity::Low,
            hint: "base".into(),
        }];
        let overrides = vec![CataloguePattern {
            id: "a".into(),
            name: "A".into(),
            languages: vec!["typescript".into()],
            default_severity: DefaultSeverity::High,
            hint: "override".into(),
        }];
        let merged = merge_patterns(&base, &overrides);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].hint, "override");
        assert!(matches!(merged[0].default_severity, DefaultSeverity::High));
    }

    #[test]
    fn merge_appends_new_ids() {
        let base = vec![pattern("a"), pattern("b")];
        let overrides = vec![pattern("c")];
        let merged = merge_patterns(&base, &overrides);
        let ids: Vec<_> = merged.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    fn pattern(id: &str) -> CataloguePattern {
        CataloguePattern {
            id: id.into(),
            name: id.into(),
            languages: vec!["typescript".into()],
            default_severity: DefaultSeverity::Medium,
            hint: String::new(),
        }
    }
}

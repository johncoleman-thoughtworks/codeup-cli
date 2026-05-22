//! Generic schema migration runner — mirrors TS `migrations/runner.ts`.
//!
//! Every persisted YAML carries a `schemaVersion`. When we bump a schema,
//! register a Migration. On read, the runner walks
//! `from → from+1 → ... → currentVersion`. If the file is already at
//! current, the runner is a no-op.
//!
//! Migrations operate on `serde_yaml::Value` (the dynamic untyped form)
//! so they can rewrite arbitrary shapes without needing typed input or
//! output structs.

use serde_yaml::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("{artifact}: file schemaVersion {found} is newer than this build supports ({current})")]
    SchemaTooNew {
        artifact: String,
        found: u32,
        current: u32,
    },
    #[error("{artifact}: cannot migrate, value is not a YAML mapping")]
    NotAMapping { artifact: String },
    #[error("{artifact}: no migration registered from v{from} to v{to}")]
    MissingMigration {
        artifact: String,
        from: u32,
        to: u32,
    },
}

#[derive(Debug)]
pub struct Migration {
    pub from_version: u32,
    pub to_version: u32,
    pub migrate: fn(Value) -> Value,
}

#[derive(Debug)]
pub struct MigrationResult {
    pub value: Value,
    pub migrated: bool,
    pub applied_steps: Vec<u32>,
}

pub fn run_migrations(
    raw: Value,
    artifact_name: &str,
    current_version: u32,
    registry: &[Migration],
) -> Result<MigrationResult, MigrationError> {
    if !raw.is_mapping() {
        return Err(MigrationError::NotAMapping {
            artifact: artifact_name.to_string(),
        });
    }

    let found = raw
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(1);

    if found > current_version {
        return Err(MigrationError::SchemaTooNew {
            artifact: artifact_name.to_string(),
            found,
            current: current_version,
        });
    }

    let mut cur = raw;
    let mut applied_steps = Vec::new();
    for v in found..current_version {
        let step = registry.iter().find(|m| m.from_version == v).ok_or_else(|| {
            MigrationError::MissingMigration {
                artifact: artifact_name.to_string(),
                from: v,
                to: v + 1,
            }
        })?;
        cur = (step.migrate)(cur);
        if let Value::Mapping(ref mut m) = cur {
            m.insert(Value::String("schemaVersion".into()), Value::Number(step.to_version.into()));
        }
        applied_steps.push(step.to_version);
    }

    let migrated = !applied_steps.is_empty();
    Ok(MigrationResult {
        value: cur,
        migrated,
        applied_steps,
    })
}

// ─── Per-artifact registry constants. Empty today; populate when bumping. ───

pub const FINDING_CURRENT_VERSION: u32 = 1;
pub const FINDING_MIGRATIONS: &[Migration] = &[];

pub const DISMISSAL_CURRENT_VERSION: u32 = 1;
pub const DISMISSAL_MIGRATIONS: &[Migration] = &[];

pub const EXEMPLAR_CURRENT_VERSION: u32 = 1;
pub const EXEMPLAR_MIGRATIONS: &[Migration] = &[];

pub const CUSTOM_PATTERNS_CURRENT_VERSION: u32 = 1;
pub const CUSTOM_PATTERNS_MIGRATIONS: &[Migration] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Mapping;

    fn obj(schema_version: u32, extras: &[(&str, Value)]) -> Value {
        let mut m = Mapping::new();
        m.insert(Value::String("schemaVersion".into()), Value::Number(schema_version.into()));
        for (k, v) in extras {
            m.insert(Value::String((*k).into()), v.clone());
        }
        Value::Mapping(m)
    }

    fn bumped_field(prev: Value) -> Value {
        if let Value::Mapping(mut m) = prev {
            m.insert(Value::String("bumped".into()), Value::Bool(true));
            return Value::Mapping(m);
        }
        prev
    }

    #[test]
    fn no_op_at_current_version() {
        let r = run_migrations(obj(3, &[("x", Value::Number(1.into()))]), "thing", 3, &[]).unwrap();
        assert!(!r.migrated);
        assert!(r.applied_steps.is_empty());
        assert_eq!(r.value.get("x").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn runs_one_step_v1_to_v2() {
        let migs = vec![Migration {
            from_version: 1,
            to_version: 2,
            migrate: bumped_field,
        }];
        let r = run_migrations(
            obj(1, &[("legacy", Value::String("a".into()))]),
            "thing",
            2,
            &migs,
        )
        .unwrap();
        assert!(r.migrated);
        assert_eq!(r.applied_steps, vec![2]);
        assert_eq!(r.value.get("bumped").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(r.value.get("schemaVersion").and_then(|v| v.as_u64()), Some(2));
    }

    #[test]
    fn chains_v1_v2_v3() {
        let migs = vec![
            Migration {
                from_version: 1,
                to_version: 2,
                migrate: bumped_field,
            },
            Migration {
                from_version: 2,
                to_version: 3,
                migrate: |v| {
                    if let Value::Mapping(mut m) = v {
                        m.insert(Value::String("chained".into()), Value::Bool(true));
                        return Value::Mapping(m);
                    }
                    v
                },
            },
        ];
        let r = run_migrations(obj(1, &[]), "thing", 3, &migs).unwrap();
        assert_eq!(r.applied_steps, vec![2, 3]);
        assert_eq!(r.value.get("schemaVersion").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(r.value.get("bumped").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(r.value.get("chained").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn missing_step_errors() {
        let migs = vec![Migration {
            from_version: 2,
            to_version: 3,
            migrate: |v| v,
        }];
        let err = run_migrations(obj(1, &[]), "thing", 3, &migs).unwrap_err();
        match err {
            MigrationError::MissingMigration { from, to, .. } => {
                assert_eq!(from, 1);
                assert_eq!(to, 2);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn too_new_errors() {
        let err = run_migrations(obj(9, &[]), "thing", 3, &[]).unwrap_err();
        assert!(matches!(err, MigrationError::SchemaTooNew { found: 9, current: 3, .. }));
    }

    #[test]
    fn defaults_to_v1_when_field_absent() {
        let migs = vec![Migration {
            from_version: 1,
            to_version: 2,
            migrate: bumped_field,
        }];
        let mut m = Mapping::new();
        m.insert(Value::String("a".into()), Value::Number(1.into()));
        let r = run_migrations(Value::Mapping(m), "thing", 2, &migs).unwrap();
        assert_eq!(r.value.get("bumped").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn rejects_non_mapping() {
        let err = run_migrations(Value::Null, "thing", 1, &[]).unwrap_err();
        assert!(matches!(err, MigrationError::NotAMapping { .. }));
        let err = run_migrations(Value::String("hello".into()), "thing", 1, &[]).unwrap_err();
        assert!(matches!(err, MigrationError::NotAMapping { .. }));
    }
}

//! Codeup analysis core — pure, vscode-free, runtime-free.
//!
//! Modules:
//! - `schema` — Finding, severity/status/priority enums.
//! - `migrations` — generic version migration runner.
//! - `catalogue` — embedded default catalogue + per-language filter.
//! - `knowledge` — dismissals + exemplars + relevance retrieval.
//! - `intent` — layer rules + deterministic cycle / layer-violation findings.
//! - `scanner` — workspace walk, per-language imports, dependency graph + SCC.
//! - `quality` — deterministic oversized-file finding.
//! - `cache` — analysis cache key composition.

pub mod cache;
pub mod catalogue;
pub mod intent;
pub mod knowledge;
pub mod migrations;
pub mod quality;
pub mod scanner;
pub mod schema;

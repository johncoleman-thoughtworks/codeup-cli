//! Codeup analysis core — pure, sync, vscode-free.
//!
//! Modules:
//! - `schema` — Finding, knowledge, catalogue types (mirrors the TypeScript shapes).
//! - `scanner` — workspace walk + per-language import extraction.
//! - `graph` — dependency graph + Tarjan SCC cycle detection.
//! - `intent` — layer rules + violation detection.
//! - `knowledge` — dismissals + exemplars retrieval (in-memory).
//! - `catalogue` — pattern catalogue load + per-language filter.
//! - `migrations` — generic schema migration runner.
//! - `quality` — deterministic file-size check.
//!
//! All public APIs return owned values; callers (the CLI binary, the MCP
//! server, anyone embedding this crate) are responsible for I/O.

pub mod schema;

// Module skeletons — filled out in subsequent commits.
// pub mod scanner;
// pub mod graph;
// pub mod intent;
// pub mod knowledge;
// pub mod catalogue;
// pub mod migrations;
// pub mod quality;

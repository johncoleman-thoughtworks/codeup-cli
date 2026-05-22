//! Workspace scanner + dependency graph.

pub mod graph;
pub mod imports;
pub mod walk;

pub use walk::{scan_workspace, FileEntry, ProjectIndex};

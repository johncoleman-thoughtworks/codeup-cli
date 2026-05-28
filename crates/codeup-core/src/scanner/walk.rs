//! Workspace walker — mirrors TS `scanner/index.ts`.
//!
//! Uses the `ignore` crate (canonical Rust gitignore impl). Builds a
//! ProjectIndex with one FileEntry per source file: path, language,
//! size, sha256 content hash, mtime, raw imports.

use crate::scanner::imports::extract_imports;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{Match, WalkBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_FILE_BYTES: u64 = 512 * 1024;

const DEFAULT_EXCLUDES: &[&str] = &[
    // VCS / editor
    "!.git",
    "!.idea",
    "!.vscode-test",
    // Node
    "!node_modules",
    "!dist",
    "!out",
    // JVM / Gradle / Maven / Kotlin
    "!build",
    "!.gradle",
    "!.kotlin",
    "!target",
    "!.mvn",
    "!bin",
    "!*.class",
    "!*.jar",
    "!*.war",
    "!*.ear",
    // Go
    "!vendor",
    "!*.exe",
    "!*.test",
    // Python
    "!__pycache__",
    "!.venv",
    "!venv",
    "!.tox",
    "!.pytest_cache",
    "!.mypy_cache",
    "!.ruff_cache",
    "!*.egg-info",
    "!*.pyc",
    "!*.pyo",
    // .NET
    "!obj",
    "!packages",
    "!.vs",
    "!TestResults",
    "!*.dll",
    "!*.pdb",
    "!*.nupkg",
    "!*.suo",
    "!*.user",
    // Codeup itself
    "!.codeup",
    // Generated dependency lock files. Always committed, often huge,
    // never meaningfully analyzable as source — flagging them as
    // oversized just spams the report.
    "!Cargo.lock",
    "!package-lock.json",
    "!yarn.lock",
    "!pnpm-lock.yaml",
    "!npm-shrinkwrap.json",
    "!bun.lockb",
    "!Pipfile.lock",
    "!poetry.lock",
    "!uv.lock",
    "!Gemfile.lock",
    "!composer.lock",
    "!go.sum",
    "!mix.lock",
    "!Podfile.lock",
    "!packages.lock.json",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub language: String,
    pub size: u64,
    #[serde(rename = "contentHash")]
    pub content_hash: String,
    pub mtime: i64,
    #[serde(rename = "rawImports")]
    pub raw_imports: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIndex {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    #[serde(rename = "rootName")]
    pub root_name: String,
    pub files: Vec<FileEntry>,
}

pub fn language_for_ext(ext: &str) -> &'static str {
    match ext {
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "cs" => "csharp",
        "cpp" | "cc" | "cxx" | "hpp" | "h" => "cpp",
        "c" => "c",
        "php" => "php",
        "swift" => "swift",
        "md" => "markdown",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        "sh" | "bash" | "zsh" => "shell",
        "html" => "html",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        _ => "plaintext",
    }
}

/// Build the non-overridable defaults matcher (.git, node_modules, …).
fn build_defaults_ignore(root: &Path) -> std::io::Result<Gitignore> {
    let mut b = GitignoreBuilder::new(root);
    for pat in DEFAULT_EXCLUDES {
        let stripped = pat.strip_prefix('!').unwrap_or(pat);
        let _ = b.add_line(None, stripped);
    }
    b.build().map_err(std::io::Error::other)
}

/// Discover every `.gitignore` and `.codeupignore` under `root`, skipping
/// directories already excluded by `defaults`. Returns
/// `(codeupignore_matcher, gitignore_matcher)`.
fn build_user_ignores(
    root: &Path,
    defaults: &Gitignore,
) -> std::io::Result<(Gitignore, Gitignore)> {
    let mut git_b = GitignoreBuilder::new(root);
    let mut codeup_b = GitignoreBuilder::new(root);
    collect_ignore_files(root, root, defaults, &mut git_b, &mut codeup_b)?;
    let git_ig = git_b.build().map_err(std::io::Error::other)?;
    let codeup_ig = codeup_b.build().map_err(std::io::Error::other)?;
    Ok((codeup_ig, git_ig))
}

fn collect_ignore_files(
    root: &Path,
    dir: &Path,
    defaults: &Gitignore,
    git_b: &mut GitignoreBuilder,
    codeup_b: &mut GitignoreBuilder,
) -> std::io::Result<()> {
    // Process this directory's ignore files (shallow-first so deeper
    // files added later override shallower ones — gitignore semantics).
    let gi = dir.join(".gitignore");
    if gi.is_file() {
        if let Some(err) = git_b.add(&gi) {
            tracing::warn!("ignore: failed to load {gi:?}: {err}");
        }
    }
    let ci = dir.join(".codeupignore");
    if ci.is_file() {
        if let Some(err) = codeup_b.add(&ci) {
            tracing::warn!("ignore: failed to load {ci:?}: {err}");
        }
    }
    // Descend into subdirectories, skipping defaults-excluded paths.
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        // Don't follow symlinked directories during ignore-file discovery —
        // matches our scanner's safety stance and avoids cycle traversal.
        if file_type.is_symlink() {
            continue;
        }
        let rel = match child.strip_prefix(root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if defaults
            .matched_path_or_any_parents(rel, true)
            .is_ignore()
        {
            continue;
        }
        collect_ignore_files(root, &child, defaults, git_b, codeup_b)?;
    }
    Ok(())
}

/// Apply the three-tier precedence at a single path. `is_dir` toggles
/// gitignore's directory-only matching (`foo/`).
fn should_skip(
    rel: &Path,
    is_dir: bool,
    defaults: &Gitignore,
    codeup_ig: &Gitignore,
    git_ig: &Gitignore,
) -> bool {
    // Defaults are non-overridable.
    if defaults
        .matched_path_or_any_parents(rel, is_dir)
        .is_ignore()
    {
        return true;
    }
    // .codeupignore wins over .gitignore at any depth.
    match codeup_ig.matched_path_or_any_parents(rel, is_dir) {
        Match::Ignore(_) => return true,
        Match::Whitelist(_) => return false, // codeupignore un-ignored → keep
        Match::None => {}
    }
    git_ig
        .matched_path_or_any_parents(rel, is_dir)
        .is_ignore()
}

/// Walk the workspace at `root`, applying:
///
/// 1. **Defaults** (non-overridable): `.git`, `node_modules`, `.codeup`,
///    lock files, … — same set as the VS Code extension.
/// 2. **`.codeupignore`** rules at any depth — override `.gitignore` at
///    any depth. A `!keep.snap` in `.codeupignore` brings the file back
///    even when `.gitignore` ignores it.
/// 3. **`.gitignore`** rules — applied only when `.codeupignore` is
///    neutral on the path.
///
/// Returns a ProjectIndex with one entry per source file.
pub fn scan_workspace(root: &Path, generated_at: String) -> std::io::Result<ProjectIndex> {
    // Build the three matchers up front.
    let defaults = build_defaults_ignore(root)?;
    let (codeup_ig, git_ig) = build_user_ignores(root, &defaults)?;

    // Walk without the ignore crate's built-in gitignore handling — we
    // apply our own precedence (defaults → codeupignore → gitignore).
    // Prune ignored directories at filter_entry time so we don't descend
    // into node_modules / .git / etc.
    let root_for_filter = root.to_path_buf();
    let defaults_for_filter = defaults.clone();
    let codeup_for_filter = codeup_ig.clone();
    let git_for_filter = git_ig.clone();
    let walker = WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .parents(false)
        .filter_entry(move |dent| {
            if dent.depth() == 0 {
                return true; // root itself
            }
            let Some(file_type) = dent.file_type() else { return true };
            let rel = match dent.path().strip_prefix(&root_for_filter) {
                Ok(p) => p,
                Err(_) => return true,
            };
            !should_skip(
                rel,
                file_type.is_dir(),
                &defaults_for_filter,
                &codeup_for_filter,
                &git_for_filter,
            )
        })
        .build();

    let mut files = Vec::new();
    for result in walker {
        let dent = match result {
            Ok(d) => d,
            Err(_) => continue,
        };
        if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let rel_path = match dent.path().strip_prefix(root) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if should_skip(rel_path, false, &defaults, &codeup_ig, &git_ig) {
            continue;
        }
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");
        let metadata = match dent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let bytes = match std::fs::read(dent.path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let ext = Path::new(&rel_str)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let language = language_for_ext(&ext).to_string();

        let raw_imports = match std::str::from_utf8(&bytes) {
            Ok(text) => extract_imports(&language, text),
            Err(_) => Vec::new(),
        };

        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let content_hash = hex::encode(hasher.finalize());

        files.push(FileEntry {
            path: rel_str,
            language,
            size: metadata.len(),
            content_hash,
            mtime,
            raw_imports,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let root_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();

    Ok(ProjectIndex {
        schema_version: 1,
        generated_at,
        root_name,
        files,
    })
}

pub fn hash_content(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn tmpdir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("codeup-walk-test-{}", uuid_like()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{n}")
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn detects_languages_by_ext() {
        assert_eq!(language_for_ext("ts"), "typescript");
        assert_eq!(language_for_ext("java"), "java");
        assert_eq!(language_for_ext("py"), "python");
        assert_eq!(language_for_ext("go"), "go");
        assert_eq!(language_for_ext("rs"), "rust");
        assert_eq!(language_for_ext("xyz"), "plaintext");
    }

    #[test]
    fn walks_a_small_fixture_and_excludes_node_modules() {
        let root = tmpdir();
        write(&root.join("src/main.ts"), "export const x = 1;\n");
        write(&root.join("src/util.ts"), "export function f() {}\n");
        write(&root.join("node_modules/foo/index.js"), "// should be ignored\n");
        write(&root.join(".codeup/findings/x.yaml"), "# should be ignored\n");

        let idx = scan_workspace(&root, "now".into()).unwrap();
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/main.ts"));
        assert!(paths.contains(&"src/util.ts"));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        assert!(!paths.iter().any(|p| p.contains(".codeup")));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn computes_content_hash_consistently() {
        assert_eq!(hash_content(b"hello"), hash_content(b"hello"));
        assert_ne!(hash_content(b"hello"), hash_content(b"world"));
    }

    #[test]
    fn codeupignore_excludes_files_gitignore_would_have_included() {
        let root = tmpdir();
        write(&root.join("src/keep.ts"), "// keep\n");
        write(&root.join("src/skip.ts"), "// skip\n");
        write(&root.join(".codeupignore"), "src/skip.ts\n");
        let idx = scan_workspace(&root, "now".into()).unwrap();
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/keep.ts"));
        assert!(!paths.contains(&"src/skip.ts"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn codeupignore_un_ignores_a_file_gitignore_ignores() {
        let root = tmpdir();
        write(&root.join("src/keep.ts"), "// keep\n");
        write(&root.join("src/generated.ts"), "// generated\n");
        write(&root.join(".gitignore"), "src/generated.ts\n");
        write(&root.join(".codeupignore"), "!src/generated.ts\n");
        let idx = scan_workspace(&root, "now".into()).unwrap();
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/keep.ts"));
        assert!(paths.contains(&"src/generated.ts"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn shallow_codeupignore_overrides_deep_gitignore() {
        let root = tmpdir();
        write(&root.join("pkg/foo.ts"), "// foo\n");
        write(&root.join("pkg/.gitignore"), "foo.ts\n");
        write(&root.join(".codeupignore"), "!**/foo.ts\n");
        let idx = scan_workspace(&root, "now".into()).unwrap();
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"pkg/foo.ts"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn deep_codeupignore_overrides_shallow_gitignore_negation() {
        let root = tmpdir();
        write(&root.join("pkg/foo.ts"), "// foo\n");
        write(&root.join(".gitignore"), "!pkg/foo.ts\n");
        write(&root.join("pkg/.codeupignore"), "foo.ts\n");
        let idx = scan_workspace(&root, "now".into()).unwrap();
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        assert!(!paths.contains(&"pkg/foo.ts"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gitignore_decides_when_codeupignore_is_neutral() {
        let root = tmpdir();
        write(&root.join("src/foo.ts"), "// foo\n");
        write(&root.join("src/skip.ts"), "// skip\n");
        write(&root.join(".gitignore"), "src/skip.ts\n");
        // No .codeupignore.
        let idx = scan_workspace(&root, "now".into()).unwrap();
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/foo.ts"));
        assert!(!paths.contains(&"src/skip.ts"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn defaults_cannot_be_un_ignored_via_codeupignore() {
        let root = tmpdir();
        write(&root.join(".codeup/findings/x.yaml"), "# tracked\n");
        write(&root.join(".codeupignore"), "!.codeup/**\n");
        let idx = scan_workspace(&root, "now".into()).unwrap();
        let paths: Vec<&str> = idx.files.iter().map(|f| f.path.as_str()).collect();
        // .codeup/ contents are non-overridable; .codeupignore at root is
        // just a regular file (it carries the substring ".codeup" but
        // doesn't live under .codeup/).
        assert!(!paths.iter().any(|p| p.starts_with(".codeup/")));
        fs::remove_dir_all(&root).ok();
    }
}

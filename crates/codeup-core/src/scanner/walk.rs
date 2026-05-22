//! Workspace walker — mirrors TS `scanner/index.ts`.
//!
//! Uses the `ignore` crate (canonical Rust gitignore impl). Builds a
//! ProjectIndex with one FileEntry per source file: path, language,
//! size, sha256 content hash, mtime, raw imports.

use crate::scanner::imports::extract_imports;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
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

/// Walk the workspace at `root`, applying .gitignore + Codeup's default
/// excludes. Returns a ProjectIndex with one entry per source file.
pub fn scan_workspace(root: &Path, generated_at: String) -> std::io::Result<ProjectIndex> {
    let mut overrides_builder = OverrideBuilder::new(root);
    for pat in DEFAULT_EXCLUDES {
        // Patterns starting with "!" in OverrideBuilder are *includes*; we
        // want *excludes*, which are the unprefixed form.
        let stripped = pat.strip_prefix('!').unwrap_or(pat);
        if let Err(e) = overrides_builder.add(stripped) {
            tracing::warn!("invalid exclude pattern {stripped:?}: {e}");
        }
    }
    // OverrideBuilder semantics: by default everything is allowed, and
    // adding patterns *whitelists* them. To flip to blacklist mode we
    // build an Override that excludes — done via the "ignore" crate's
    // Walk wrapping below using `add_custom_ignore_filename`.
    //
    // Simpler approach: rely on Walk's built-in gitignore handling and
    // attach our default excludes via add_ignore.

    let walker = WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(false) // .gitignore-respected + dotfiles like .codeup/ filtered via add_ignore
        .add_custom_ignore_filename(".codeupignore")
        .build();

    // Build an additional matcher for DEFAULT_EXCLUDES.
    let mut default_ignore = ignore::gitignore::GitignoreBuilder::new(root);
    for pat in DEFAULT_EXCLUDES {
        let stripped = pat.strip_prefix('!').unwrap_or(pat);
        let _ = default_ignore.add_line(None, stripped);
    }
    let default_ignore = default_ignore.build().map_err(std::io::Error::other)?;

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
        // Apply our default-excludes filter — ancestor-aware so that
        // e.g. `node_modules/foo/index.js` is matched by the `node_modules`
        // pattern via its ancestor segment.
        if default_ignore
            .matched_path_or_any_parents(rel_path, false)
            .is_ignore()
        {
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
}

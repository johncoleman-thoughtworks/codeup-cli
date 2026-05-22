//! Dependency graph + Tarjan SCC — mirrors TS `scanner/graph.ts`.

use crate::intent::Cycle;
use crate::scanner::walk::{FileEntry, ProjectIndex};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path as StdPath;

#[derive(Debug, Clone, Default)]
pub struct DependencyGraph {
    /// adjacency: from → set of to (workspace-relative paths)
    pub edges: BTreeMap<String, BTreeSet<String>>,
    /// reverse adjacency
    pub reverse: BTreeMap<String, BTreeSet<String>>,
    /// raw imports that couldn't be resolved — for diagnostics
    pub unresolved: BTreeMap<String, Vec<String>>,
}

pub fn build_graph(index: &ProjectIndex) -> DependencyGraph {
    let by_path: HashMap<&str, &FileEntry> =
        index.files.iter().map(|f| (f.path.as_str(), f)).collect();

    let mut g = DependencyGraph::default();

    for f in &index.files {
        let mut resolved = BTreeSet::new();
        let mut still_unresolved = Vec::new();
        for raw in &f.raw_imports {
            match resolve_import(f, raw, &by_path) {
                Some(target) if target != f.path => {
                    resolved.insert(target);
                }
                Some(_) => {}
                None => still_unresolved.push(raw.clone()),
            }
        }
        if !resolved.is_empty() {
            for t in &resolved {
                g.reverse.entry(t.clone()).or_default().insert(f.path.clone());
            }
            g.edges.insert(f.path.clone(), resolved);
        }
        if !still_unresolved.is_empty() {
            g.unresolved.insert(f.path.clone(), still_unresolved);
        }
    }

    g
}

fn resolve_import(
    from: &FileEntry,
    raw: &str,
    by_path: &HashMap<&str, &FileEntry>,
) -> Option<String> {
    match from.language.as_str() {
        "java" | "kotlin" | "scala" => resolve_jvm(raw, by_path, &from.language),
        "typescript" | "typescriptreact" | "javascript" | "javascriptreact" => {
            resolve_js(&from.path, raw, by_path)
        }
        "python" => resolve_python(raw, by_path),
        "go" => resolve_go(raw, by_path),
        "csharp" => None, // namespace-based; no deterministic file
        _ => None,
    }
}

fn resolve_jvm(
    raw: &str,
    by_path: &HashMap<&str, &FileEntry>,
    lang: &str,
) -> Option<String> {
    if raw.ends_with(".*") {
        return None;
    }
    let dotted = raw.replace('.', "/");
    let exts: &[&str] = match lang {
        "kotlin" => &[".kt"],
        "scala" => &[".scala"],
        _ => &[".java"],
    };
    for candidate in by_path.keys() {
        for ext in exts {
            if candidate.ends_with(&format!("/{dotted}{ext}")) || candidate == &&*format!("{dotted}{ext}") {
                return Some((*candidate).to_string());
            }
        }
    }
    None
}

fn resolve_js(
    from_path: &str,
    raw: &str,
    by_path: &HashMap<&str, &FileEntry>,
) -> Option<String> {
    if !raw.starts_with('.') {
        return None;
    }
    let base_dir = StdPath::new(from_path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let joined_path = base_dir.join(raw);
    let joined = normalize_posix(&joined_path.to_string_lossy());
    let exts = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
    for e in &exts {
        let candidate = format!("{joined}{e}");
        if by_path.contains_key(candidate.as_str()) {
            return Some(candidate);
        }
    }
    for e in &exts {
        let candidate = format!("{joined}/index{e}");
        if by_path.contains_key(candidate.as_str()) {
            return Some(candidate);
        }
    }
    if by_path.contains_key(joined.as_str()) {
        return Some(joined);
    }
    None
}

fn resolve_python(raw: &str, by_path: &HashMap<&str, &FileEntry>) -> Option<String> {
    if raw.starts_with('.') {
        return None;
    }
    let dotted = raw.replace('.', "/");
    for candidate in by_path.keys() {
        if candidate.ends_with(&format!("/{dotted}.py")) || candidate == &&*format!("{dotted}.py") {
            return Some((*candidate).to_string());
        }
        if candidate.ends_with(&format!("/{dotted}/__init__.py"))
            || candidate == &&*format!("{dotted}/__init__.py")
        {
            return Some((*candidate).to_string());
        }
    }
    None
}

fn resolve_go(raw: &str, by_path: &HashMap<&str, &FileEntry>) -> Option<String> {
    let parts: Vec<&str> = raw.split('/').collect();
    if parts.is_empty() {
        return None;
    }
    let tail = parts.iter().rev().take(2).rev().cloned().collect::<Vec<_>>().join("/");
    for candidate in by_path.keys() {
        if !candidate.ends_with(".go") {
            continue;
        }
        let dir = StdPath::new(candidate)
            .parent()
            .map(|p| normalize_posix(&p.to_string_lossy()))
            .unwrap_or_default();
        if dir.ends_with(&format!("/{tail}")) || dir == tail {
            return Some((*candidate).to_string());
        }
    }
    None
}

fn normalize_posix(s: &str) -> String {
    let mut out = s.replace('\\', "/");
    // Collapse "a/b/../c" → "a/c", remove "./" runs.
    let mut stack: Vec<&str> = Vec::new();
    for seg in out.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let abs = s.starts_with('/');
    out = stack.join("/");
    if abs {
        out.insert(0, '/');
    }
    out
}

/// Tarjan's SCC — every component of size > 1 is a cycle. Self-loops
/// (size 1 with self-edge) are filtered out to match the TS behaviour.
pub fn find_cycles(g: &DependencyGraph) -> Vec<Cycle> {
    let mut nodes: HashSet<&str> = HashSet::new();
    for k in g.edges.keys() {
        nodes.insert(k);
    }
    for k in g.reverse.keys() {
        nodes.insert(k);
    }
    let nodes: Vec<String> = nodes.into_iter().map(String::from).collect();

    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut lowlink: HashMap<String, usize> = HashMap::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let mut counter: usize = 0;
    let mut cycles: Vec<Cycle> = Vec::new();

    #[allow(clippy::too_many_arguments)]
    fn strongconnect(
        v: &str,
        g: &DependencyGraph,
        idx: &mut HashMap<String, usize>,
        lowlink: &mut HashMap<String, usize>,
        on_stack: &mut HashSet<String>,
        stack: &mut Vec<String>,
        counter: &mut usize,
        cycles: &mut Vec<Cycle>,
    ) {
        idx.insert(v.to_string(), *counter);
        lowlink.insert(v.to_string(), *counter);
        *counter += 1;
        stack.push(v.to_string());
        on_stack.insert(v.to_string());

        let empty = BTreeSet::new();
        let succ = g.edges.get(v).unwrap_or(&empty).clone();
        for w in &succ {
            if !idx.contains_key(w) {
                strongconnect(w, g, idx, lowlink, on_stack, stack, counter, cycles);
                let new_low = std::cmp::min(lowlink[v], lowlink[w]);
                lowlink.insert(v.to_string(), new_low);
            } else if on_stack.contains(w) {
                let new_low = std::cmp::min(lowlink[v], idx[w]);
                lowlink.insert(v.to_string(), new_low);
            }
        }

        if lowlink[v] == idx[v] {
            let mut component: Vec<String> = Vec::new();
            loop {
                let w = stack.pop().expect("stack empty during SCC pop");
                on_stack.remove(&w);
                component.push(w.clone());
                if w == v {
                    break;
                }
            }
            if component.len() > 1 {
                component.reverse();
                cycles.push(Cycle { files: component });
            } else if succ.contains(v) {
                cycles.push(Cycle { files: component });
            }
        }
    }

    for v in &nodes {
        if !idx.contains_key(v) {
            strongconnect(v, g, &mut idx, &mut lowlink, &mut on_stack, &mut stack, &mut counter, &mut cycles);
        }
    }
    cycles
}

pub fn neighbors_of<'a>(g: &'a DependencyGraph, file: &str) -> (Vec<&'a str>, Vec<&'a str>) {
    let imports: Vec<&str> = g
        .edges
        .get(file)
        .map(|s| s.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let imported_by: Vec<&str> = g
        .reverse
        .get(file)
        .map(|s| s.iter().map(String::as_str).collect())
        .unwrap_or_default();
    (imports, imported_by)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::walk::FileEntry;

    fn entry(path: &str, language: &str, raw_imports: &[&str]) -> FileEntry {
        FileEntry {
            path: path.into(),
            language: language.into(),
            size: 0,
            content_hash: format!("h_{path}"),
            mtime: 0,
            raw_imports: raw_imports.iter().map(|s| (*s).into()).collect(),
        }
    }

    fn index(files: Vec<FileEntry>) -> ProjectIndex {
        ProjectIndex {
            schema_version: 1,
            generated_at: String::new(),
            root_name: "r".into(),
            files,
        }
    }

    #[test]
    fn java_imports_resolve_to_files() {
        let idx = index(vec![
            entry("src/main/java/com/example/A.java", "java", &["com.example.B"]),
            entry("src/main/java/com/example/B.java", "java", &[]),
            entry("src/main/java/com/example/C.java", "java", &["com.example.unknown"]),
        ]);
        let g = build_graph(&idx);
        let from_a = g.edges.get("src/main/java/com/example/A.java").unwrap();
        assert_eq!(from_a.iter().next().unwrap(), "src/main/java/com/example/B.java");
        assert!(!g.edges.contains_key("src/main/java/com/example/C.java"));
        assert_eq!(
            g.unresolved.get("src/main/java/com/example/C.java").unwrap(),
            &vec!["com.example.unknown".to_string()]
        );
    }

    #[test]
    fn ts_relative_imports_resolve() {
        let idx = index(vec![
            entry("src/a.ts", "typescript", &["./b"]),
            entry("src/b.ts", "typescript", &[]),
            entry("src/nested/c.ts", "typescript", &["../a"]),
        ]);
        let g = build_graph(&idx);
        assert_eq!(g.edges.get("src/a.ts").unwrap().iter().next().unwrap(), "src/b.ts");
        assert_eq!(g.edges.get("src/nested/c.ts").unwrap().iter().next().unwrap(), "src/a.ts");
    }

    #[test]
    fn detects_a_to_b_to_a_cycle() {
        let idx = index(vec![
            entry("src/a.ts", "typescript", &["./b"]),
            entry("src/b.ts", "typescript", &["./a"]),
        ]);
        let cycles = find_cycles(&build_graph(&idx));
        assert_eq!(cycles.len(), 1);
        let mut files = cycles[0].files.clone();
        files.sort();
        assert_eq!(files, vec!["src/a.ts".to_string(), "src/b.ts".to_string()]);
    }

    #[test]
    fn self_loops_filtered_when_resolver_strips_them() {
        let idx = index(vec![entry("src/self.ts", "typescript", &["./self"])]);
        let g = build_graph(&idx);
        assert!(!g.edges.contains_key("src/self.ts"));
        assert!(find_cycles(&g).is_empty());
    }

    #[test]
    fn dag_has_no_cycles() {
        let idx = index(vec![
            entry("src/a.ts", "typescript", &["./b", "./c"]),
            entry("src/b.ts", "typescript", &["./c"]),
            entry("src/c.ts", "typescript", &[]),
        ]);
        assert!(find_cycles(&build_graph(&idx)).is_empty());
    }

    #[test]
    fn detects_two_disjoint_cycles() {
        let idx = index(vec![
            entry("src/a.ts", "typescript", &["./b"]),
            entry("src/b.ts", "typescript", &["./a"]),
            entry("src/x.ts", "typescript", &["./y"]),
            entry("src/y.ts", "typescript", &["./x"]),
        ]);
        assert_eq!(find_cycles(&build_graph(&idx)).len(), 2);
    }
}

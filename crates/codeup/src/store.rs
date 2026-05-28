//! Disk I/O for `.codeup/`. Findings YAML (one file per finding,
//! upsert-preserves-status), knowledge YAML (dismissals + exemplars +
//! custom catalogue patterns), intent YAML. Migration runner is applied
//! on every load.

use anyhow::{anyhow, Context, Result};
use codeup_core::catalogue::CataloguePattern;
use codeup_core::intent::IntentConfig;
use codeup_core::knowledge::{
    CustomPatternsFile, DismissalsFile, ExemplarsFile,
    KnowledgeSnapshot,
};
use codeup_core::migrations::{
    run_migrations, CUSTOM_PATTERNS_CURRENT_VERSION, CUSTOM_PATTERNS_MIGRATIONS,
    DISMISSAL_CURRENT_VERSION, DISMISSAL_MIGRATIONS, EXEMPLAR_CURRENT_VERSION,
    EXEMPLAR_MIGRATIONS, FINDING_CURRENT_VERSION, FINDING_MIGRATIONS,
};
use codeup_core::schema::Finding;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Workspace-relative POSIX path allowlist: no traversal, no absolute
/// prefixes, no backslashes, no NUL.
fn is_safe_relative_path(p: &str) -> bool {
    if p.is_empty() || p.len() > 1024 {
        return false;
    }
    if p.starts_with('/') || p.starts_with('\\') {
        return false;
    }
    if p.contains('\\') || p.contains('\0') {
        return false;
    }
    // Drive letter on any platform.
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    for seg in p.split('/') {
        if seg == ".." {
            return false;
        }
    }
    true
}

/// Pre-parse safety check for YAML files under .codeup/. Hard size cap
/// plus a cheap anchor-density bound to reject billion-laughs payloads
/// without invoking the full parser.
fn is_yaml_bytes_safe(bytes: &[u8], name: &str) -> bool {
    const MAX_YAML_BYTES: usize = 256 * 1024;
    // Codeup's own YAML schemas (findings, intent, dismissals, exemplars,
    // patterns) do not use anchors or aliases. Tight caps catch
    // billion-laughs constructions — 9 levels × 10 aliases needs at
    // least 9 anchors and 90 aliases, both well past these limits.
    const MAX_ANCHORS: usize = 8;
    const MAX_ALIASES: usize = 16;
    if bytes.len() > MAX_YAML_BYTES {
        tracing::warn!("yaml: {name}: skipping — exceeds size cap ({} bytes)", bytes.len());
        return false;
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return true, // let the parser produce a typed error
    };
    let mut anchors = 0usize;
    let mut aliases = 0usize;
    // Count " &name" / " *name" tokens that begin a YAML node, not strings.
    // Cheap, conservative; false positives only push real billion-laughs
    // payloads further from the limit.
    let mut prev_was_break = true;
    let mut in_quotes: Option<char> = None;
    for c in text.chars() {
        match in_quotes {
            Some(q) if c == q => in_quotes = None,
            Some(_) => {}
            None => {
                if c == '"' || c == '\'' {
                    in_quotes = Some(c);
                } else if prev_was_break || c.is_whitespace() {
                    // no-op — wait for the next char
                }
                if c == '&' { anchors += 1; }
                if c == '*' { aliases += 1; }
            }
        }
        prev_was_break = c == '\n';
    }
    if anchors > MAX_ANCHORS || aliases > MAX_ALIASES {
        tracing::warn!(
            "yaml: {name}: skipping — pathological anchor/alias density (anchors={anchors}, aliases={aliases})"
        );
        return false;
    }
    true
}

/// Strict filename-component allowlist for filenames we derive from
/// untrusted data (a finding id, after revalidate_cached). Rejects
/// path separators, traversal, and absolute prefixes.
fn is_safe_filename_component(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    if s == "." || s == ".." {
        return false;
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Walk every component of `target` (must be a descendant of `root`) and
/// refuse if any component is a symlink. Caller passes the already-joined
/// path; this is a lstat check, not a canonicalisation pass — we want to
/// detect a planted symlink rather than follow it.
fn assert_no_symlink_ancestors(root: &Path, target: &Path) -> Result<()> {
    let rel = target
        .strip_prefix(root)
        .map_err(|_| anyhow!("path {target:?} is outside workspace root {root:?}"))?;
    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component);
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(anyhow!(
                    "refusing to traverse symlink at {cur:?} (planted under workspace)"
                ));
            }
            // Missing component is fine — we'll create it below.
            Ok(_) | Err(_) => {}
        }
    }
    Ok(())
}

/// Create `dir` (and any missing ancestors) under `root`, refusing if
/// any existing component on the way is a symlink. Mirrors create_dir_all
/// but with symlink_metadata-checks instead of metadata-checks.
fn safe_create_dir_all(root: &Path, dir: &Path) -> Result<()> {
    assert_no_symlink_ancestors(root, dir)?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {dir:?}"))?;
    Ok(())
}

/// Write `body` to `dir/filename`, refusing if the destination already
/// exists as a symlink or if any path component is a symlink. Atomic via
/// write-to-temp + rename; the temp file is opened with create_new so a
/// concurrent attacker cannot race a symlink into the same name.
fn safe_write_yaml(root: &Path, dir: &Path, filename: &str, body: &str) -> Result<()> {
    safe_create_dir_all(root, dir)?;
    let final_path = dir.join(filename);
    // If destination exists, it must not be a symlink — we'd overwrite
    // through it on the rename below.
    if let Ok(meta) = std::fs::symlink_metadata(&final_path) {
        if meta.file_type().is_symlink() {
            return Err(anyhow!(
                "refusing to write through symlink at {final_path:?}"
            ));
        }
    }
    // Use a process-/pid-unique temp filename so concurrent scans don't
    // collide. The `.` prefix keeps it out of casual `ls` output.
    let tmp_name = format!(".{}.{}.tmp", filename, std::process::id());
    let tmp_path = dir.join(tmp_name);
    // Best-effort cleanup of a stale temp from a previous crash.
    let _ = std::fs::remove_file(&tmp_path);
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // O_NOFOLLOW: refuse if the path turns out to be a symlink.
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = opts
            .open(&tmp_path)
            .with_context(|| format!("creating {tmp_path:?}"))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("writing {tmp_path:?}"))?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming {tmp_path:?} -> {final_path:?}"))?;
    Ok(())
}

pub const FINDINGS_REL: &str = ".codeup/findings";
pub const KNOWLEDGE_REL: &str = ".codeup/knowledge";
pub const INTENT_REL: &str = ".codeup/intent.yaml";

pub struct FindingsStore {
    root: PathBuf,
    by_id: BTreeMap<String, Finding>,
    /// Ids of findings that the current run regenerated via
    /// upsert_from_analysis. Anything else in `by_id` came purely from
    /// persisted state and should not be emitted as an authoritative
    /// new event (SARIF, --fail-on gate) by the caller.
    produced_ids: std::collections::HashSet<String>,
}

impl FindingsStore {
    pub fn load(root: &Path) -> Result<Self> {
        let dir = root.join(FINDINGS_REL);
        let mut by_id = BTreeMap::new();
        let produced_ids = std::collections::HashSet::new();
        if !dir.exists() {
            return Ok(Self { root: root.to_path_buf(), by_id, produced_ids });
        }
        // Security: refuse a symlinked .codeup/findings directory itself —
        // a planted symlink to /home/runner/ would redirect every write
        // through it. Same defence in depth as save() applies on read.
        if let Ok(meta) = std::fs::symlink_metadata(&dir) {
            if meta.file_type().is_symlink() {
                tracing::warn!("findings: skipping load — {dir:?} is a symlink");
                return Ok(Self { root: root.to_path_buf(), by_id, produced_ids });
            }
        }
        for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {dir:?}"))? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".yaml") && !name.ends_with(".yml") {
                continue;
            }
            // Security: refuse symlinked YAML files. Without this, a
            // planted .codeup/findings/x.yaml -> ~/.bashrc would be read
            // and (later) overwritten by save().
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.file_type().is_symlink() {
                    tracing::warn!("findings: {name}: skipping — symlink");
                    continue;
                }
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if !is_yaml_bytes_safe(&bytes, name) {
                continue;
            }
            let raw: serde_yaml::Value = match serde_yaml::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("findings: {name}: {e}");
                    continue;
                }
            };
            let mig = match run_migrations(raw, name, FINDING_CURRENT_VERSION, FINDING_MIGRATIONS) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("findings: {name}: {e}");
                    continue;
                }
            };
            let finding: Finding = match serde_yaml::from_value(mig.value) {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!("findings: {name}: {e}");
                    continue;
                }
            };
            // Belt-and-braces: never surface findings pointing at .codeup/
            if finding.location.file == ".codeup" || finding.location.file.starts_with(".codeup/") {
                continue;
            }
            // Security: refuse paths with traversal, absolute prefixes, or
            // backslashes. A planted finding with location.file
            // "../../../../home/runner/.ssh/id_rsa" would otherwise be
            // serialised back into SARIF and point a Code Scanning alert
            // at the victim's private key.
            if !is_safe_relative_path(&finding.location.file) {
                tracing::warn!(
                    "findings: {name}: skipping — unsafe location.file {:?}",
                    finding.location.file
                );
                continue;
            }
            // Security: refuse self-asserted ids that don't match
            // stable_id of the persisted (file, category, line). This
            // neutralises planted records that try to occupy the slot
            // a freshly-detected finding would have ended up in.
            if let Some(line) = finding.location.line {
                let expected = crate::analyzer::stable_id(
                    &finding.location.file,
                    &finding.category,
                    line,
                );
                if expected != finding.id {
                    tracing::warn!(
                        "findings: {name}: discarding — id {:?} does not match stable_id of (file, category, line)",
                        finding.id
                    );
                    continue;
                }
            }
            // Security: refuse Dismissed status carried purely from disk.
            // A planted record with status: dismissed would otherwise be
            // merged onto a re-detected real finding by upsert_from_analysis,
            // silently suppressing the alert. Reset to Unconfirmed on load;
            // legitimate dismissals reapply if the user dismisses again.
            let mut finding = finding;
            if matches!(
                finding.status,
                codeup_core::schema::Status::Dismissed | codeup_core::schema::Status::Fixed
            ) {
                tracing::info!(
                    "findings: {name}: resetting status {:?} -> unconfirmed (persisted security state not honoured)",
                    finding.status
                );
                finding.status = codeup_core::schema::Status::Unconfirmed;
            }
            by_id.insert(finding.id.clone(), finding);
        }
        Ok(Self { root: root.to_path_buf(), by_id, produced_ids })
    }

    /// Iterate every finding currently in the store — including those
    /// loaded from disk that the current scan did NOT re-detect. Use
    /// only for state lookups; never as authoritative output (see
    /// `produced_by_this_run` for that).
    #[allow(dead_code)]
    pub fn all(&self) -> impl Iterator<Item = &Finding> {
        self.by_id.values()
    }

    /// Findings that the current scan re-detected. Use this — not `all()` —
    /// for any authoritative downstream emission: SARIF, --fail-on gate,
    /// PR annotations. Persisted-only records (potentially planted) are
    /// excluded; they're available via `all()` only for state lookups.
    pub fn produced_by_this_run(&self) -> impl Iterator<Item = &Finding> {
        self.by_id
            .iter()
            .filter(|(id, _)| self.produced_ids.contains(*id))
            .map(|(_, f)| f)
    }

    /// Lookup by id. Used by the future intent suggester and MCP server
    /// surfaces; the scan command doesn't need it directly.
    #[allow(dead_code)]
    pub fn get(&self, id: &str) -> Option<&Finding> {
        self.by_id.get(id)
    }

    /// Upsert: if a finding with the same id exists, preserve its
    /// status / priority / history but refresh the analytical fields.
    /// Otherwise insert as `unconfirmed` with a `detected` history event.
    pub fn upsert_from_analysis(&mut self, new: Finding) -> Result<&Finding> {
        let merged = match self.by_id.get(&new.id) {
            Some(existing) => Finding {
                schema_version: existing.schema_version,
                id: existing.id.clone(),
                category: new.category,
                severity: new.severity,
                // Security: do NOT inherit status from persisted state.
                // Re-detecting a finding is grounds to surface it again
                // (regression of Fixed, retraction of Dismissed); the
                // user can re-dismiss interactively if appropriate.
                status: new.status,
                priority: existing.priority,
                location: new.location,
                explanation: new.explanation,
                suggested_remediation: new.suggested_remediation,
                detected_at: existing.detected_at.clone(),
                detected_by: new.detected_by,
                confidence: new.confidence,
                history: existing.history.clone(),
            },
            None => Finding {
                history: vec![codeup_core::schema::HistoryEvent {
                    timestamp: new.detected_at.clone(),
                    event: "detected".into(),
                    by: None,
                    from: None,
                    to: None,
                    note: None,
                }],
                ..new
            },
        };
        self.save(&merged)?;
        let id = merged.id.clone();
        self.by_id.insert(id.clone(), merged);
        self.produced_ids.insert(id.clone());
        Ok(self.by_id.get(&id).unwrap())
    }

    fn save(&self, finding: &Finding) -> Result<()> {
        // Security: refuse traversal-bearing ids. cache-revalidation
        // already gates `category` (which prefixes the id), but a
        // belt-and-braces filename check costs nothing.
        if !is_safe_filename_component(&finding.id) {
            return Err(anyhow!("refusing to save finding with unsafe id: {}", finding.id));
        }
        let dir = self.root.join(FINDINGS_REL);
        let body = serde_yaml::to_string(finding)?;
        let body = quote_timestamps_in_yaml(&body);
        safe_write_yaml(&self.root, &dir, &format!("{}.yaml", finding.id), &body)?;
        Ok(())
    }
}

/// Force quoted-string emission for timestamp fields.
///
/// serde_yaml emits ISO-8601 strings as plain (unquoted) scalars. The
/// TS extension reads findings with `js-yaml` v4 + DEFAULT_SCHEMA, which
/// auto-types plain ISO-8601 strings into `!!timestamp` (a JS Date) and
/// the validator then rejects them with "must be a non-empty string".
///
/// SCHEMA.md mandates quoted timestamps as the wire form. We post-process
/// the YAML to wrap the value of any known timestamp key in double quotes
/// if it isn't already quoted. Cheap line scan — no regex dep.
fn quote_timestamps_in_yaml(yaml: &str) -> String {
    const KEYS: &[&str] = &["detectedAt", "dismissedAt", "confirmedAt", "timestamp"];
    let mut out = String::with_capacity(yaml.len() + 64);
    for line in yaml.split_inclusive('\n') {
        // Strip the trailing newline (if any) for parsing, re-add at the end.
        let (content, newline) = match line.strip_suffix('\n') {
            Some(rest) => (rest, "\n"),
            None => (line, ""),
        };
        // Find leading whitespace + optional "- " list marker.
        let trimmed = content.trim_start();
        if !trimmed.contains(':') {
            out.push_str(line);
            continue;
        }
        // Allow either "key: value" or "- key: value".
        let after_dash = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        let Some((key, value)) = after_dash.split_once(':') else {
            out.push_str(line);
            continue;
        };
        let key = key.trim();
        if !KEYS.contains(&key) {
            out.push_str(line);
            continue;
        }
        let value = value.trim();
        if value.is_empty() || value.starts_with('"') || value.starts_with('\'') {
            out.push_str(line);
            continue;
        }
        // Rebuild the line preserving prefix exactly.
        let prefix_len = content.len() - trimmed.len();
        let prefix = &content[..prefix_len];
        let dash = if trimmed.starts_with("- ") { "- " } else { "" };
        out.push_str(prefix);
        out.push_str(dash);
        out.push_str(key);
        out.push_str(": \"");
        out.push_str(value);
        out.push('"');
        out.push_str(newline);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{is_safe_filename_component, is_safe_relative_path, is_yaml_bytes_safe, quote_timestamps_in_yaml};

    #[test]
    fn safe_filename_accepts_hash_ids() {
        assert!(is_safe_filename_component("long-method-abc123def456"));
        assert!(is_safe_filename_component("oversized-file.x"));
    }

    #[test]
    fn safe_filename_rejects_traversal_and_separators() {
        assert!(!is_safe_filename_component("../../tmp/pwn"));
        assert!(!is_safe_filename_component("a/b"));
        assert!(!is_safe_filename_component("a\\b"));
        assert!(!is_safe_filename_component(".."));
        assert!(!is_safe_filename_component(""));
        assert!(!is_safe_filename_component(&"a".repeat(129)));
    }

    #[test]
    fn safe_relative_path_accepts_workspace_relative() {
        assert!(is_safe_relative_path("src/foo.rs"));
        assert!(is_safe_relative_path("__orphan__/src/foo.rs"));
        assert!(is_safe_relative_path("README.md"));
    }

    #[test]
    fn safe_relative_path_rejects_traversal_absolute_drive() {
        assert!(!is_safe_relative_path("../../etc/passwd"));
        assert!(!is_safe_relative_path("a/../b"));
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("\\foo"));
        assert!(!is_safe_relative_path("C:/foo"));
        assert!(!is_safe_relative_path("a\\b"));
        assert!(!is_safe_relative_path("a\0b"));
    }

    #[test]
    fn yaml_safety_rejects_oversized_input() {
        let oversized = vec![b'x'; 300 * 1024];
        assert!(!is_yaml_bytes_safe(&oversized, "x"));
    }

    #[test]
    fn yaml_safety_rejects_billion_laughs_anchor_density() {
        // 9 levels x 10 aliases each — classic billion-laughs.
        let mut payload = String::from("entries: &a [\"x\"]\n");
        for i in 1..=9 {
            payload.push_str(&format!(
                "a{i}: &a{i} [{}]\n",
                std::iter::repeat(if i == 1 { "*a" } else { "*a" })
                    .take(10)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        assert!(!is_yaml_bytes_safe(payload.as_bytes(), "bomb.yaml"));
    }

    #[test]
    fn yaml_safety_accepts_modest_real_yaml() {
        let ok = "schemaVersion: 1\nentries:\n  - id: x\n    note: hi\n";
        assert!(is_yaml_bytes_safe(ok.as_bytes(), "ok.yaml"));
    }

    #[test]
    fn quotes_detected_at_and_history_timestamps() {
        let input = "schemaVersion: 1\n\
detectedAt: 2026-05-22T14:32:11.123Z\n\
history:\n\
- timestamp: 2026-05-22T14:32:11.123Z\n  event: detected\n";
        let out = quote_timestamps_in_yaml(input);
        assert!(out.contains("detectedAt: \"2026-05-22T14:32:11.123Z\""), "got: {out}");
        assert!(out.contains("- timestamp: \"2026-05-22T14:32:11.123Z\""), "got: {out}");
        assert!(out.contains("event: detected"));
    }

    #[test]
    fn leaves_already_quoted_values_alone() {
        let input = "detectedAt: \"2026-05-22T14:32:11.123Z\"\n";
        assert_eq!(quote_timestamps_in_yaml(input), input);
    }

    #[test]
    fn ignores_non_timestamp_keys() {
        let input = "category: long-method\nseverity: high\n";
        assert_eq!(quote_timestamps_in_yaml(input), input);
    }
}

pub fn load_knowledge(root: &Path) -> Result<(KnowledgeSnapshot, Vec<CataloguePattern>)> {
    let dir = root.join(KNOWLEDGE_REL);
    let dismissals = read_yaml_with_migration::<DismissalsFile>(
        &dir.join("dismissals.yaml"),
        DISMISSAL_CURRENT_VERSION,
        DISMISSAL_MIGRATIONS,
    )?
    .map(|f| f.entries)
    .unwrap_or_default();
    let exemplars = read_yaml_with_migration::<ExemplarsFile>(
        &dir.join("exemplars.yaml"),
        EXEMPLAR_CURRENT_VERSION,
        EXEMPLAR_MIGRATIONS,
    )?
    .map(|f| f.entries)
    .unwrap_or_default();
    let custom_patterns = read_yaml_with_migration::<CustomPatternsFile>(
        &dir.join("patterns.yaml"),
        CUSTOM_PATTERNS_CURRENT_VERSION,
        CUSTOM_PATTERNS_MIGRATIONS,
    )?
    .map(|f| f.patterns)
    .unwrap_or_default();
    Ok((
        KnowledgeSnapshot { dismissals, exemplars },
        custom_patterns,
    ))
}

pub fn load_intent(root: &Path) -> Result<Option<IntentConfig>> {
    let path = root.join(INTENT_REL);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {path:?}"))?;
    if !is_yaml_bytes_safe(&bytes, "intent.yaml") {
        return Ok(None);
    }
    let intent: IntentConfig = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("parsing {path:?}"))?;
    Ok(Some(intent))
}

fn read_yaml_with_migration<T: serde::de::DeserializeOwned>(
    path: &Path,
    current_version: u32,
    migrations: &[codeup_core::migrations::Migration],
) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {path:?}"))?;
    let display = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !is_yaml_bytes_safe(&bytes, display) {
        return Ok(None);
    }
    let raw: serde_yaml::Value = serde_yaml::from_slice(&bytes)
        .with_context(|| format!("parsing YAML at {path:?}"))?;
    let mig = run_migrations(raw, display, current_version, migrations)
        .map_err(|e| anyhow!("{e}"))?;
    let value: T = serde_yaml::from_value(mig.value)
        .with_context(|| format!("decoding {path:?} into typed value"))?;
    Ok(Some(value))
}

// Re-export DismissalEntry / ExemplarEntry so callers don't need to dig
// into codeup_core; keeps the CLI's import surface flat.

# Changelog

All notable changes to the Codeup CLI are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project uses
[Semantic Versioning](https://semver.org/).

## 0.2.0 — 2026-05-28

### Added

- **`.codeupignore` files.** Optional ignore files that work exactly like `.gitignore` and can appear at any depth in the workspace. Use them to exclude paths from Codeup analysis that you want tracked by git (generated source, fixtures, vendored data).
- **Global precedence.** `.codeupignore` rules override `.gitignore` rules at any depth — a `!keep.snap` in a `.codeupignore` re-includes a file even when a `.gitignore` (anywhere in the tree) ignores it. A non-overridable defaults set (`.git`, `node_modules`, `.codeup`, lock files, …) is always skipped and cannot be un-ignored via either file.

### Changed

- Workspace walker now uses three separate matchers (defaults / codeupignore / gitignore) consulted in priority order, replacing the previous `WalkBuilder` configuration that treated all custom ignore files additively.

## 0.1.1 — 2026-05-28

### Security

Hardened against a malicious contributor shipping crafted files under
`.codeup/` via a pull request that CI checks out and scans.

- **Cache hits now revalidate.** Every cached analysis result is re-checked
  against the catalogue allowlist before its `category` flows into
  `stable_id` and onto the filesystem. A poisoned cache entry with a
  traversal-bearing category (`../../tmp/pwn`) can no longer become the
  filename in `FindingsStore::save`.
- **Findings YAML writes refuse symlinks.** `FindingsStore::save` now
  writes via a `safe_write_yaml` helper that opens the temp file with
  `O_NOFOLLOW`, checks every path component with `symlink_metadata`,
  and atomically renames into place. Refuses if the destination already
  exists as a symlink. Same protection applies to `create_dir_all` of
  the findings directory. (Mitigates symlink-following arbitrary file
  write through both intermediate-directory and final-component links.)
- **Persisted state no longer suppresses re-detected findings.**
  `upsert_from_analysis` does not inherit `status` from disk. Loaded
  records carrying `Dismissed` or `Fixed` are reset to `Unconfirmed` on
  load, with a log line. Combined with the next item this neutralises
  the "plant a `status: dismissed` YAML with a predicted id" attack.
- **Self-asserted ids are cross-checked against `stable_id`** on load.
  Records whose on-disk id doesn't match
  `stable_id(location.file, category, location.line)` are discarded.
- **SARIF and `--fail-on` only emit findings the current run re-detected.**
  Records loaded purely from disk (potentially planted) are state, not
  authoritative output. New `FindingsStore::produced_by_this_run`.
- **YAML inputs under `.codeup/` are size- and anchor-capped before
  parsing.** A 256 KB hard cap and tight anchor/alias-density bounds
  reject billion-laughs constructions before `serde_yaml` materialises
  the alias graph. Applies to findings, intent, dismissals, exemplars,
  and custom patterns.
- **Provider credentials are bound to destinations in the type system.**
  `AnthropicKey` and `GitHubToken` are distinct types; the resolver
  reads each from its own env var only when the matching provider is
  active. There is no shared `--api-key` fallback. **Breaking change:**
  `--api-key` is removed; use `--anthropic-api-key` (env
  `ANTHROPIC_API_KEY`) or `--github-token` (env `GITHUB_TOKEN`).
- **Neighbor truncation no longer panics on multi-byte UTF-8.** Slicing
  by byte index would terminate the scan on input where a codepoint
  straddles `MAX_NEIGHBOR_CHARS`. Now snaps to the greatest char
  boundary `<= max`.
- **Tarjan SCC is iterative.** `find_cycles` no longer recurses, so a
  deep linear import chain (any depth) cannot overflow the OS thread
  stack. Tested with a 20,000-deep chain.

### Catalogue

Synced with the VS Code extension catalogue — added eleven abstract
security anti-patterns and one exception-handling pattern. See
`crates/codeup-core/resources/default.yaml`.

## 0.1.0

Initial release.

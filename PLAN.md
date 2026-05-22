# codeup-cli — Port Plan

Goal: a Rust workspace producing a single static `codeup` binary that runs the same analysis as the TypeScript VS Code extension, suitable for GitHub Actions and other CI runners.

The reference implementation lives at [johncoleman-thoughtworks/codeup-vscx](https://github.com/johncoleman-thoughtworks/codeup-vscx). Each phase below ports a slice and tests it against the same fixtures.

## Phase 1 — Pure analysis core (4 days estimate)

Port the vscode-free TypeScript modules to `codeup-core`. Each maps roughly 1:1.

- [x] `schema.rs` — Finding, location, history, severity/status/priority enums; serde round-trip tests.
- [ ] `migrations.rs` — generic version migration runner (TS: `migrations/runner.ts`).
- [ ] `catalogue.rs` — catalogue loader + per-language filter (TS: `catalogue/loader.ts`). Ships the same `default.yaml` from this repo.
- [ ] `knowledge.rs` — schema + retrieval (glob match, directory proximity). Mirrors TS `knowledge/{schema,retrieve}.ts`.
- [ ] `intent.rs` — layer rules + matching (TS: `intent/layers.ts`).
- [ ] `scanner/walk.rs` — workspace walk via `ignore::Walk`, language detection.
- [ ] `scanner/imports.rs` — per-language regex import extraction (TS: `scanner/imports.ts`).
- [ ] `scanner/graph.rs` — dependency graph + Tarjan SCC (TS: `scanner/graph.ts`).
- [ ] `quality/size_check.rs` — oversized-file finding (TS: `quality/sizeCheck.ts`).
- [ ] `cache.rs` — per-entry analysis cache (TS: `analyzer/cache.ts`).

Port the existing unit tests alongside each module. Aim for parity with the TS test suite (currently ~96 tests).

## Phase 2 — HTTP + LLM orchestration (3 days estimate)

In the `codeup` binary:

- [ ] `llm/anthropic.rs` — Anthropic Messages API client. No official Rust SDK; handwritten request/response types via `serde` against the public schema.
- [ ] `llm/github_models.rs` — GitHub Models endpoint. Same Claude wire format, different base URL + auth header.
- [ ] `llm/provider.rs` — `LLMClient` trait + selection (`--provider anthropic|github-models` plus auto-detect).
- [ ] `analyzer.rs` — neighbor gathering + tool-use loop + cache integration (TS: `analyzer/analyze.ts`).
- [ ] `runner.rs` — orchestrates the deterministic checks + LLM pass + finding persistence.
- [ ] `intent_suggest.rs` — propose_layer_rules tool flow.

## Phase 3 — Reporters & flags (2 days estimate)

- [ ] `reporters/text.rs` — terminal-friendly summary.
- [ ] `reporters/json.rs` — structured dump.
- [ ] `reporters/markdown.rs` — PR-comment-shaped markdown.
- [ ] `reporters/sarif.rs` — SARIF 2.1.0, schema-validated against the official JSON schema.
- [ ] `--diff <ref>` mode using `git diff --name-only`.
- [ ] `--max-cost` enforcement.
- [ ] `--fail-on <severity>` exit-code logic.
- [ ] `--persist` flag to write findings YAML.

## Phase 4 — Distribution (2 days estimate)

- [ ] GitHub Actions release workflow: cross-compile to `darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `windows-x64`. Attach to GitHub release.
- [ ] Install script: `curl -fsSL https://.../install.sh | sh` picks the right binary.
- [ ] `examples/.github/workflows/codeup-daily.yml` — daily scheduled scan reference recipe.
- [ ] `examples/.github/workflows/codeup-pr-deterministic.yml` — free per-PR safety net.
- [ ] README rewrite with installation + recipes.

## Phase 5 — VS Code extension delegation (separate repo, separate work)

The VS Code extension at `codeup-vscx` will be refactored to invoke the Rust binary for scans, watching `.codeup/findings/` for changes. Tracked separately in that repo. Out of scope for this CLI.

## Cross-cutting

- **Testing strategy**: unit tests per module, plus a `tests/` integration suite that runs the binary against fixture workspaces and asserts output. Snapshot-test SARIF output against the official schema.
- **CI**: `cargo build --release`, `cargo test`, `cargo clippy -- -D warnings`, `cargo audit`, `cargo deny` (license + dep policy).
- **Versioning**: starts at 0.1.0; reach 1.0.0 when phase 1-3 are feature-complete vs the TS extension.

## Open questions

1. Tool-use through GitHub Models for Claude — has anyone verified Codeup's `report_finding` schema round-trips correctly through the proxy? Spike before depending on it.
2. Cache invalidation across binary versions — bump the cache key when the binary version changes? Or keep keyed only on (contentHash, catalogueHash, model, neighborsKey, knowledgeKey) as today?
3. .codeup directory format compatibility — when this CLI writes findings YAML, the existing VS Code extension must be able to read them and vice versa. Need a conformance test that round-trips fixtures both ways.

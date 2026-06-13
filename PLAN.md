# codeup-cli — Port Plan

Goal: a Rust workspace producing a single static `codeup` binary that runs the same analysis as the TypeScript VS Code extension, suitable for GitHub Actions and other CI runners.

The reference implementation lives at [johncoleman-thoughtworks/codeup-vscx](https://github.com/johncoleman-thoughtworks/codeup-vscx). Each phase below ports a slice and tests it against the same fixtures.

## Status (updated 2026-06-13)

**Phases 1–4 are substantially complete: the `codeup` binary builds, scans, and is released (v0.2.1).** Remaining gaps are individually flagged `[ ]` below. A separate **MCP capability** (`codeup mcp`) has since been built on top of this core — tracked in **[PLAN-MCP.md](PLAN-MCP.md)**.

Done:
- ✅ Whole analysis core (Phase 1) — every module ported and unit-tested (61 core tests).
- ✅ LLM orchestration (Phase 2) — Anthropic + GitHub Models providers, analyzer, runner.
- ✅ Reporters + key flags (Phase 3) — text / JSON / SARIF, `--fail-on`, `--deterministic-only`.
- ✅ Distribution (Phase 4) — multi-target release workflow + curl installer + README.

Not done (carried):
- ❌ `intent suggest` subcommand (Phase 2) — still stubbed (`bail!("not implemented yet")`).
- ❌ `--diff <ref>` mode, markdown reporter, `--max-cost` enforcement, `--persist` flag (Phase 3).
- ❌ Windows release target; `examples/` recipe files (Phase 4).
- ⏳ The three cross-cutting open questions below remain open.

## Phase 1 — Pure analysis core ✅ complete

Ported the vscode-free TypeScript modules to `codeup-core`.

- [x] `schema.rs` — Finding, location, history, severity/status/priority enums; serde round-trip tests.
- [x] `migrations.rs` — generic version migration runner (TS: `migrations/runner.ts`).
- [x] `catalogue.rs` — catalogue loader + per-language filter (TS: `catalogue/loader.ts`). Ships the same `default.yaml`.
- [x] `knowledge.rs` — schema + retrieval (glob match, directory proximity). Mirrors TS `knowledge/{schema,retrieve}.ts`.
- [x] `intent.rs` — layer rules + matching, plus deterministic cycle / layer-violation findings (TS: `intent/layers.ts`).
- [x] `scanner/walk.rs` — workspace walk via `ignore::Walk`, language detection.
- [x] `scanner/imports.rs` — per-language regex import extraction (TS: `scanner/imports.ts`).
- [x] `scanner/graph.rs` — dependency graph + iterative Tarjan SCC (TS: `scanner/graph.ts`).
- [x] `quality.rs` — oversized-file finding (TS: `quality/sizeCheck.ts`). (Named `quality.rs`, not `quality/size_check.rs`.)
- [x] `cache.rs` — per-entry analysis cache key (TS: `analyzer/cache.ts`).

## Phase 2 — HTTP + LLM orchestration ◐ mostly complete

In the `codeup` binary:

- [x] `llm/anthropic.rs` — Anthropic Messages API client; handwritten request/response types, with 429/5xx retry.
- [x] `llm/github_models.rs` — GitHub Models endpoint (same Claude wire format, different base URL + auth).
- [x] `llm/provider.rs` — provider selection (`--provider anthropic|github-models` + auto-detect). *(Enum dispatch rather than a `dyn` trait — there are only ever two providers.)*
- [x] `analyzer.rs` — neighbour gathering + tool-use loop + cache integration (TS: `analyzer/analyze.ts`). *(Per-file orchestration since lifted into `review::review_workspace`, shared with the MCP server — see PLAN-MCP.md.)*
- [x] `runner.rs` — orchestrates deterministic checks + LLM pass + finding persistence.
- [ ] `intent_suggest.rs` — `propose_layer_rules` tool flow. **Not done**: `intent suggest` still bails with "not implemented yet".

## Phase 3 — Reporters & flags ◐ partial

- [x] text reporter — terminal-friendly summary (`render_text` in `main.rs`; not a separate `reporters/` module).
- [x] json reporter — structured dump (`--out json`, serde in `main.rs`).
- [ ] markdown reporter — PR-comment-shaped markdown. **Not done.**
- [x] `sarif.rs` — SARIF 2.1.0 (`--out sarif`).
- [ ] `--diff <ref>` mode using `git diff --name-only`. **Not done** (no flag).
- [ ] `--max-cost` enforcement. **Partial**: the flag is parsed (default 5.0) but no enforcement/prompt is wired — it's a documented ceiling only.
- [x] `--fail-on <severity>` exit-code logic (default `high`).
- [ ] `--persist` flag to write findings YAML. **Not done as a flag**: findings are always persisted (`RunOptions.persist` is hardcoded `true`); the `--no-persist` branch in `runner.rs` is inert.

## Phase 4 — Distribution ◐ mostly complete

- [x] GitHub Actions release workflow — cross-compiles `linux x86_64`, `linux aarch64`, `macos x86_64`, `macos aarch64`; attaches to the GitHub release. **Windows target not yet added.**
- [x] Install script: `scripts/install.sh` — `curl -fsSL … | sh` picks the right binary (linux + macos).
- [ ] `examples/.github/workflows/codeup-daily.yml` + `codeup-pr-deterministic.yml`. **Not done as files** — the README's GitHub Actions section carries an inline recipe instead.
- [x] README — installation + recipes (and an MCP usage section).

## Phase 5 — VS Code extension delegation (separate repo) — out of scope

The VS Code extension at `codeup-vscx` will be refactored to invoke the Rust binary. Tracked separately in that repo; out of scope for this CLI.

## Addendum — MCP server (`codeup mcp`) ✅ P0–P2 built, P3 pending

Built after the original phases: a local MCP stdio server exposing the analyzer to MCP hosts with no provider key, with the catalogue review running on the host's model via MCP sampling. **Full status (what's done / not done, including the sampling-support findings) lives in [PLAN-MCP.md](PLAN-MCP.md).** Summary: P0 (shared review module) + P1 (keyless tools) + P2 (sampling review) implemented and verified; P3 (skill capability-ladder, `mcp install` helper, registry submission, sampling cache) not done.

## Cross-cutting

- **Testing strategy**: unit tests per module (100 across the workspace), plus the MCP server smoke-tested end-to-end. A dedicated `tests/` integration suite running the binary against fixture workspaces is **not yet** built.
- **CI**: `cargo build --release`, `cargo test`, `cargo clippy` run; `cargo audit` / `cargo deny` **not yet** wired.
- **Versioning**: at 0.2.1. Reach 1.0.0 when phases 1–3 are feature-complete vs the TS extension (the Phase 2/3 gaps above are what stand between here and that).

## Open questions

1. **Tool-use through GitHub Models for Claude** — has anyone verified Codeup's `report_finding` schema round-trips correctly through the proxy? **Still unverified.** Spike before depending on it.
2. **Cache invalidation across binary versions** — the cache key is still `(contentHash, catalogueHash, model, neighborsKey, knowledgeKey)` with no binary-version component. **Open**: decide whether to bump on version change.
3. **.codeup directory format compatibility** — round-trip conformance test between this CLI and the VS Code extension. **Open**: no dedicated conformance test exists yet (the shared SCHEMA.md + quoted-timestamp handling is the current safeguard).

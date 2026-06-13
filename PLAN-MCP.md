# `codeup mcp` — design plan

Expose codeup as a local **MCP server** (`codeup mcp`, stdio) so MCP hosts —
**GitHub Copilot / VS Code first**, also Claude Desktop, Cursor, Claude Code —
can run codeup's analysis with **no LLM provider key**. The LLM catalogue
judgment runs on the **host's own model via MCP sampling** (consuming the host's
tokens / the user's existing Copilot or Claude subscription); the deterministic
engine runs locally and keyless. Single-sourced from `codeup-core`, so findings
stay byte-identical with the CLI, the VS Code extension, and the skill.

## Status (updated 2026-06-13)

**Built and verified (branch `mcp`, not yet merged):**

- ✅ **P0** shared review module · ✅ **P1** keyless tools · ✅ **P2** sampling review
  + host-delegation prompt. 100 tests green; clippy clean on lib+bin.
- ✅ Six tools + `codeup` prompt over a hand-rolled stdio JSON-RPC server.
- ✅ Bidirectional sampling proven against a **simulated** host (Python harness).
- ✅ DDD pass: single review orchestrator behind a `FileReviewer` port, one
  `Finding` constructor, bounded-context `root` fence.
- ✅ Installed (`cargo install`) and **registered with Claude Code** (`claude mcp
  add`, user scope) — `claude mcp list` reports it connected.

**Not done / unverified:**

- ❌ **P3 (DX):** skill capability-ladder integration, `codeup mcp install` helper,
  registry submission, per-file sampling cache.
- ⚠️ **Sampling against a real host:** `codeup_review` on host tokens is **not yet
  validated end-to-end on a released build.** Empirically, **Claude Code (v2.1.177,
  protocol 2025-11-25) advertises `roots` + `elicitation` but NOT `sampling`** — so
  in Claude Code `codeup_review` correctly falls back to deterministic-only +
  delegation. **VS Code / Copilot is the host to validate** the real host-tokens
  path (Open question #1).
- ❌ Rust import resolution (affects `codeup_deterministic_scan` cycles/layers on
  Rust repos — the resolver covers JVM/TS/JS/Python/Go/C#, not Rust). Pre-existing
  CLI limitation, not MCP-specific, but relevant when reviewing this repo's crates.

## Hard requirements (from the brief)

1. **Usable from Copilot** (VS Code agent mode) and other MCP hosts.
2. **Consume the host's tokens, not a provider API key** — no `ANTHROPIC_API_KEY`
   / `GITHUB_TOKEN` required for the LLM pass.
3. Local, $0 to run, no network listener.
4. No catalogue/schema drift — reuse `codeup-core`.

Requirement 2 is the design driver: an MCP server borrows the host's model only
via **`sampling/createMessage`**. So sampling is the spine of the LLM path, not
an afterthought.

## Execution modes (by client capability)

The server negotiates `sampling` capability at `initialize` and picks a mode:

| Host | `sampling`? | LLM patterns run via | Key needed |
|---|---|---|---|
| **VS Code / Copilot** | yes¹ | **MCP sampling → host model (Copilot tokens)** | none |
| **Claude Desktop** | yes¹ | MCP sampling → host model | none |
| **Cursor** | yes¹ | MCP sampling | none |
| **Claude Code** | no¹ (today) | host-delegation prompt *or* shell-out *or* opt-in key | none (delegation) |

¹ Sampling support is the load-bearing assumption — **must be re-verified per
client/version before build** (see Open questions). VS Code/Copilot is the
primary target and is reported to support MCP sampling.

**Deterministic tools never need sampling or a key** — they work in every host.

## Tool surface

Naming: tools are exposed as `mcp__codeup__<tool>` to hosts. All paths are
workspace-relative and validated (no traversal; writes confined to `.codeup/`).

### Keyless (no model, every host)

**`codeup_deterministic_scan`** — cycles + layer violations + oversized files.
```json
{
  "name": "codeup_deterministic_scan",
  "description": "Run codeup's deterministic checks (import cycles via Tarjan SCC, layer violations against .codeup/intent.yaml, oversized files). No model, no key. Persists findings to .codeup/findings/ and returns a summary.",
  "input_schema": {
    "type": "object",
    "properties": {
      "root": { "type": "string", "description": "Workspace root. Default: server cwd." },
      "persist": { "type": "boolean", "default": true, "description": "Write findings to .codeup/findings/." }
    }
  }
}
```

**`codeup_catalogue`** — the patterns (so a host/agent can reason against them).
```json
{
  "name": "codeup_catalogue",
  "description": "Return the codeup pattern catalogue (id, name, languages, defaultSeverity, hint). Optionally filtered to one language.",
  "input_schema": {
    "type": "object",
    "properties": { "language": { "type": "string", "description": "e.g. java, typescript, python. Omit for all." } }
  }
}
```

**`codeup_graph_neighbors`** — real import neighbours (sharpens any LLM pass).
```json
{
  "name": "codeup_graph_neighbors",
  "description": "Top-N dependency-graph neighbours of a file (imports, importedBy, samePackage) for cross-file context.",
  "input_schema": {
    "type": "object",
    "required": ["file"],
    "properties": {
      "file": { "type": "string" },
      "n": { "type": "integer", "default": 6, "minimum": 0, "maximum": 20 }
    }
  }
}
```

**`codeup_list_findings`** / **`codeup_save_findings`** — read/merge `.codeup/`.
```json
{
  "name": "codeup_save_findings",
  "description": "Validate findings against the catalogue allowlist and write/merge them into .codeup/findings/ per the shared schema (stable ids, quoted timestamps). Use after a host agent has reasoned over code (host-delegation mode).",
  "input_schema": {
    "type": "object",
    "required": ["findings"],
    "properties": {
      "findings": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["category", "file", "line", "explanation"],
          "properties": {
            "category": { "type": "string", "description": "MUST be a catalogue pattern id." },
            "file": { "type": "string" },
            "line": { "type": "integer" },
            "endLine": { "type": "integer" },
            "severity": { "type": "string", "enum": ["low", "medium", "high"] },
            "priority": { "type": "string", "enum": ["ignore", "low", "medium", "high"] },
            "explanation": { "type": "string" },
            "suggestedRemediation": { "type": "string" },
            "confidence": { "type": "number" }
          }
        }
      }
    }
  }
}
```

### Host-tokens LLM pass (sampling)

**`codeup_review`** — the full catalogue review using the host's model.
```json
{
  "name": "codeup_review",
  "description": "Review files against the 107-pattern catalogue using the HOST's model (via MCP sampling — no API key). Runs deterministic checks too, persists all findings to .codeup/. Falls back to host-delegation when the client lacks sampling.",
  "input_schema": {
    "type": "object",
    "properties": {
      "paths": { "type": "array", "items": { "type": "string" }, "description": "Files/globs to review. Default: changed files, else whole root." },
      "root": { "type": "string" },
      "deterministic_only": { "type": "boolean", "default": false }
    }
  }
}
```

### Prompt (host-delegation fallback / agent hosts)

MCP **prompt** `codeup` (surfaces as a slash command, e.g. `/mcp__codeup__codeup`):
returns the system prompt + the language-relevant catalogue + the `.codeup/`
schema + target files, instructing the host's *own* agent loop to reason and
then call `codeup_save_findings`. This is the no-sampling path (e.g. Claude
Code) and needs no key either.

## How `codeup_review` consumes host tokens (the sampling flow)

Per eligible file, the server:
1. Builds the codeup **system prompt** + **user prompt** (file + graph neighbours)
   — *lifted from the existing `analyzer.rs` builders* (see Refactor).
2. Issues `sampling/createMessage` to the host:
   ```jsonc
   {
     "systemPrompt": "<codeup reviewer system prompt>",
     "messages": [{ "role": "user", "content": { "type": "text", "text": "<file + catalogue + 'return findings as JSON'>" } }],
     "modelPreferences": { "intelligencePriority": 0.8 },
     "maxTokens": 2048
   }
   ```
   The **host** runs this on its model and bills its own tokens/subscription.
3. **Parse structured output from the completion.** MCP sampling returns a text
   message, **not guaranteed tool-calls** — so the prompt asks for a fenced JSON
   array of findings and the server parses it (same approach the skill eval used
   when tool-use wasn't available). Do *not* rely on forced `report_finding`
   tool-use over sampling.
4. **Validate** each finding's `category` against the catalogue allowlist
   (reuse existing validation), compute the **stable id**
   (`<category>-<sha256("file:category:line")[..12]>`), and **persist** via the
   store — identical `.codeup/` output to the CLI.
5. **Cache**: keep the per-file content-hash cache so unchanged files don't
   re-sample — this directly saves the user's host tokens.

Result: zero provider key, host pays for inference, byte-compatible findings.

## How the skill detects and prefers it

`SKILL.md` gains a capability ladder (highest-fidelity available wins):

1. **MCP tools present** (`mcp__codeup__*` connected): call
   `codeup_deterministic_scan` for cycles/layers/oversized; optionally
   `codeup_graph_neighbors` to ground cross-file findings. (In Claude Code the
   skill still does the *LLM judgment* on the host model itself — so it uses MCP
   only for the deterministic/graph parts it can't do well.)
2. **`codeup` binary on PATH** (no MCP): shell out to
   `codeup scan <root> --deterministic-only` (keyless; persists to `.codeup/`).
3. **Neither**: do the LLM catalogue pass only, and state in the summary that
   cycle/layer detection was skipped (don't fabricate graph results).

In **Copilot/VS Code**, where there is no agentic "skill" reading files, the MCP
server's `codeup_review` (sampling) does the whole job end-to-end.

## Build / form factor

- **Add an `mcp` subcommand to the existing `codeup` binary** (`codeup mcp` →
  stdio MCP server) rather than a new crate. One artifact, one install, reuses
  all of `codeup-core`. Implement with an `rmcp`-style stdio server.
- **Refactor (P0):** lift `build_system_prompt`, `build_user_prompt`, the
  catalogue-validation, and the stable-id/save path out of
  `crates/codeup/src/analyzer.rs` into a shared module both the CLI analyzer and
  the MCP server use. (The code already anticipates this — see the `NeighborFile`
  "MCP server will want it" note.)
- **No new LLM client code** for the host-tokens path — sampling replaces the
  provider call. The existing Anthropic/GitHub providers stay as an *opt-in*
  fallback for non-sampling hosts that do have a key.

## Install / registration ($0, keyless)

Ship `codeup mcp` in the same release artifacts. Register per host:

- **VS Code / Copilot** — `.vscode/mcp.json` (or user settings):
  ```json
  { "servers": { "codeup": { "command": "codeup", "args": ["mcp"] } } }
  ```
- **Claude Code** — `claude mcp add codeup -- codeup mcp`
- **Claude Desktop** — the equivalent block in its MCP config.

Optionally a `codeup mcp install --client vscode|claude-code|claude-desktop`
helper that writes the right config.

## Security

- **stdio only**, child of a trusted host — no inbound surface, no network.
- **No credentials**: the host-tokens path holds no API key (the cross-vendor
  key-leakage hazard `provider.rs` guards against simply doesn't arise here).
- **Sampling consent**: hosts show their own approval UI for `sampling/*`; we
  inherit it. Source content goes only to the *same host* the user already
  trusts with their code — no new exfil path.
- **Write scope fenced**: `codeup_save_findings` / persistence write only under
  `.codeup/`, validate every `category` against the catalogue allowlist, reuse
  `store.rs` path-traversal guards.
- **Static tool/prompt text** shipped in the signed binary — no runtime-fetched
  descriptions; untrusted repo content goes only into prompt *arguments*.

## Phasing

- **P0 — refactor:** ✅ *implemented (branch `mcp`).* Analyzer prompt builders,
  catalogue validation, and the stable-id/save path are reused by the MCP server
  (`analyzer` fns made `pub(crate)`; new `review` module for sampling
  JSON-in-text parsing). CLI tests stay green (61 core + 39 CLI).
- **P1 — keyless MCP MVP:** ✅ *implemented.* `codeup mcp` (hand-rolled
  newline-delimited JSON-RPC stdio server, no SDK dep) with
  `codeup_deterministic_scan`, `codeup_catalogue`, `codeup_graph_neighbors`,
  `codeup_list_findings`, `codeup_save_findings`. Smoke-tested end-to-end.
- **P2 — host-tokens review:** ✅ *implemented.* `codeup_review` via server-
  initiated `sampling/createMessage` (JSON-parse + catalogue-validate + persist,
  attributes `detectedBy` to the host model), and the `codeup` MCP prompt for
  delegation fallback. Bidirectional sampling verified with a simulated host;
  still **to validate against a real Copilot/VS Code build** (see Open
  questions #1).
- **P3 — DX (not yet):** skill capability-ladder integration, `mcp install`
  helper, registry submission.

### Implementation notes (branch `mcp`)

- Form factor delivered as the `codeup mcp` subcommand (`crates/codeup/src/mcp/`:
  `mod.rs` server loop, `protocol.rs` JSON-RPC, `sampling.rs` host-tokens client,
  `tools.rs` tool/prompt impls). No new crate; reuses `codeup-core` + the CLI
  analyzer/store.
- Bidirectional transport: a single writer task owns stdout; the read loop routes
  client requests to per-request handler tasks and routes responses to our
  server-initiated sampling requests back by id (oneshot map). A `tool_lock`
  serializes tool execution so concurrent calls can't race on the `.codeup/`
  store — it can't deadlock with sampling because sampling responses are handled
  in the read loop, which never takes the lock.
- No `rmcp`/SDK dependency: hand-rolled to keep the binary small and the
  dependency surface minimal (consistent with the rest of codeup). Only tokio
  features `io-std`/`sync`/`time` were added.

### DDD pass (branch `mcp`, follow-up to the initial implementation)

Reviewed against *Rediscovering Domain-Driven Design, one MCP server at a time*
(Bounded Context + Anti-Corruption Layer). Three improvements landed:

- **ACL / single orchestrator:** the per-file review use-case was extracted into
  `review::review_workspace` behind a `FileReviewer` port (a dyn-compatible trait,
  `BoxFuture`-based, no async-trait dep). The CLI supplies `ToolUseReviewer`
  (provider tool-use); the MCP server supplies `SamplingReviewer` (host sampling).
  The tool layer is now a thin adapter — domain orchestration (file selection,
  neighbour context, persistence, telemetry) lives in one place, used by both
  `codeup scan` and `codeup_review`. The use-case is unit-tested with a fake
  reviewer that never touches an LLM (the article's testability/replaceability win).
- **One aggregate constructor:** `analyzer::make_finding(file, content_hash, …)` is
  the single place a `Finding` is built (stable-id derivation + severity→priority +
  history). The MCP `build_finding` duplicate was deleted; sampling, save, and the
  CLI all go through it, so the invariant can't drift between ingress paths.
- **Bounded-context fence:** per-tool `root` arguments are LLM-supplied, so
  `resolve_root` now accepts only the server's launch workspace or a descendant
  (canonicalized) — closing an injection-driven path to read/scan/write outside
  the workspace the host scoped the server to.

Not done (deliberately): splitting the server into multiple bounded contexts (it is
already one coherent domain) or renaming tools.

- **Per-file sampling cache (deferred):** the CLI analyzer keys an on-disk cache
  by content hash; `codeup_review` does not yet reuse it, so a re-review
  re-samples unchanged files. PLAN §"How `codeup_review` consumes host tokens"
  step 5 calls for wiring this in to save the user's host tokens — a P3 follow-up.

## Open questions / risks

1. **Sampling support per host** — the whole host-tokens story rests on this.
   Status by host:
   - **Claude Code** — ❌ **verified absent** (v2.1.177, protocol 2025-11-25:
     advertises `roots` + `elicitation`, no `sampling`). `codeup_review` falls
     back to deterministic-only + the `codeup` delegation prompt. As designed.
   - **VS Code / Copilot** — ⏳ reported to support sampling, **not yet validated
     end-to-end** with codeup on a released build. This is the remaining
     must-verify before claiming the host-tokens review "works."
   - **Claude Desktop / Cursor** — ⏳ unverified; treat as VS Code until confirmed.
2. **No forced tool-use over sampling** → rely on JSON-in-text parsing; budget
   for occasional malformed output (retry/repair, then skip the file with a note).
3. **Token cost & rate limits** are the host's; large repos could be slow/costly
   — lean on the content-hash cache and `paths`/changed-files scoping by default.
4. **Findings attribution**: deterministic findings keep `detectedBy:
   codeup-deterministic` (confidence 1); sampled findings record the host model
   id when the host exposes it, else `detectedBy: codeup-mcp:host-sampling`.

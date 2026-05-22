# codeup-cli

Architectural anti-pattern scanner for the command line. Single static binary, primarily for GitHub Actions and other CI runners, but works locally too.

The same analysis core powers the [Codeup VS Code extension](https://github.com/johncoleman-thoughtworks/codeup-vscx). Findings live as YAML files under `.codeup/` so they travel with the repo and accumulate the team's decisions over time — whether produced by the editor or by CI. Both tools read and write to the contract documented in [`SCHEMA.md`](SCHEMA.md).

## What it does

Given a workspace root, codeup-cli walks the source, builds a dependency graph, and emits findings against a catalogue of architectural anti-patterns:

- **Deterministic checks** (always run, no LLM cost):
  - Import cycles via Tarjan SCC.
  - Layer violations against `.codeup/intent.yaml` rules.
  - Oversized files (size-based, gated to actual source languages).
- **LLM checks** (single-file tool-use against the catalogue):
  - Anemic domain models, primitive obsession, long methods, deep nesting, dead code, leaky abstractions, refused bequest, function-name mismatches, magic numbers, and ~30 more.
  - Per-file content-hash cache so unchanged files don't re-roundtrip.
  - Cross-file context: top-N graph neighbours are included in the prompt so findings about coupling actually see the coupled file.

Output goes to one of three formats: a human-readable text summary (default), SARIF 2.1.0 for GitHub Code Scanning, or raw findings JSON.

## Why a Rust CLI

- **Single static binary** — drop it on a runner and go. No `npm install`, no Node version dependency, no transitive npm dep tree.
- **Memory-safe analyzer** processing other people's source.
- **Fast** on large repos — Tarjan SCC, ignore-aware walking, regex-based import extraction all run substantially quicker than the Node equivalents.
- **Provider-agnostic LLM access** — Anthropic direct, or GitHub Models (the natural fit for Actions runners: `GITHUB_TOKEN` is auto-provided).

## Install

Build from source:

```bash
git clone https://github.com/johncoleman-thoughtworks/codeup-cli.git
cd codeup-cli
cargo build --release
./target/release/codeup --help
```

Pre-built binaries on the [releases page](https://github.com/johncoleman-thoughtworks/codeup-cli/releases) once we tag one.

## Quick start

```bash
# Scan the current directory, write SARIF, never fail the build
codeup scan . --out sarif --output codeup.sarif --fail-on none

# Print a human summary to stdout
codeup scan .

# Skip the LLM pass entirely — deterministic checks only
codeup scan . --deterministic-only
```

## Configuration

### Provider resolution

codeup auto-picks an LLM provider per scan:

| Setting | Result |
|---|---|
| `ANTHROPIC_API_KEY` is set | Anthropic Claude. |
| `ANTHROPIC_API_KEY` not set, `GITHUB_TOKEN` is set | GitHub Models (`models.github.ai`). |
| Neither | Error. Pass `--deterministic-only` to skip the LLM pass. |

Override the auto-pick explicitly with `--provider anthropic` or `--provider github-models`.

### Settings

All settings can be passed as flags or environment variables:

| Flag | Env var | Default | Notes |
|---|---|---|---|
| `--provider` | `CODEUP_PROVIDER` | `auto` | `auto`, `anthropic`, or `github-models`. |
| `--api-key` | `ANTHROPIC_API_KEY` or `GITHUB_TOKEN` | — | Highest precedence is the flag; env vars are read per-provider. |
| `--model` | `CODEUP_MODEL` | `claude-sonnet-4-6` (Anthropic) or `openai/gpt-4o-mini` (GH Models) | Must be a valid model id for the active provider. |
| `--out` | — | `text` | `text`, `sarif`, or `json`. |
| `--output` | — | stdout | File path. When set, writes the formatted report to file instead of stdout. |
| `--deterministic-only` | — | `false` | Skip the LLM pass; cycles + layer violations + oversized only. |
| `--max-cost` | — | `5.0` | Soft USD budget — the scan prompts before exceeding it on local runs. |
| `--fail-on` | — | `high` | Exit non-zero when any open finding ≥ this severity. `low`, `medium`, `high`, or `none`. |

### Anti-pattern catalogue

A baseline catalogue ships embedded in the binary (sourced from the VS Code extension's `resources/catalogue/default.yaml`). Projects can extend or override entries by adding `.codeup/knowledge/patterns.yaml` — see the extension's docs for the override format. The two implementations share one catalogue and one schema, so a finding from either looks identical on disk.

### Per-project state

Everything codeup-cli persists lives under `.codeup/`:

```
.codeup/
├── findings/<id>.yaml         # One file per finding. Commit these.
├── knowledge/
│   ├── dismissals.yaml        # Findings dismissed with a rationale.
│   ├── exemplars.yaml         # Findings confirmed and used as examples.
│   └── patterns.yaml          # Per-project catalogue overrides.
├── intent.yaml                # Layer rules for deterministic checks.
├── index/                     # Generated workspace index. Don't commit.
└── cache/                     # Per-entry analysis cache. Don't commit.
```

A reasonable starter `.gitignore`:

```
.codeup/index/
.codeup/cache/
```

## GitHub Actions

The CLI is designed for Actions first. The recommended setup uploads SARIF to GitHub Code Scanning so findings appear as inline PR annotations and under **Security → Code scanning alerts**.

### Minimal workflow

```yaml
# .github/workflows/codeup.yml
name: codeup

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read
  security-events: write   # for upload-sarif
  models: read             # for GITHUB_TOKEN → models.github.ai

jobs:
  scan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Build codeup
        run: cargo build --release --locked
        # Or download a pre-built binary from a release once we ship one.

      - name: Scan
        env:
          # If set, Anthropic is used. If not, codeup falls back to
          # GitHub Models via the auto-injected GITHUB_TOKEN.
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          ./target/release/codeup scan . \
            --out sarif \
            --output codeup.sarif \
            --fail-on none

      - name: Upload SARIF
        uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: codeup.sarif
          category: codeup
```

### Configuring the secret

To use Anthropic in CI, add the secret once:

```bash
gh secret set ANTHROPIC_API_KEY --repo OWNER/REPO
# paste your key when prompted
```

To remove it (and switch back to GitHub Models):

```bash
gh secret remove ANTHROPIC_API_KEY --repo OWNER/REPO
```

No secret is required for the GitHub Models path — `GITHUB_TOKEN` is auto-injected by Actions on every run.

### Permissions explained

```yaml
permissions:
  contents: read               # default — checkout
  security-events: write       # required by upload-sarif
  models: read                 # required for GITHUB_TOKEN to call GH Models
```

If you only ever use the Anthropic path you can drop `models: read`. If the repo is owned by an org, an org admin may also need to enable **Settings → Code, planning, and automation → Models** for the org.

### GitHub Models prerequisites

GitHub Models is free for individuals but the account needs to accept the marketplace preview terms once. Visit https://github.com/marketplace/models and open any model card to opt in. Verify with:

```bash
curl -sS -X POST https://models.github.ai/inference/chat/completions \
  -H "Authorization: Bearer $(gh auth token)" \
  -H "Content-Type: application/json" \
  -d '{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"ping"}]}'
```

A JSON response with `choices[]` means you're good. A 403 means the opt-in or the org gate is still pending.

Free-tier quotas vary by model — `openai/gpt-4o-mini` and the smaller models have generous request budgets; frontier models (gpt-4o, full Claude Sonnet) are tighter. See https://docs.github.com/en/github-models/use-github-models/prototyping-with-ai-models#rate-limits.

### Choosing a model for CI

The default `claude-sonnet-4-6` is excellent but burns through Tier-1 Anthropic rate limits (30k input tokens/min) on workspaces above ~10 files. For CI:

| Provider | Suggested model | Why |
|---|---|---|
| Anthropic | `claude-haiku-4-5` | Higher per-tier caps, ~3× cheaper, easily handles the analyzer's single-file tool-use task. |
| GitHub Models | `openai/gpt-4o-mini` (default) | Free-tier friendly, reliable tool-use. |

Set via env:

```yaml
- name: Scan
  env:
    ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
    CODEUP_MODEL: claude-haiku-4-5
  run: ./target/release/codeup scan . --out sarif --output codeup.sarif --fail-on none
```

### Resilience

Every LLM call retries up to 5 times on:
- **429 Too Many Requests** — sleeps the standard `Retry-After` seconds, or exponential backoff (2s, 4s, 8s, 16s) if no header.
- **5xx server errors** — including Anthropic's 529 `overloaded_error`. Exponential backoff, capped at 60s.

Other 4xx (auth, bad model, bad schema) fail immediately — retrying them just burns time.

## Output formats

### SARIF 2.1.0

Validates clean against the official OASIS schema and Microsoft's `Sarif.Multitool`. One `tool.driver` entry, one rule per category that fired with the strongest observed severity as `defaultConfiguration.level`, one result per non-suppressed finding. Stable `partialFingerprints["codeupId/v1"]` so GitHub Code Scanning dedupes across runs even when line numbers drift.

Severity mapping:

| Codeup | SARIF |
|---|---|
| `high` | `error` |
| `medium` | `warning` |
| `low` | `note` |

### Text

Human summary written to stdout (or `--output codeup.txt`):

```
# Codeup scan summary

Root           : /workspace
Files indexed  : 30
Graph edges    : 23
Cycles         : 0
Layer violations: 0
Oversized files: 0
LLM scanned    : 12
LLM cached     : 0
Total findings : 40

## high (17)
  - anemic-domain-model  src/main/java/.../Cart.java:18
  ...
```

### JSON

Raw findings as a JSON array, suitable for piping into `jq` or a downstream tool that doesn't speak SARIF.

## Layout

```
codeup-cli/
├── Cargo.toml                 # workspace
├── crates/
│   ├── codeup-core/           # pure analysis library — schema, scanner,
│   │                          # graph, intent, knowledge, catalogue
│   └── codeup/                # CLI binary — clap entry, reporters,
│                              # HTTP clients (Anthropic + GitHub Models),
│                              # shared retry policy
└── README.md
```

The split exists so the analysis core can be embedded in other consumers later (MCP server, hosted service, etc.) without dragging the CLI's clap / reqwest deps along.

## License

MIT — see [LICENSE](LICENSE).

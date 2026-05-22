# codeup-cli

Architectural anti-pattern scanner for the command line. Single static binary, primarily for GitHub Actions and other CI runners, but works locally too.

The same analysis core powers the [Codeup VS Code extension](https://github.com/johncoleman-thoughtworks/codeup-vscx). Findings live as YAML files under `.codeup/` so they travel with the repo and accumulate the team's decisions over time — whether produced by the editor or by CI.

## Status

Bones. `cargo build` succeeds; CLI parses `--help` and `--version`. Subcommands are scaffolded but return "not implemented" pending the port from the TypeScript reference implementation.

See [PLAN.md](PLAN.md) for the port roadmap.

## Why a Rust CLI

- **Single static binary** — `curl -L .../codeup-linux-x64 -o codeup && chmod +x codeup` and you're done. No `npm install`, no Node version dependency, no transitive npm dep tree.
- **Memory-safe analyzer** processing other people's source.
- **Fast** on large repos — Tarjan SCC, ignore-aware walking, regex-based import extraction all run substantially quicker than the Node equivalents.
- **Provider-agnostic LLM access**: Anthropic direct (`--api-key $ANTHROPIC_API_KEY`) or GitHub Models (`--api-key $GITHUB_TOKEN --provider github-models`). The latter is the natural fit for GitHub Actions in a Copilot-licensed org — `GITHUB_TOKEN` is auto-provided to every runner.

## Build

```bash
cargo build --release
./target/release/codeup --help
```

## Layout

```
codeup-cli/
├── Cargo.toml                 # workspace
├── crates/
│   ├── codeup-core/           # pure analysis library — schema, scanner,
│   │                          # graph, intent, knowledge, catalogue
│   └── codeup/                # CLI binary — clap entry, reporters,
│                              # HTTP clients (Anthropic + GitHub Models)
└── README.md
```

The split exists so the analysis core can be embedded in other consumers later (MCP server, hosted service, etc.) without dragging the CLI's clap / reqwest deps along.

## License

MIT — see [LICENSE](LICENSE).

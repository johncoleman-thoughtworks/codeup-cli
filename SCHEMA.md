# Codeup YAML Schema — canonical reference

The Rust CLI at this repo and the TS extension at
[codeup-vscx](https://github.com/johncoleman-thoughtworks/codeup-vscx)
both read and write the same `.codeup/` files. This document is the
contract between them. Any other tool (MCP server, alternative CLI,
hand-edited file) MUST emit per-spec or its files will be rejected.

## Locations

| Path | Contents |
|---|---|
| `.codeup/findings/<id>.yaml` | One file per finding. |
| `.codeup/knowledge/dismissals.yaml` | All dismissal entries in one file. |
| `.codeup/knowledge/exemplars.yaml` | All confirmed exemplars in one file. |
| `.codeup/knowledge/patterns.yaml` | Team-specific catalogue overrides. |
| `.codeup/intent.yaml` | Layer rules for deterministic layer-violation checks. |
| `.codeup/index/index.json` | Generated workspace index (regenerable; do not commit). |
| `.codeup/index/graph.json` | Generated dependency graph (regenerable; do not commit). |
| `.codeup/cache/entries/<hash>.json` | Per-entry analysis cache (regenerable; do not commit). |

## Encoding

- **File encoding**: UTF-8, LF line endings.
- **YAML version**: 1.2.
- **Field names**: camelCase (`schemaVersion`, `detectedAt`, etc.).
- **`schemaVersion`**: integer `1` (current). Migrations land in
  `crates/codeup-core/src/migrations.rs` (CLI) /
  `src/migrations/runner.ts` (extension) when this bumps.

## Strings

All non-numeric, non-boolean values are **strings**. Several string values
look superficially like other YAML types (timestamps, version numbers).
These MUST be emitted in a form that round-trips as a string in any
YAML 1.1 or 1.2 reader.

### Timestamp fields

`detectedAt`, `dismissedAt`, `confirmedAt`, `history[].timestamp` —
all timestamps.

- **Format**: ISO 8601 UTC with millisecond precision, e.g.
  `2026-05-22T14:32:11.123Z`.
- **Required wire form**: **quoted YAML scalar** (`"2026-..."`),
  not the plain (unquoted) form.

  Plain unquoted ISO strings are interpreted as `!!timestamp` by
  YAML 1.1 parsers (including `js-yaml`'s default schema), which
  returns a JavaScript `Date` object rather than a string. The
  extension's validator then rejects them as `must be a non-empty
  string`. Always quote.

- **Examples**:

  Good:
  ```yaml
  detectedAt: "2026-05-22T14:32:11.123Z"
  history:
    - timestamp: "2026-05-22T14:32:11.123Z"
      event: detected
  ```

  Bad (rejected by extension):
  ```yaml
  detectedAt: 2026-05-22T14:32:11Z       # unquoted → Date object
  detectedAt: '2026-05-22 14:32:11'      # space separator → Date object
  ```

### Enum string fields

`severity` ∈ `low | medium | high`. `status` ∈ `unconfirmed | confirmed | dismissed | fixed`. `priority` ∈ `ignore | low | medium | high`. All lowercase. Plain (unquoted) is fine for these because they don't look like other types.

## Finding shape

```yaml
schemaVersion: 1
id: <category>-<sha-prefix>            # stable across runs
category: <pattern-id>                 # must be in catalogue
severity: low | medium | high
status: unconfirmed | confirmed | dismissed | fixed
priority: ignore | low | medium | high
location:
  file: <workspace-relative-path>      # required
  line: <int>                          # optional, 1-based
  endLine: <int>                       # optional, 1-based inclusive
  contentHash: <sha256-hex>            # optional, of the file at detection time
explanation: <text>
suggestedRemediation: <text>           # optional
detectedAt: "<ISO 8601>"               # quoted
detectedBy: <tool-or-model-id>
confidence: <float 0..1>               # optional
history:
  - timestamp: "<ISO 8601>"            # quoted
    event: detected | status_changed | priority_changed | note | rebound
    by: <actor>                        # optional
    from: <prev-value>                 # optional, for status/priority changes
    to: <new-value>                    # optional
    note: <text>                       # optional
```

## Implementation notes

- **Rust CLI** uses `serde_yaml` with a post-serialization pass in
  `crates/codeup/src/store.rs` that quotes timestamp values
  (`detectedAt` / `dismissedAt` / `confirmedAt` / `timestamp`) before
  writing to disk. serde_yaml doesn't expose a per-field "force
  quoted" hint, hence the post-processing.
- **TS extension** uses `js-yaml` v4 with `DEFAULT_SCHEMA` (which
  includes `!!timestamp`). It dumps timestamps as quoted strings via
  `yaml.dump`'s default behaviour for strings containing `:`.
- Any new implementation MUST: (1) round-trip read its own output and
  the other side's output, (2) verify against a fixture YAML file
  containing every field type, including timestamps in quoted form.

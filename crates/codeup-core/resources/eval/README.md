# Catalogue eval harness

Labelled findings from running codeup on its own source, used as a
regression set when tuning catalogue patterns. Without it, every
"reduce false positives" change is theory; with it, you can measure
whether a wording change moves precision up or down.

## The labelled set

`self-scan.jsonl` — one row per finding from a baseline scan of
`crates/` with `--model claude-haiku-4-5`. Each row:

```json
{
  "file":     "crates-relative path",
  "line":     42,
  "category": "pattern id from catalogue",
  "severity": "as reported by the scan",
  "label":    "true_positive | overreach | fabrication | needs_review",
  "reason":   "one-line explanation"
}
```

Label meanings:

- **`true_positive`** — the finding describes a real issue worth
  acting on, or a real cosmetic concern (magic-number, dead-code,
  long-method). Even if low value, the catalogue identified the
  thing it was supposed to identify.
- **`overreach`** — the pattern matched signals but the underlying
  code is legitimate plumbing. Tuning the catalogue should make
  these stop firing. Examples: bare-`String` storage of an API key
  in a single-destination client, primitive-obsession on schema
  enums that ARE the wire contract.
- **`fabrication`** — the model hallucinated code that does not
  exist at the cited location. Different from overreach: a tuned
  catalogue won't help; this is a model-quality issue (likely Haiku
  vs. Sonnet). Tracking these separately tells us whether the noise
  source is the catalogue or the model.
- **`needs_review`** — author wasn't confident enough to label;
  someone with deeper context on that file should verify.

## How to run the eval

```sh
scripts/eval-catalogue.sh
```

The script:

1. Wipes `.codeup/cache/` so cached judgements don't pollute the run.
2. Runs `codeup scan crates --model claude-haiku-4-5 --out json` with
   the API key from the environment.
3. Joins the new findings against `self-scan.jsonl` on
   `(file, category)` — line numbers drift across refactors, so we
   key on (file, category) and accept any-line within the same
   (file, category) pair as a match.
4. Reports four counters:
   - **agreed_TP** — labelled `true_positive` and still fires. Good.
   - **agreed_FP** — labelled `overreach` or `fabrication` and still
     fires. **This is the metric to drive down.**
   - **new_finding** — fires now but not in the labelled set. Needs
     a label. Check whether your catalogue change introduced it.
   - **missed_TP** — labelled `true_positive` but does not fire.
     **This is the metric to keep at zero.** Catalogue tuning must
     not silence real findings.

Exit code is non-zero if `missed_TP > 0`.

## Updating the labelled set

When the codebase changes in a way that legitimately changes the
finding landscape (e.g. a fix that resolves a real issue), update
the JSONL:

- A `true_positive` was fixed → remove the row. `missed_TP` will
  catch the regression if the fix is reverted.
- A new `overreach` appears → add a row with that label. Without
  it, the new finding will show as `new_finding` forever.
- A `needs_review` finding has been examined → change its label and
  tighten the `reason`.

Keep `self-scan.jsonl` byte-stable apart from intentional updates.
Reviewers should be able to read diffs to it directly.

## Baseline (as of v0.2.1)

```
true_positive  27
overreach      21
needs_review   11
fabrication     2
total          61
```

Baseline source: `claude-haiku-4-5` against `crates/` at commit
of v0.2.1. See [/tmp/codeup-self.json](/) for the raw scan output
on the run that produced these labels (not committed; reproduce
locally).

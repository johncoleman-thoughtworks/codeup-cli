#!/usr/bin/env bash
# Regression-test catalogue tuning against the labelled self-scan set.
#
# Runs codeup against `crates/` with a pinned model + clean cache, joins
# the new findings against resources/eval/self-scan.jsonl on
# (file, category), and reports:
#   agreed_TP      labelled true_positive and still fires        (good)
#   agreed_FP      labelled overreach|fabrication and still fires (drive down)
#   new_finding    fires now but unlabelled                       (triage)
#   missed_TP      labelled true_positive but no longer fires     (regression)
#
# Exits non-zero if missed_TP > 0, so this can gate catalogue PRs in CI.
#
# Requirements: ANTHROPIC_API_KEY in env, `codeup` binary on PATH or
# built at target/release/codeup, jq.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

LABELS="crates/codeup-core/resources/eval/self-scan.jsonl"
MODEL="${CODEUP_EVAL_MODEL:-claude-haiku-4-5}"
SCAN_OUT="${CODEUP_EVAL_OUT:-/tmp/codeup-eval.json}"

[ -f "$LABELS" ] || { echo "missing $LABELS" >&2; exit 2; }
command -v jq >/dev/null     || { echo "need jq on PATH" >&2; exit 2; }
[ -n "${ANTHROPIC_API_KEY:-}" ] || { echo "need ANTHROPIC_API_KEY in env" >&2; exit 2; }

if command -v codeup >/dev/null 2>&1; then
  CODEUP=codeup
elif [ -x "./target/release/codeup" ]; then
  CODEUP="./target/release/codeup"
else
  echo "build codeup first: cargo build --release --bin codeup" >&2
  exit 2
fi

echo "==> Wiping .codeup/cache/ so cached judgements don't pollute the run"
rm -rf .codeup/cache

echo "==> Running $CODEUP scan crates --model $MODEL"
"$CODEUP" scan crates \
  --model "$MODEL" \
  --out json \
  --output "$SCAN_OUT" \
  --fail-on none >/dev/null

# Normalise findings to the same shape as the labelled set, keyed on
# (file, category). Line numbers drift across refactors so we match on
# the pair and accept any line as the same finding.
NEW_KEYS=$(jq -r '.[] | "\(.location.file)\t\(.category)"' "$SCAN_OUT" | sort -u)
LABEL_KEYS_TP=$(jq -r 'select(.label=="true_positive") | "\(.file)\t\(.category)"' "$LABELS" | sort -u)
LABEL_KEYS_FP=$(jq -r 'select(.label=="overreach" or .label=="fabrication") | "\(.file)\t\(.category)"' "$LABELS" | sort -u)
LABEL_KEYS_ALL=$(jq -r '"\(.file)\t\(.category)"' "$LABELS" | sort -u)

agreed_tp=$(comm -12 <(printf '%s\n' "$NEW_KEYS") <(printf '%s\n' "$LABEL_KEYS_TP") | wc -l | tr -d ' ')
agreed_fp=$(comm -12 <(printf '%s\n' "$NEW_KEYS") <(printf '%s\n' "$LABEL_KEYS_FP") | wc -l | tr -d ' ')
new_finding=$(comm -23 <(printf '%s\n' "$NEW_KEYS") <(printf '%s\n' "$LABEL_KEYS_ALL") | wc -l | tr -d ' ')
missed_tp=$(comm -13 <(printf '%s\n' "$NEW_KEYS") <(printf '%s\n' "$LABEL_KEYS_TP") | wc -l | tr -d ' ')

total_labelled=$(printf '%s\n' "$LABEL_KEYS_ALL" | wc -l | tr -d ' ')
total_now=$(printf '%s\n' "$NEW_KEYS" | wc -l | tr -d ' ')

cat <<EOF

Catalogue eval — model=$MODEL, scan=crates/
-------------------------------------------
Labelled findings:  $total_labelled
Current findings:   $total_now

agreed_TP:          $agreed_tp   (good — real findings still surface)
agreed_FP:          $agreed_fp   (drive this down with catalogue tuning)
new_finding:        $new_finding   (unlabelled — triage and add to self-scan.jsonl)
missed_TP:          $missed_tp   (regression — labelled TP no longer fires)

EOF

if [ "$new_finding" -gt 0 ]; then
  echo "New (unlabelled) findings:"
  comm -23 <(printf '%s\n' "$NEW_KEYS") <(printf '%s\n' "$LABEL_KEYS_ALL") \
    | awk -F'\t' '{ printf "  %-50s  %s\n", $1, $2 }'
  echo
fi

if [ "$missed_tp" -gt 0 ]; then
  echo "Missed true positives (catalogue change silenced these):"
  comm -13 <(printf '%s\n' "$NEW_KEYS") <(printf '%s\n' "$LABEL_KEYS_TP") \
    | awk -F'\t' '{ printf "  %-50s  %s\n", $1, $2 }'
  echo
  exit 1
fi
